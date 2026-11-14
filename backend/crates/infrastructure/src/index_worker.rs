//! Durable, read-only Git indexing. Crashed work is reclaimed through its lease;
//! no repository mutation is retried by this worker.
use crate::search_index::{IndexError, IndexLease, PgSearchIndex};
use async_trait::async_trait;
use specspace_application::{
    book::{BookError, BookService, BookSnapshot, ReadAuthorization},
    reader::{BookProviderFactory, BookReader},
    search::IndexSnapshot,
};
use specspace_git_adapter::{CommitSha, GitError};
use sqlx::PgPool;
use std::{sync::Arc, time::Duration};
use tokio::sync::watch;
use uuid::Uuid;

#[async_trait]
pub trait IndexSource: Send + Sync {
    async fn load(
        &self,
        organization: Uuid,
        repository: Uuid,
        commit: CommitSha,
    ) -> Result<Arc<BookSnapshot>, BookError>;
}

pub struct GitIndexSource {
    pool: PgPool,
    factory: Arc<dyn BookProviderFactory>,
    reader: Option<Arc<BookReader>>,
}
impl GitIndexSource {
    pub fn new(
        pool: PgPool,
        factory: Arc<dyn BookProviderFactory>,
        reader: Option<Arc<BookReader>>,
    ) -> Self {
        Self {
            pool,
            factory,
            reader,
        }
    }
}

struct IndexAuthorization {
    pool: PgPool,
    organization: Uuid,
    repository: Uuid,
}
#[async_trait]
impl ReadAuthorization for IndexAuthorization {
    async fn can_read(&self, actor: Uuid, repository: Uuid) -> Result<bool, BookError> {
        if actor != Uuid::nil() || repository != self.repository {
            return Ok(false);
        }
        sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM repositories WHERE id=$1 AND organization_id=$2)",
        )
        .bind(repository)
        .bind(self.organization)
        .fetch_one(&self.pool)
        .await
        .map_err(|_| BookError::AuthorizationUnavailable)
    }
}

#[async_trait]
impl IndexSource for GitIndexSource {
    async fn load(
        &self,
        organization: Uuid,
        repository: Uuid,
        commit: CommitSha,
    ) -> Result<Arc<BookSnapshot>, BookError> {
        let authorization = Arc::new(IndexAuthorization {
            pool: self.pool.clone(),
            organization,
            repository,
        });
        if !authorization.can_read(Uuid::nil(), repository).await? {
            return Err(BookError::NotFound);
        }
        if let Some(reader) = &self.reader
            && let Some(book) = reader.cached_for_index(repository, &commit).await
        {
            if !authorization.can_read(Uuid::nil(), repository).await? {
                return Err(BookError::NotFound);
            }
            return Ok(book);
        }
        let provider = self.factory.provider(repository).await?;
        let book = Arc::new(
            BookService::new(repository, provider, authorization)
                .load_at(Uuid::nil(), commit)
                .await?,
        );
        if let Some(reader) = &self.reader {
            reader.remember_indexed_main(repository, book.clone()).await;
        }
        Ok(book)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum WorkResult {
    Idle,
    Completed(Uuid),
    Deferred(Uuid),
    Failed(Uuid),
    LeaseLost(Uuid),
}

pub struct IndexWorker {
    store: Arc<PgSearchIndex>,
    source: Arc<dyn IndexSource>,
    worker: Uuid,
    timeout: Duration,
}
impl IndexWorker {
    pub fn new(
        store: Arc<PgSearchIndex>,
        source: Arc<dyn IndexSource>,
        timeout: Duration,
    ) -> Result<Self, IndexError> {
        if !(30..=3600).contains(&timeout.as_secs()) {
            return Err(IndexError::InvalidQuery("INVALID_INDEX_TIMEOUT"));
        }
        Ok(Self {
            store,
            source,
            worker: Uuid::new_v4(),
            timeout,
        })
    }

    pub async fn run_once(&self) -> Result<WorkResult, IndexError> {
        let Some(lease) = self.store.claim(self.worker, 60).await? else {
            return Ok(WorkResult::Idle);
        };
        let work = async {
            let commit =
                CommitSha::parse(&lease.payload.snapshot_sha).map_err(BookError::Provider)?;
            let book = self
                .source
                .load(lease.organization_id, lease.payload.repository_id, commit)
                .await?;
            tokio::task::spawn_blocking(move || IndexSnapshot::build(&book))
                .await
                .map_err(|_| BookError::ProcessingUnavailable)?
        };
        tokio::pin!(work);
        let deadline = tokio::time::sleep(self.timeout);
        tokio::pin!(deadline);
        let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        heartbeat.tick().await;
        let prepared = loop {
            tokio::select! {
                result = &mut work => break result,
                _ = heartbeat.tick() => {
                    match self.store.heartbeat(&lease, 60).await {
                        Ok(()) => {}, Err(IndexError::LeaseLost) => return Ok(WorkResult::LeaseLost(lease.id)), Err(error) => return Err(error),
                    }
                },
                _ = &mut deadline => return self.failure(&lease, "INDEX_TIMEOUT", Some(backoff(lease.attempts))).await,
            }
        };
        match prepared {
            Ok(snapshot) => match self.store.publish(&lease, &snapshot).await {
                Ok(()) => Ok(WorkResult::Completed(lease.id)),
                Err(IndexError::LeaseLost) => Ok(WorkResult::LeaseLost(lease.id)),
                Err(IndexError::InvalidSnapshot) => {
                    self.failure(&lease, "INVALID_SEARCH_INDEX", None).await
                }
                Err(error) => Err(error),
            },
            Err(error) => {
                let (code, delay) = classify(error, lease.attempts);
                self.failure(&lease, code, delay).await
            }
        }
    }

    async fn failure(
        &self,
        lease: &IndexLease,
        code: &'static str,
        delay: Option<u32>,
    ) -> Result<WorkResult, IndexError> {
        match self.store.fail(lease, code, delay).await {
            Ok(true) => Ok(WorkResult::Deferred(lease.id)),
            Ok(false) => Ok(WorkResult::Failed(lease.id)),
            Err(IndexError::LeaseLost) => Ok(WorkResult::LeaseLost(lease.id)),
            Err(error) => Err(error),
        }
    }

    pub async fn run(&self, mut stop: watch::Receiver<bool>) {
        loop {
            if *stop.borrow() {
                break;
            }
            let result = tokio::select! {
                result = self.run_once() => result,
                _ = stop.wait_for(|stop| *stop) => break,
            };
            match result {
                Ok(WorkResult::Idle) => {}
                Ok(result) => tracing::info!(?result, "index work completed"),
                Err(error) => tracing::warn!(?error, "index worker iteration failed"),
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(2)) => {},
                _ = stop.wait_for(|stop| *stop) => break,
            }
        }
    }
}

fn backoff(attempt: i32) -> u32 {
    2u32.saturating_pow(attempt.clamp(1, 6) as u32).min(60)
}
fn classify(error: BookError, attempt: i32) -> (&'static str, Option<u32>) {
    match error {
        BookError::Provider(GitError::RateLimited {
            retry_after_seconds,
        }) => {
            let delay = retry_after_seconds.unwrap_or(u64::from(backoff(attempt)));
            (
                "PROVIDER_RATE_LIMIT",
                u32::try_from(delay.max(1))
                    .ok()
                    .filter(|delay| *delay <= 86400),
            )
        }
        BookError::Provider(GitError::ProviderUnavailable)
        | BookError::ProcessingUnavailable
        | BookError::AuthorizationUnavailable => {
            ("INDEX_TEMPORARILY_UNAVAILABLE", Some(backoff(attempt)))
        }
        BookError::RepositoryNotConfigured => ("REPOSITORY_NOT_CONFIGURED", None),
        BookError::InvalidRepositoryConfiguration => ("REPOSITORY_CONFIGURATION_INVALID", None),
        BookError::NotFound
        | BookError::Provider(GitError::BranchMissing | GitError::FileMissing) => {
            ("SOURCE_NOT_AVAILABLE", None)
        }
        BookError::SearchSnapshotNotReady | BookError::InvalidCatalog => {
            ("SEARCH_SNAPSHOT_INVALID", None)
        }
        BookError::BookTooLarge => ("FILE_TOO_LARGE", None),
        BookError::InvalidQuery(code) | BookError::InvalidAsset(code) => (code, None),
        BookError::Provider(_) => ("INDEX_SOURCE_REJECTED", None),
    }
}
