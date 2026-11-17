//! Durable read-only preparation followed by atomic snapshot publication. This
//! worker never creates branches, commits, PRs or checks at the Git provider.
use crate::{
    review_snapshots::{PgReviewSnapshots, SnapshotError},
    review_submissions::{PgReviewSubmissions, SubmissionError, SubmissionLease},
};
use specspace_application::{
    book::BookError,
    comparison_reader::ComparisonReadError,
    review_context::ReviewContextError,
    review_preparation::{ReviewPreparationError, ReviewPreparer},
};
use specspace_git_adapter::GitError;
use std::{sync::Arc, time::Duration};
use tokio::sync::watch;
use uuid::Uuid;

#[derive(Debug, PartialEq, Eq)]
pub enum ReviewWork {
    Idle,
    Completed(Uuid),
    Deferred(Uuid),
    Failed(Uuid),
    LeaseLost(Uuid),
}
pub struct ReviewWorker {
    submissions: Arc<PgReviewSubmissions>,
    snapshots: Arc<PgReviewSnapshots>,
    preparer: Arc<ReviewPreparer>,
    worker: Uuid,
    timeout: Duration,
}
impl ReviewWorker {
    pub fn new(
        submissions: Arc<PgReviewSubmissions>,
        snapshots: Arc<PgReviewSnapshots>,
        preparer: Arc<ReviewPreparer>,
        timeout: Duration,
    ) -> Result<Self, SubmissionError> {
        if !(Duration::from_secs(30)..=Duration::from_secs(900)).contains(&timeout) {
            return Err(SubmissionError::InvalidQuery);
        }
        Ok(Self {
            submissions,
            snapshots,
            preparer,
            worker: Uuid::new_v4(),
            timeout,
        })
    }
    pub async fn run_once(&self) -> Result<ReviewWork, SubmissionError> {
        let Some(lease) = self.submissions.claim(self.worker, 60).await? else {
            return Ok(ReviewWork::Idle);
        };
        let bound = self
            .preparer
            .with_authorization(Arc::new(self.submissions.authorization(&lease)));
        let preparation = async {
            let candidate = bound
                .prepare(lease.actor(), lease.proposal(), lease.expectation().clone())
                .await?;
            bound.verify_media(candidate).await
        };
        tokio::pin!(preparation);
        let deadline_at = tokio::time::Instant::now() + self.timeout;
        let deadline = tokio::time::sleep_until(deadline_at);
        tokio::pin!(deadline);
        let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        heartbeat.tick().await;
        let verified = loop {
            tokio::select! {
                result=&mut preparation=>match result {
                    Ok(verified)=>break verified,
                    Err(error)=>return self.failed(&lease,SnapshotError::Preparation(error)).await,
                },
                _=&mut deadline=>return self.failure(&lease,"REVIEW_PREPARATION_TIMEOUT",Some(backoff(lease.attempts()))).await,
                _=heartbeat.tick()=>match tokio::time::timeout_at(deadline_at,self.submissions.heartbeat(&lease,60)).await {
                    Ok(Ok(()))=>{},
                    Ok(Err(error))=>return self.failed(&lease,SnapshotError::Submission(error)).await,
                    Err(_)=>return self.failure(&lease,"REVIEW_PREPARATION_TIMEOUT",Some(backoff(lease.attempts()))).await,
                },
            }
        };
        // No concurrent heartbeat can wait on or cancel our own publication.
        // The publisher renews its lease and applies its final attempt fence.
        let remaining = deadline_at.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(
            remaining,
            self.snapshots.publish(&lease, &verified, &self.preparer),
        )
        .await
        {
            Ok(Ok(_)) => Ok(ReviewWork::Completed(lease.job_id())),
            Ok(Err(error)) => self.failed(&lease, error).await,
            Err(_) => {
                self.failure(
                    &lease,
                    "REVIEW_PUBLICATION_TIMEOUT",
                    Some(backoff(lease.attempts())),
                )
                .await
            }
        }
    }
    async fn failed(
        &self,
        lease: &SubmissionLease,
        error: SnapshotError,
    ) -> Result<ReviewWork, SubmissionError> {
        match error {
            SnapshotError::Submission(SubmissionError::Unavailable) => {
                Err(SubmissionError::Unavailable)
            }
            SnapshotError::Submission(SubmissionError::LeaseLost)
            | SnapshotError::Submission(SubmissionError::Context(ReviewContextError::LeaseLost))
            | SnapshotError::Preparation(ReviewPreparationError::Context(
                ReviewContextError::LeaseLost,
            )) => Ok(ReviewWork::LeaseLost(lease.job_id())),
            other => {
                let (code, delay) = classify(other, lease.attempts());
                self.failure(lease, code, delay).await
            }
        }
    }
    async fn failure(
        &self,
        lease: &SubmissionLease,
        code: &str,
        delay: Option<u32>,
    ) -> Result<ReviewWork, SubmissionError> {
        match self.submissions.fail(lease, code, delay).await {
            Ok(true) => Ok(ReviewWork::Deferred(lease.job_id())),
            Ok(false) => Ok(ReviewWork::Failed(lease.job_id())),
            Err(SubmissionError::LeaseLost) => Ok(ReviewWork::LeaseLost(lease.job_id())),
            Err(error) => Err(error),
        }
    }
    pub async fn run(&self, mut stop: watch::Receiver<bool>) {
        loop {
            if *stop.borrow() {
                break;
            }
            let result = tokio::select! { result=self.run_once()=>result,_=stop.wait_for(|stop|*stop)=>break };
            match result {
                Ok(ReviewWork::Idle) => {}
                Ok(result) => tracing::info!(?result, "review snapshot work completed"),
                Err(error) => tracing::warn!(?error, "review snapshot work failed"),
            }
            tokio::select! {_=tokio::time::sleep(Duration::from_secs(2))=>{},_=stop.wait_for(|stop|*stop)=>break}
        }
    }
}
fn backoff(attempt: i32) -> u32 {
    2u32.saturating_pow(attempt.clamp(1, 6) as u32).min(60)
}
fn provider(error: GitError, attempt: i32) -> (&'static str, Option<u32>) {
    let delay = |seconds: Option<u64>| {
        u32::try_from(seconds.unwrap_or(u64::from(backoff(attempt))).max(1))
            .ok()
            .filter(|seconds| *seconds <= 86400)
    };
    match error {
        GitError::RateLimited {
            retry_after_seconds,
        } => ("REVIEW_PROVIDER_RATE_LIMIT", delay(retry_after_seconds)),
        GitError::ResultUnknownWithRetryAfter {
            retry_after_seconds,
        } => (
            "REVIEW_PROVIDER_UNAVAILABLE",
            delay(Some(retry_after_seconds)),
        ),
        GitError::ProviderUnavailable | GitError::ResultUnknown => {
            ("REVIEW_PROVIDER_UNAVAILABLE", Some(backoff(attempt)))
        }
        GitError::Unauthorized => ("REVIEW_PROVIDER_UNAUTHORIZED", None),
        _ => ("REVIEW_PROVIDER_REJECTED", None),
    }
}
fn context(error: ReviewContextError, attempt: i32) -> (&'static str, Option<u32>) {
    match error {
        ReviewContextError::Read(error) => comparison(error, attempt),
        ReviewContextError::Unavailable => {
            ("REVIEW_AUTHORIZATION_UNAVAILABLE", Some(backoff(attempt)))
        }
        ReviewContextError::Forbidden => ("REVIEW_FORBIDDEN", None),
        ReviewContextError::StateChanged => ("REVIEW_STATE_CHANGED", None),
        ReviewContextError::Stale => ("REVIEW_CONTEXT_CHANGED", None),
        ReviewContextError::Busy => ("REVIEW_BUSY", Some(backoff(attempt))),
        ReviewContextError::OwnerNotEligible => ("REVIEW_OWNER_NOT_ELIGIBLE", None),
        _ => ("REVIEW_CONFIGURATION_INVALID", None),
    }
}
fn comparison(error: ComparisonReadError, attempt: i32) -> (&'static str, Option<u32>) {
    match error {
        ComparisonReadError::Provider(error)
        | ComparisonReadError::Source(BookError::Provider(error)) => provider(error, attempt),
        ComparisonReadError::Source(BookError::AuthorizationUnavailable) => {
            ("REVIEW_AUTHORIZATION_UNAVAILABLE", Some(backoff(attempt)))
        }
        ComparisonReadError::Source(BookError::ProcessingUnavailable)
        | ComparisonReadError::Busy
        | ComparisonReadError::TimedOut
        | ComparisonReadError::ProcessingUnavailable => {
            ("REVIEW_PROCESSING_DELAY", Some(backoff(attempt)))
        }
        ComparisonReadError::NotFound => ("REVIEW_FORBIDDEN", None),
        ComparisonReadError::ContextChanged => ("REVIEW_CONTEXT_CHANGED", None),
        ComparisonReadError::InvalidConfiguration
        | ComparisonReadError::Source(
            BookError::RepositoryNotConfigured | BookError::InvalidRepositoryConfiguration,
        ) => ("REVIEW_SETUP_REQUIRED", None),
        _ => ("REVIEW_COMPARISON_INVALID", None),
    }
}
fn classify(error: SnapshotError, attempt: i32) -> (&'static str, Option<u32>) {
    match error {
        SnapshotError::Submission(SubmissionError::Context(error)) => context(error, attempt),
        SnapshotError::Submission(SubmissionError::Forbidden | SubmissionError::NotFound) => {
            ("REVIEW_FORBIDDEN", None)
        }
        SnapshotError::Submission(SubmissionError::Busy) => ("REVIEW_BUSY", Some(backoff(attempt))),
        SnapshotError::TimedOut => ("REVIEW_PUBLICATION_TIMEOUT", Some(backoff(attempt))),
        SnapshotError::Preparation(error) => match error {
            ReviewPreparationError::Provider(error) => provider(error, attempt),
            ReviewPreparationError::Comparison(error) => comparison(error, attempt),
            ReviewPreparationError::Context(error) => context(error, attempt),
            ReviewPreparationError::Busy
            | ReviewPreparationError::TimedOut
            | ReviewPreparationError::ProcessingUnavailable => {
                ("REVIEW_PROCESSING_DELAY", Some(backoff(attempt)))
            }
            ReviewPreparationError::ContextChanged => ("REVIEW_CONTEXT_CHANGED", None),
            ReviewPreparationError::HeadChanged => ("REVIEW_HEAD_CHANGED", None),
            ReviewPreparationError::TargetChanged => ("REVIEW_TARGET_CHANGED", None),
            ReviewPreparationError::Plan(error) => (error.code(), None),
            ReviewPreparationError::InvalidMedia { code, .. } => (code, None),
        },
        _ => ("REVIEW_EVIDENCE_INVALID", None),
    }
}
