//! Read main, prove the destination is absent, durably mark the mutation, then
//! create once. A recovered started mutation only observes the reserved ref.
use crate::provisioning::{FailureCertainty, PgProvisioning, ProvisionError, ProvisionLease};
use specspace_application::{book::BookError, reader::BookProviderFactory};
use specspace_git_adapter::GitError;
use std::{sync::Arc, time::Duration};
use tokio::sync::watch;
use uuid::Uuid;

#[derive(Debug, PartialEq, Eq)]
pub enum ProvisionWork {
    Idle,
    Completed(Uuid),
    Deferred(Uuid),
    Failed(Uuid),
    LeaseLost(Uuid),
}

pub struct ProposalWorker {
    journal: Arc<PgProvisioning>,
    source: Arc<dyn BookProviderFactory>,
    worker: Uuid,
    timeout: Duration,
}
impl ProposalWorker {
    pub fn new(
        journal: Arc<PgProvisioning>,
        source: Arc<dyn BookProviderFactory>,
        timeout: Duration,
    ) -> Result<Self, ProvisionError> {
        if !(30..=3600).contains(&timeout.as_secs()) {
            return Err(ProvisionError::InvalidLease);
        }
        Ok(Self {
            journal,
            source,
            timeout,
            worker: Uuid::new_v4(),
        })
    }
    pub async fn run_once(&self) -> Result<ProvisionWork, ProvisionError> {
        let Some(lease) = self.journal.claim(self.worker, 60).await? else {
            return Ok(ProvisionWork::Idle);
        };
        let work = self.provision(&lease);
        tokio::pin!(work);
        let deadline = tokio::time::sleep(self.timeout);
        tokio::pin!(deadline);
        let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        heartbeat.tick().await;
        let result = loop {
            tokio::select! {
                result = &mut work => break result,
                _ = heartbeat.tick() => match self.journal.heartbeat(&lease, 60).await {
                    Ok(()) => {}, Err(ProvisionError::LeaseLost) => return Ok(ProvisionWork::LeaseLost(lease.id)), Err(error) => return Err(error),
                },
                _ = &mut deadline => break Err(TaskError::Stop("PROVISIONING_TIMEOUT", Some(backoff(lease.attempts)))),
            }
        };
        match result {
            Ok(()) => Ok(ProvisionWork::Completed(lease.id)),
            Err(TaskError::Journal(ProvisionError::LeaseLost)) => {
                Ok(ProvisionWork::LeaseLost(lease.id))
            }
            Err(TaskError::Journal(ProvisionError::Unavailable)) => {
                Err(ProvisionError::Unavailable)
            }
            Err(error) => {
                let failure = classify(error, lease.attempts);
                match self
                    .journal
                    .record_failure(&lease, failure.code, failure.delay, failure.certainty)
                    .await
                {
                    Ok(true) => Ok(ProvisionWork::Deferred(lease.id)),
                    Ok(false) => Ok(ProvisionWork::Failed(lease.id)),
                    Err(ProvisionError::LeaseLost) => Ok(ProvisionWork::LeaseLost(lease.id)),
                    Err(error) => Err(error),
                }
            }
        }
    }
    async fn provision(&self, lease: &ProvisionLease) -> Result<(), TaskError> {
        self.journal.authorize(lease).await?;
        let provider = self
            .source
            .provider(lease.repository())
            .await
            .map_err(TaskError::Source)?;
        if lease.needs_reconciliation() {
            // Only an operation that durably crossed its mutation boundary may
            // recover a matching ref. A pre-existing ref in fresh work is rejected.
            let actual = match provider.branch_head(lease.branch()).await {
                Ok(actual) => actual,
                Err(GitError::BranchMissing) => {
                    return Err(TaskError::Stop(
                        "CREATE_RESULT_UNKNOWN",
                        Some(backoff(lease.attempts)),
                    ));
                }
                Err(error) => return Err(TaskError::Provider(error)),
            };
            if actual != *lease.source() {
                return Err(TaskError::Stop("BRANCH_HEAD_MISMATCH", None));
            }
            self.journal.complete(lease, &actual).await?;
            return Ok(());
        }
        let current = provider.main_head().await.map_err(TaskError::Provider)?;
        if current != *lease.source() {
            return Err(TaskError::Stop("SOURCE_MOVED", None));
        }
        match provider.branch_head(lease.branch()).await {
            Ok(_) => return Err(TaskError::Stop("BRANCH_EXISTS", None)),
            Err(GitError::BranchMissing) => {}
            Err(error) => return Err(TaskError::Provider(error)),
        }
        // Rechecks ACL, immutable plan and unexpired lease before allowing I/O.
        self.journal.begin_mutation(lease).await?;
        let actual = provider
            .create_branch(lease.branch(), lease.source())
            .await
            .map_err(TaskError::Provider)?;
        if actual != *lease.source() {
            return Err(TaskError::Stop("CREATE_RESULT_UNKNOWN", None));
        }
        // Treat any post-write observation failure as uncertain, including a
        // rate limit: it cannot authorize another create request.
        let observed = provider
            .branch_head(lease.branch())
            .await
            .map_err(|error| {
                TaskError::Stop(
                    "CREATE_RESULT_UNKNOWN",
                    observation_delay(&error, lease.attempts),
                )
            })?;
        if observed != actual {
            return Err(TaskError::Stop("BRANCH_HEAD_MISMATCH", None));
        }
        self.journal.complete(lease, &observed).await?;
        Ok(())
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
                Ok(ProvisionWork::Idle) => {}
                Ok(result) => tracing::info!(?result, "proposal provisioning completed"),
                Err(error) => tracing::warn!(?error, "proposal provisioning iteration failed"),
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(2)) => {},
                _ = stop.wait_for(|stop| *stop) => break,
            }
        }
    }
}

enum TaskError {
    Journal(ProvisionError),
    Source(BookError),
    Provider(GitError),
    Stop(&'static str, Option<u32>),
}
impl From<ProvisionError> for TaskError {
    fn from(error: ProvisionError) -> Self {
        Self::Journal(error)
    }
}
struct Failure {
    code: &'static str,
    delay: Option<u32>,
    certainty: FailureCertainty,
}
fn backoff(attempts: i32) -> u32 {
    2u32.saturating_pow(attempts.clamp(1, 6) as u32).min(60)
}
fn observation_delay(error: &GitError, attempts: i32) -> Option<u32> {
    let seconds = match error {
        GitError::RateLimited {
            retry_after_seconds: Some(seconds),
        }
        | GitError::ResultUnknownWithRetryAfter {
            retry_after_seconds: seconds,
        } => *seconds,
        _ => u64::from(backoff(attempts)),
    };
    u32::try_from(seconds.max(1))
        .ok()
        .filter(|seconds| *seconds <= 86400)
}
fn classify(error: TaskError, attempts: i32) -> Failure {
    let mut certainty = FailureCertainty::ObserveJournal;
    let (code, delay) = match error {
        TaskError::Journal(ProvisionError::AccessRevoked) => ("PERMISSION_REVOKED", None),
        TaskError::Journal(_) => ("PROVISIONING_STATE_CHANGED", None),
        TaskError::Stop(code, delay) => (code, delay),
        TaskError::Source(BookError::RepositoryNotConfigured) => {
            ("REPOSITORY_NOT_CONFIGURED", None)
        }
        TaskError::Source(BookError::InvalidRepositoryConfiguration) => {
            ("REPOSITORY_CONFIGURATION_INVALID", None)
        }
        TaskError::Source(
            BookError::AuthorizationUnavailable | BookError::ProcessingUnavailable,
        ) => ("PROVIDER_UNAVAILABLE", Some(backoff(attempts))),
        TaskError::Provider(error) | TaskError::Source(BookError::Provider(error)) => match error {
            GitError::RateLimited {
                retry_after_seconds,
            } => {
                certainty = FailureCertainty::DefinitelyNotApplied;
                (
                    "PROVIDER_RATE_LIMIT",
                    u32::try_from(
                        retry_after_seconds
                            .unwrap_or(u64::from(backoff(attempts)))
                            .max(1),
                    )
                    .ok()
                    .filter(|delay| *delay <= 86400),
                )
            }
            GitError::ResultUnknown | GitError::ProviderUnavailable => {
                ("CREATE_RESULT_UNKNOWN", Some(backoff(attempts)))
            }
            error @ GitError::ResultUnknownWithRetryAfter { .. } => {
                ("CREATE_RESULT_UNKNOWN", observation_delay(&error, attempts))
            }
            GitError::BranchExists => {
                certainty = FailureCertainty::DefinitelyNotApplied;
                ("BRANCH_EXISTS", None)
            }
            GitError::Unauthorized
            | GitError::ProviderRejected
            | GitError::InvalidInput
            | GitError::BranchMissing
            | GitError::FileMissing => {
                certainty = FailureCertainty::DefinitelyNotApplied;
                ("PROVIDER_REJECTED", None)
            }
            _ => ("CREATE_RESULT_UNKNOWN", Some(backoff(attempts))),
        },
        TaskError::Source(_) => ("PROVISIONING_SOURCE_UNAVAILABLE", None),
    };
    Failure {
        code,
        delay,
        certainty,
    }
}
