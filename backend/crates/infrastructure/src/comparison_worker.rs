//! Read-only durable work. Reclaims repeat reads, never Git mutations. A ready
//! result appears only after one atomic, current-scope-checked DB publication.
use crate::comparisons::{ComparisonLease, ComparisonStoreError, PgComparisons};
use specspace_application::{
    book::BookError,
    comparison_reader::{ComparisonReadError, ComparisonReader},
};
use specspace_git_adapter::GitError;
use std::{sync::Arc, time::Duration};
use tokio::sync::watch;
use uuid::Uuid;
#[derive(Debug, PartialEq, Eq)]
pub enum ComparisonWork {
    Idle,
    Completed(Uuid),
    Deferred(Uuid),
    Failed(Uuid),
    LeaseLost(Uuid),
}
pub struct ComparisonWorker {
    store: Arc<PgComparisons>,
    reader: Arc<ComparisonReader>,
    worker: Uuid,
    timeout: Duration,
}
impl ComparisonWorker {
    pub fn new(
        store: Arc<PgComparisons>,
        reader: Arc<ComparisonReader>,
        timeout: Duration,
    ) -> Result<Self, ComparisonStoreError> {
        if !(Duration::from_secs(30)..=Duration::from_secs(900)).contains(&timeout) {
            return Err(ComparisonStoreError::InvalidQuery);
        }
        Ok(Self {
            store,
            reader,
            worker: Uuid::new_v4(),
            timeout,
        })
    }
    pub async fn run_once(&self) -> Result<ComparisonWork, ComparisonStoreError> {
        let Some(lease) = self.store.claim(self.worker, 60).await? else {
            return Ok(ComparisonWork::Idle);
        };
        let work = async {
            self.store.authorize(&lease).await?;
            let loaded = self
                .reader
                .load(lease.actor(), lease.proposal(), lease.selection())
                .await?;
            Ok::<_, ComparisonStoreError>(loaded)
        };
        tokio::pin!(work);
        let deadline_at = tokio::time::Instant::now() + self.timeout;
        let deadline = tokio::time::sleep_until(deadline_at);
        tokio::pin!(deadline);
        let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        heartbeat.tick().await;
        let result = loop {
            tokio::select! {
                result=&mut work=>break result,
                _=&mut deadline=>return self.failure(&lease,"COMPARISON_TIMEOUT",Some(backoff(lease.attempts()))).await,
                _=heartbeat.tick()=>{
                    match self.store.heartbeat(&lease,60).await {
                        Ok(())=>{},Err(ComparisonStoreError::LeaseLost)=>return Ok(ComparisonWork::LeaseLost(lease.job_id())),Err(ComparisonStoreError::Unavailable)=>return Err(ComparisonStoreError::Unavailable),Err(error)=>{let (code,delay)=classify(error,lease.attempts());return self.failure(&lease,code,delay).await},
                    }
                }
            }
        };
        let result = match result {
            Ok(loaded) => match self.store.publication_lease(&lease).await {
                Ok(()) => {
                    // No concurrent heartbeat waits on this worker's own
                    // publication lock. The bounded transaction fits inside
                    // its refreshed lease and still fences its final update.
                    let remaining = deadline_at
                        .saturating_duration_since(tokio::time::Instant::now())
                        .min(Duration::from_secs(60));
                    match tokio::time::timeout(remaining, self.store.publish(&lease, &loaded)).await
                    {
                        Ok(result) => result,
                        Err(_) => {
                            return self
                                .failure(
                                    &lease,
                                    "COMPARISON_PUBLICATION_TIMEOUT",
                                    Some(backoff(lease.attempts())),
                                )
                                .await;
                        }
                    }
                }
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        };
        match result {
            Ok(()) => Ok(ComparisonWork::Completed(lease.job_id())),
            Err(ComparisonStoreError::LeaseLost) => Ok(ComparisonWork::LeaseLost(lease.job_id())),
            Err(ComparisonStoreError::Unavailable) => Err(ComparisonStoreError::Unavailable),
            Err(error) => {
                let (code, delay) = classify(error, lease.attempts());
                self.failure(&lease, code, delay).await
            }
        }
    }
    async fn failure(
        &self,
        lease: &ComparisonLease,
        code: &str,
        delay: Option<u32>,
    ) -> Result<ComparisonWork, ComparisonStoreError> {
        match self.store.fail(lease, code, delay).await {
            Ok(true) => Ok(ComparisonWork::Deferred(lease.job_id())),
            Ok(false) => Ok(ComparisonWork::Failed(lease.job_id())),
            Err(ComparisonStoreError::LeaseLost) => Ok(ComparisonWork::LeaseLost(lease.job_id())),
            Err(error) => Err(error),
        }
    }
    pub async fn run(&self, mut stop: watch::Receiver<bool>) {
        loop {
            if *stop.borrow() {
                break;
            }
            let result =
                tokio::select! {result=self.run_once()=>result,_=stop.wait_for(|stop|*stop)=>break};
            match result {
                Ok(ComparisonWork::Idle) => {}
                Ok(result) => tracing::info!(?result, "comparison work completed"),
                Err(error) => tracing::warn!(?error, "comparison work failed"),
            }
            tokio::select! {_=tokio::time::sleep(Duration::from_secs(2))=>{},_=stop.wait_for(|stop|*stop)=>break}
        }
    }
}
fn backoff(attempt: i32) -> u32 {
    2u32.saturating_pow(attempt.clamp(1, 6) as u32).min(60)
}
fn classify(error: ComparisonStoreError, attempt: i32) -> (&'static str, Option<u32>) {
    match error {
        ComparisonStoreError::Read(
            ComparisonReadError::Provider(error)
            | ComparisonReadError::Source(BookError::Provider(error)),
        ) => match error {
            GitError::RateLimited {
                retry_after_seconds,
            } => {
                let delay = retry_after_seconds
                    .unwrap_or(u64::from(backoff(attempt)))
                    .max(1);
                (
                    "PROVIDER_RATE_LIMIT",
                    u32::try_from(delay).ok().filter(|n| *n <= 86400),
                )
            }
            GitError::ProviderUnavailable
            | GitError::ResultUnknown
            | GitError::ResultUnknownWithRetryAfter { .. } => {
                ("COMPARISON_PROVIDER_UNAVAILABLE", Some(backoff(attempt)))
            }
            _ => ("COMPARISON_PROVIDER_REJECTED", None),
        },
        ComparisonStoreError::Read(
            ComparisonReadError::Busy
            | ComparisonReadError::TimedOut
            | ComparisonReadError::ProcessingUnavailable,
        ) => ("COMPARISON_PROCESSING_DELAY", Some(backoff(attempt))),
        ComparisonStoreError::Read(ComparisonReadError::Source(
            BookError::AuthorizationUnavailable,
        )) => (
            "COMPARISON_AUTHORIZATION_UNAVAILABLE",
            Some(backoff(attempt)),
        ),
        ComparisonStoreError::Read(ComparisonReadError::NotFound) => ("COMPARISON_FORBIDDEN", None),
        ComparisonStoreError::Read(ComparisonReadError::ContextChanged) => {
            ("COMPARISON_CONTEXT_CHANGED", None)
        }
        ComparisonStoreError::Read(ComparisonReadError::Source(
            BookError::RepositoryNotConfigured,
        )) => ("COMPARISON_SETUP_REQUIRED", None),
        _ => ("INVALID_COMPARISON_SNAPSHOT", None),
    }
}
