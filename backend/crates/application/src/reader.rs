//! Bounded derived cache: every entry came from an authorized main-history read.
//! Git remains authoritative. This cache can be discarded and rebuilt at any time.
use crate::book::{BookError, BookService, BookSnapshot, ReadAuthorization};
use async_trait::async_trait;
use specspace_document_engine::find::MatchPage;
use specspace_document_engine::media::{self, DisplayImage};
use specspace_domain::RepositoryPath;
use specspace_git_adapter::{CommitSha, GitProvider};
use std::{collections::VecDeque, sync::Arc};
use tokio::sync::{Mutex, Semaphore};
use uuid::Uuid;

#[async_trait]
pub trait BookProviderFactory: Send + Sync {
    async fn provider(&self, repository: Uuid) -> Result<Arc<dyn GitProvider>, BookError>;
}

/// Trusted persistence of a completed main-history read. This proof never
/// replaces the reader's current user/repository authorization checks.
#[async_trait]
pub trait MainHistoryProof: Send + Sync {
    async fn contains(&self, repository: Uuid, commit: &CommitSha) -> Result<bool, BookError>;
}

struct CachedBook {
    repository: Uuid,
    book: Arc<BookSnapshot>,
}

pub struct BookReader {
    authorization: Arc<dyn ReadAuthorization>,
    source: Arc<dyn BookProviderFactory>,
    history: Option<Arc<dyn MainHistoryProof>>,
    cache: Mutex<VecDeque<CachedBook>>,
    loads: Semaphore,
    computations: Arc<Semaphore>,
    assets: Arc<Semaphore>,
}

impl BookReader {
    pub fn new(
        authorization: Arc<dyn ReadAuthorization>,
        source: Arc<dyn BookProviderFactory>,
    ) -> Self {
        Self {
            authorization,
            source,
            history: None,
            cache: Mutex::new(VecDeque::new()),
            loads: Semaphore::new(2),
            computations: Arc::new(Semaphore::new(2)),
            assets: Arc::new(Semaphore::new(2)),
        }
    }

    pub fn with_history(mut self, history: Arc<dyn MainHistoryProof>) -> Self {
        self.history = Some(history);
        self
    }

    pub async fn authorize(&self, actor: Uuid, repository: Uuid) -> Result<(), BookError> {
        if self.authorization.can_read(actor, repository).await? {
            Ok(())
        } else {
            Err(BookError::NotFound)
        }
    }

    async fn cached(&self, repository: Uuid, commit: &CommitSha) -> Option<Arc<BookSnapshot>> {
        let mut cache = self.cache.lock().await;
        let index = cache
            .iter()
            .position(|entry| entry.repository == repository && entry.book.commit == *commit)?;
        let entry = cache.remove(index)?;
        let book = entry.book.clone();
        cache.push_back(entry);
        Some(book)
    }

    /// Trusted indexing code checks repository/organization scope before using
    /// this cache. Entries were loaded through the main-history reader only.
    pub async fn cached_for_index(
        &self,
        repository: Uuid,
        commit: &CommitSha,
    ) -> Option<Arc<BookSnapshot>> {
        self.cached(repository, commit).await
    }

    /// Seed reader data only after an indexing source proved main provenance.
    /// Each subsequent browser read still performs its own current ACL checks.
    pub async fn remember_indexed_main(&self, repository: Uuid, book: Arc<BookSnapshot>) {
        let mut cache = self.cache.lock().await;
        cache.retain(|entry| entry.repository != repository || entry.book.commit != book.commit);
        cache.push_back(CachedBook { repository, book });
        while cache.len() > 4 {
            cache.pop_front();
        }
    }

    pub async fn main(
        &self,
        actor: Uuid,
        repository: Uuid,
    ) -> Result<Arc<BookSnapshot>, BookError> {
        self.authorize(actor, repository).await?;
        let provider = self.source.provider(repository).await?;
        let commit = provider.main_head().await.map_err(BookError::Provider)?;
        self.load(actor, repository, commit, provider).await
    }

    pub async fn at(
        &self,
        actor: Uuid,
        repository: Uuid,
        commit: CommitSha,
    ) -> Result<Arc<BookSnapshot>, BookError> {
        self.authorize(actor, repository).await?;
        if let Some(book) = self.cached(repository, &commit).await {
            self.authorize(actor, repository).await?;
            return Ok(book);
        }
        let provider = self.source.provider(repository).await?;
        self.load(actor, repository, commit, provider).await
    }

    async fn load(
        &self,
        actor: Uuid,
        repository: Uuid,
        commit: CommitSha,
        provider: Arc<dyn GitProvider>,
    ) -> Result<Arc<BookSnapshot>, BookError> {
        let _permit = self
            .loads
            .acquire()
            .await
            .map_err(|_| BookError::ProcessingUnavailable)?;
        // Recheck after waiting: another caller may already have built this SHA.
        let book = if let Some(book) = self.cached(repository, &commit).await {
            book
        } else {
            let recorded = match &self.history {
                Some(history) => history.contains(repository, &commit).await?,
                None => false,
            };
            let service = BookService::new(repository, provider, self.authorization.clone());
            let book = Arc::new(if recorded {
                service.load_recorded_main(actor, commit).await?
            } else {
                service.load_at(actor, commit).await?
            });
            let mut cache = self.cache.lock().await;
            if !cache
                .iter()
                .any(|entry| entry.repository == repository && entry.book.commit == book.commit)
            {
                cache.push_back(CachedBook {
                    repository,
                    book: book.clone(),
                });
                while cache.len() > 4 {
                    cache.pop_front();
                }
            }
            book
        };
        self.authorize(actor, repository).await?;
        Ok(book)
    }

    pub async fn find(
        &self,
        actor: Uuid,
        repository: Uuid,
        commit: CommitSha,
        query: String,
        offset: usize,
        limit: usize,
    ) -> Result<MatchPage, BookError> {
        let book = self.at(actor, repository, commit).await?;
        let permit = self
            .computations
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| BookError::ProcessingUnavailable)?;
        let page = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            book.find(&query, offset, limit)
        })
        .await
        .map_err(|_| BookError::ProcessingUnavailable)??;
        self.authorize(actor, repository).await?;
        Ok(page)
    }

    pub async fn highlights(
        &self,
        actor: Uuid,
        repository: Uuid,
        commit: CommitSha,
        document: String,
        query: String,
        windows: Vec<specspace_document_engine::find::MatchWindow>,
    ) -> Result<Vec<specspace_document_engine::find::Highlight>, BookError> {
        let book = self.at(actor, repository, commit).await?;
        let permit = self
            .computations
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| BookError::ProcessingUnavailable)?;
        let highlights = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            book.highlights(&document, &query, &windows)
        })
        .await
        .map_err(|_| BookError::ProcessingUnavailable)??;
        self.authorize(actor, repository).await?;
        Ok(highlights)
    }

    pub async fn asset(
        &self,
        actor: Uuid,
        repository: Uuid,
        commit: CommitSha,
        path: String,
    ) -> Result<DisplayImage, BookError> {
        self.authorize(actor, repository).await?;
        if !media::valid_path(&path) {
            return Err(BookError::InvalidQuery("INVALID_ASSET_PATH"));
        }
        // Proves the selected SHA belongs to authorized main history and rejects
        // unsupported tree entries, exactly as document/search reads do.
        self.at(actor, repository, commit.clone()).await?;
        let permit = self
            .assets
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| BookError::ProcessingUnavailable)?;
        self.authorize(actor, repository).await?;
        let provider = self.source.provider(repository).await?;
        let repository_path = RepositoryPath::parse(&path)
            .map_err(|_| BookError::InvalidQuery("INVALID_ASSET_PATH"))?;
        let bytes = provider
            .read_bytes(&commit, &repository_path, media::MAX_ASSET_BYTES)
            .await
            .map_err(|error| match error {
                specspace_git_adapter::GitError::FileMissing => BookError::NotFound,
                specspace_git_adapter::GitError::ResponseTooLarge => {
                    BookError::InvalidAsset("FILE_TOO_LARGE")
                }
                error => BookError::Provider(error),
            })?;
        let image = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            media::display(&path, &bytes).map_err(BookError::InvalidAsset)
        })
        .await
        .map_err(|_| BookError::ProcessingUnavailable)??;
        self.authorize(actor, repository).await?;
        Ok(image)
    }
}
