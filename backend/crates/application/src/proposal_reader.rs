//! Isolated, bounded proposal cache. These snapshots never establish main
//! provenance and cannot seed the approved-book index or its cache.
use crate::{
    book::{BookError, BookService, BookSnapshot, ReadAuthorization},
    reader::BookProviderFactory,
};
use async_trait::async_trait;
use specspace_document_engine::media::{self, DisplayImage};
use specspace_domain::{BranchName, RepositoryPath};
use specspace_git_adapter::{CommitSha, GitError};
use std::{collections::VecDeque, sync::Arc};
use tokio::sync::{Mutex, Semaphore};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposalReadScope {
    pub repository: Uuid,
    pub branch: BranchName,
    pub head: CommitSha,
}

#[async_trait]
pub trait ProposalReadAuthorization: Send + Sync {
    /// Resolve current scope and recorded refs, never browser-supplied refs.
    async fn resolve(&self, actor: Uuid, proposal: Uuid) -> Result<ProposalReadScope, BookError>;
}

struct CachedProposal {
    proposal: Uuid,
    scope: ProposalReadScope,
    book: Arc<BookSnapshot>,
}

pub struct ProposalBook {
    pub scope: ProposalReadScope,
    pub book: Arc<BookSnapshot>,
}

pub struct ProposalReader {
    authorization: Arc<dyn ProposalReadAuthorization>,
    repositories: Arc<dyn ReadAuthorization>,
    source: Arc<dyn BookProviderFactory>,
    cache: Mutex<VecDeque<CachedProposal>>,
    loads: Semaphore,
    assets: Arc<Semaphore>,
}

impl ProposalReader {
    pub fn new(
        authorization: Arc<dyn ProposalReadAuthorization>,
        repositories: Arc<dyn ReadAuthorization>,
        source: Arc<dyn BookProviderFactory>,
    ) -> Self {
        Self {
            authorization,
            repositories,
            source,
            cache: Mutex::new(VecDeque::new()),
            loads: Semaphore::new(2),
            assets: Arc::new(Semaphore::new(2)),
        }
    }

    /// Recheck after provider/CPU awaits and cache hits. A changed anchor cannot
    /// authorize content proved against a different recorded proposal head.
    pub async fn recheck(
        &self,
        actor: Uuid,
        proposal: Uuid,
        scope: &ProposalReadScope,
    ) -> Result<(), BookError> {
        if self.authorization.resolve(actor, proposal).await? == *scope {
            Ok(())
        } else {
            Err(BookError::NotFound)
        }
    }

    pub async fn at(
        &self,
        actor: Uuid,
        proposal: Uuid,
        selected: Option<CommitSha>,
    ) -> Result<ProposalBook, BookError> {
        let scope = self.authorization.resolve(actor, proposal).await?;
        let commit = selected.unwrap_or_else(|| scope.head.clone());
        let _permit = self
            .loads
            .acquire()
            .await
            .map_err(|_| BookError::ProcessingUnavailable)?;
        self.recheck(actor, proposal, &scope).await?;
        let cached = {
            let mut cache = self.cache.lock().await;
            let index = cache.iter().position(|entry| {
                entry.proposal == proposal && entry.scope == scope && entry.book.commit == commit
            });
            index.and_then(|index| cache.remove(index)).map(|entry| {
                let book = entry.book.clone();
                cache.push_back(entry);
                book
            })
        };
        let book = match cached {
            Some(book) => book,
            None => {
                let provider = self.source.provider(scope.repository).await?;
                let service =
                    BookService::new(scope.repository, provider, self.repositories.clone());
                let book = Arc::new(service.load_proposal_at(actor, commit, &scope.head).await?);
                self.recheck(actor, proposal, &scope).await?;
                let mut cache = self.cache.lock().await;
                cache.retain(|entry| {
                    !(entry.proposal == proposal && entry.book.commit == book.commit)
                });
                cache.push_back(CachedProposal {
                    proposal,
                    scope: scope.clone(),
                    book: book.clone(),
                });
                while cache.len() > 4 {
                    cache.pop_front();
                }
                book
            }
        };
        self.recheck(actor, proposal, &scope).await?;
        Ok(ProposalBook { scope, book })
    }

    pub async fn asset(
        &self,
        actor: Uuid,
        proposal: Uuid,
        commit: CommitSha,
        path: String,
    ) -> Result<DisplayImage, BookError> {
        let snapshot = self.at(actor, proposal, Some(commit.clone())).await?;
        if !media::valid_path(&path) {
            return Err(BookError::InvalidQuery("INVALID_ASSET_PATH"));
        }
        let permit = self
            .assets
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| BookError::ProcessingUnavailable)?;
        self.recheck(actor, proposal, &snapshot.scope).await?;
        let provider = self.source.provider(snapshot.scope.repository).await?;
        let path_ref = RepositoryPath::parse(&path)
            .map_err(|_| BookError::InvalidQuery("INVALID_ASSET_PATH"))?;
        let bytes = provider
            .read_bytes(&commit, &path_ref, media::MAX_ASSET_BYTES)
            .await
            .map_err(|error| match error {
                GitError::FileMissing => BookError::NotFound,
                GitError::ResponseTooLarge => BookError::InvalidAsset("FILE_TOO_LARGE"),
                error => BookError::Provider(error),
            })?;
        let image = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            media::display(&path, &bytes).map_err(BookError::InvalidAsset)
        })
        .await
        .map_err(|_| BookError::ProcessingUnavailable)??;
        self.recheck(actor, proposal, &snapshot.scope).await?;
        Ok(image)
    }
}
