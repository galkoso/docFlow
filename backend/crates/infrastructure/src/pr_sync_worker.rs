//! Bounded provider orchestration for the immutable PR journal. A started write
//! is never repeated: subsequent attempts only observe and reconcile its result.
use crate::{
    pr_sync::{PgPrSync, PrSyncLease, PrSyncObservation, PrSyncResolution, PrSyncStoreError},
    review_submissions::SubmissionError,
};
use specspace_application::{
    book::BookError,
    comparison_reader::{BoundProviderFactory, ComparisonReadError},
    pr_sync::{PrBinding, PrProviderError, PrProviderFactory, PrSyncDecision, PrWritePhase},
    review_context::ReviewContextError,
};
use specspace_git_adapter::{
    CommitSha, GitError, GitProvider,
    pull_requests::{
        PullRequest, PullRequestError, PullRequestPage, PullRequestProvider, PullRequestState,
    },
};
use std::{future::Future, sync::Arc, time::Duration};
use tokio::sync::watch;
use uuid::Uuid;

#[derive(Debug, PartialEq, Eq)]
pub enum PrSyncWork {
    Idle,
    Completed(Uuid, PrSyncResolution),
    Deferred(Uuid),
    Stopped(Uuid),
    LeaseLost(Uuid),
}
enum WorkError {
    Store(PrSyncStoreError),
    Pr(PullRequestError),
    Git(GitError),
    Code(&'static str, Option<u32>),
}
impl From<PrSyncStoreError> for WorkError {
    fn from(e: PrSyncStoreError) -> Self {
        Self::Store(e)
    }
}
impl From<PullRequestError> for WorkError {
    fn from(e: PullRequestError) -> Self {
        Self::Pr(e)
    }
}
impl From<GitError> for WorkError {
    fn from(e: GitError) -> Self {
        Self::Git(e)
    }
}
pub struct PrSyncWorker {
    journal: Arc<PgPrSync>,
    refs: Arc<dyn BoundProviderFactory>,
    prs: Arc<dyn PrProviderFactory>,
    worker: Uuid,
    timeout: Duration,
}
impl PrSyncWorker {
    pub fn new(
        journal: Arc<PgPrSync>,
        refs: Arc<dyn BoundProviderFactory>,
        prs: Arc<dyn PrProviderFactory>,
        timeout: Duration,
    ) -> Result<Self, PrSyncStoreError> {
        if !(Duration::from_secs(30)..=Duration::from_secs(900)).contains(&timeout) {
            return Err(PrSyncStoreError::InvalidQuery);
        }
        Ok(Self {
            journal,
            refs,
            prs,
            worker: Uuid::new_v4(),
            timeout,
        })
    }
    pub async fn run_once(&self) -> Result<PrSyncWork, PrSyncStoreError> {
        let Some(lease) = self.journal.claim(self.worker, 60).await? else {
            return Ok(PrSyncWork::Idle);
        };
        let deadline = tokio::time::Instant::now() + self.timeout;
        let candidate = match tokio::time::timeout_at(deadline, self.prepare(&lease)).await {
            Ok(Ok(pr)) => pr,
            Ok(Err(error)) => return self.failure(&lease, error).await,
            Err(_) => {
                return self
                    .failure(
                        &lease,
                        WorkError::Code("PR_SYNC_TIMEOUT", Some(backoff(lease.attempts()))),
                    )
                    .await;
            }
        };
        // No heartbeat races our own transaction. The journal applies the final
        // attempt/deadline/authority fence and atomically publishes the receipt.
        match tokio::time::timeout_at(deadline, self.journal.complete(&lease, &candidate)).await {
            Ok(Ok(result)) => Ok(PrSyncWork::Completed(lease.job_id(), result)),
            Ok(Err(error)) => self.failure(&lease, error.into()).await,
            Err(_) => {
                self.failure(
                    &lease,
                    WorkError::Code(
                        "PR_SYNC_PUBLICATION_TIMEOUT",
                        Some(backoff(lease.attempts())),
                    ),
                )
                .await
            }
        }
    }
    // Heartbeats run only during provider IO, never while our own journal
    // transaction might hold the proposal/job locks the heartbeat needs.
    async fn io<T, E, F>(&self, lease: &PrSyncLease, future: F) -> Result<T, WorkError>
    where
        F: Future<Output = Result<T, E>>,
        WorkError: From<E>,
    {
        tokio::pin!(future);
        let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        heartbeat.tick().await;
        loop {
            tokio::select! {
                result=&mut future=>{
                    // Renew between short calls too: resetting a per-call timer
                    // alone would let a long sequence of short requests expire.
                    self.journal.heartbeat(lease,60).await?;
                    return result.map_err(WorkError::from);
                },
                _=heartbeat.tick()=>self.journal.heartbeat(lease,60).await?,
            }
        }
    }
    async fn authority(&self, lease: &PrSyncLease) -> Result<PrSyncObservation, WorkError> {
        let view = self.journal.observe(lease).await?;
        if view.phase == PrWritePhase::NotStarted {
            return Ok(self.journal.check(lease).await?);
        }
        Ok(view)
    }
    async fn prepare(&self, lease: &PrSyncLease) -> Result<PullRequest, WorkError> {
        let view = self.authority(lease).await?;
        let binding = &lease.intent().source().repository;
        let refs = self
            .io(lease, async {
                self.refs
                    .provider_bound(binding)
                    .await
                    .map_err(|e| comparison(e, lease.attempts()))
            })
            .await?;
        self.authority(lease).await?;
        let prs = self
            .io(lease, async {
                self.prs
                    .pr_provider_bound(binding)
                    .await
                    .map_err(|e| match e {
                        PrProviderError::BindingChanged => {
                            WorkError::Code("PR_BINDING_CHANGED", None)
                        }
                        PrProviderError::Source(e) => book(e, lease.attempts()),
                    })
            })
            .await?;
        self.authority(lease).await?;
        let observed = self.discover(lease, prs.as_ref(), view.binding).await?;
        if view.phase != PrWritePhase::NotStarted {
            return self.reconcile(lease, refs.as_ref(), observed).await;
        }
        self.expected_refs(lease, refs.as_ref()).await?;
        let observations: Vec<_> = observed.into_iter().collect();
        let decision = lease
            .intent()
            .decide(&observations, view.binding, view.phase);
        match decision {
            PrSyncDecision::Synchronized(_) if view.binding.is_some() => {
                self.reconcile(lease, refs.as_ref(), observations.into_iter().next())
                    .await
            }
            PrSyncDecision::CreateDraft | PrSyncDecision::UpdateMetadata(_) => {
                let phase = self.journal.start_write(lease, &observations).await?;
                // A change in the gap after journaling cannot silently authorize
                // a write of obsolete context. Such a gap retains uncertainty.
                self.journal.check(lease).await?;
                self.expected_refs(lease, refs.as_ref()).await?;
                self.journal.check(lease).await?;
                let scope = lease.intent().scope();
                let receipt = match (decision, phase) {
                    (PrSyncDecision::CreateDraft, PrWritePhase::CreateStarted) => {
                        self.io(
                            lease,
                            prs.create_pull_request(&scope, lease.intent().content()),
                        )
                        .await?
                    }
                    (PrSyncDecision::UpdateMetadata(number), PrWritePhase::UpdateStarted) => {
                        self.io(
                            lease,
                            prs.update_pull_request(&scope, number, lease.intent().content()),
                        )
                        .await?
                    }
                    _ => return Err(WorkError::Code("PR_JOURNAL_PHASE_INVALID", None)),
                };
                self.authority(lease).await?;
                if !exact(lease, &receipt)
                    || view
                        .binding
                        .is_some_and(|b| b.number != receipt.number || b.provider_id != receipt.id)
                {
                    return Err(WorkError::Code(
                        "PR_WRITE_RECEIPT_INVALID",
                        Some(backoff(lease.attempts())),
                    ));
                }
                let hint = PrBinding {
                    number: receipt.number,
                    provider_id: receipt.id,
                };
                let observed = self.discover(lease, prs.as_ref(), Some(hint)).await?;
                if observed
                    .as_ref()
                    .is_some_and(|pr| pr.node_id != receipt.node_id)
                {
                    return Err(WorkError::Code("PR_IDENTITY_CONFLICT", None));
                }
                self.reconcile(lease, refs.as_ref(), observed).await
            }
            PrSyncDecision::ObserveAgain => Err(WorkError::Code(
                "PR_OBSERVATION_PENDING",
                Some(backoff(lease.attempts())),
            )),
            PrSyncDecision::Stale => Err(WorkError::Code("PR_REFS_CHANGED", None)),
            _ => Err(WorkError::Code("PR_IDENTITY_CONFLICT", None)),
        }
    }
    async fn discover(
        &self,
        lease: &PrSyncLease,
        prs: &dyn PullRequestProvider,
        hint: Option<PrBinding>,
    ) -> Result<Option<PullRequest>, WorkError> {
        self.authority(lease).await?;
        let scope = lease.intent().scope();
        // Two candidates already prove a conflict. Zero/one plus exhaustion
        // proves complete discovery without retaining a large metadata listing.
        let listing = self
            .io(
                lease,
                prs.pull_requests(
                    &scope,
                    PullRequestPage::new(1, 2).expect("fixed bounded page"),
                ),
            )
            .await?;
        self.authority(lease).await?;
        if listing.items.len() >= 2 {
            return Err(WorkError::Code("PR_IDENTITY_CONFLICT", None));
        }
        if listing.next.is_some() {
            return Err(WorkError::Code("PR_DISCOVERY_INCOMPLETE", None));
        }
        let candidate = listing.items.into_iter().next();
        if let Some(pr) = &candidate
            && (pr.repository_id != scope.repository_id()
                || &pr.branch != scope.branch()
                || pr.id == 0
                || hint.is_some_and(|b| b.number != pr.number || b.provider_id != pr.id))
        {
            return Err(WorkError::Code("PR_IDENTITY_CONFLICT", None));
        }
        let identity = candidate
            .as_ref()
            .map(|pr| PrBinding {
                number: pr.number,
                provider_id: pr.id,
            })
            .or(hint);
        let Some(identity) = identity else {
            return Ok(None);
        };
        let pr = match self
            .io(lease, prs.pull_request(&scope, identity.number))
            .await
        {
            Err(WorkError::Pr(PullRequestError::NotFound)) => {
                self.authority(lease).await?;
                return Err(WorkError::Code(
                    "PR_OBSERVATION_PENDING",
                    Some(backoff(lease.attempts())),
                ));
            }
            result => result?,
        };
        self.authority(lease).await?;
        if pr.number != identity.number
            || pr.id != identity.provider_id
            || pr.repository_id != scope.repository_id()
            || &pr.branch != scope.branch()
            || candidate
                .as_ref()
                .is_some_and(|old| old.node_id != pr.node_id)
        {
            return Err(WorkError::Code("PR_IDENTITY_CONFLICT", None));
        }
        Ok(Some(pr))
    }
    async fn refs(
        &self,
        lease: &PrSyncLease,
        refs: &dyn GitProvider,
    ) -> Result<(CommitSha, CommitSha), WorkError> {
        self.authority(lease).await?;
        let target = self.io(lease, refs.main_head()).await?;
        self.authority(lease).await?;
        let head = self
            .io(lease, refs.branch_head(&lease.intent().source().branch))
            .await?;
        self.authority(lease).await?;
        Ok((head, target))
    }
    async fn expected_refs(
        &self,
        lease: &PrSyncLease,
        refs: &dyn GitProvider,
    ) -> Result<(), WorkError> {
        let (head, target) = self.refs(lease, refs).await?;
        let source = lease.intent().source();
        if head != source.head || target != source.target {
            return Err(WorkError::Code("PR_REFS_CHANGED", None));
        }
        Ok(())
    }
    async fn reconcile(
        &self,
        lease: &PrSyncLease,
        refs: &dyn GitProvider,
        observed: Option<PullRequest>,
    ) -> Result<PullRequest, WorkError> {
        let pr = observed.ok_or(WorkError::Code(
            "PR_OBSERVATION_PENDING",
            Some(backoff(lease.attempts())),
        ))?;
        if !exact(lease, &pr) {
            return Err(WorkError::Code(
                "PR_METADATA_NOT_OBSERVED",
                Some(backoff(lease.attempts())),
            ));
        }
        let (head, target) = self.refs(lease, refs).await?;
        if pr.head != head || pr.target != target {
            return Err(WorkError::Code(
                "PR_PROVIDER_REFS_UNSETTLED",
                Some(backoff(lease.attempts())),
            ));
        }
        self.authority(lease).await?;
        Ok(pr)
    }
    async fn failure(
        &self,
        lease: &PrSyncLease,
        error: WorkError,
    ) -> Result<PrSyncWork, PrSyncStoreError> {
        let (code, delay) = match error {
            WorkError::Store(PrSyncStoreError::Unavailable) => {
                return Err(PrSyncStoreError::Unavailable);
            }
            WorkError::Store(PrSyncStoreError::LeaseLost) => {
                return Ok(PrSyncWork::LeaseLost(lease.job_id()));
            }
            WorkError::Store(e) => classify_store(e, lease.attempts()),
            WorkError::Pr(PullRequestError::Provider(e)) | WorkError::Git(e) => {
                provider(e, lease.attempts())
            }
            WorkError::Pr(PullRequestError::NotFound) => {
                ("PR_OBSERVATION_PENDING", Some(backoff(lease.attempts())))
            }
            WorkError::Pr(_) => (
                "PR_PROVIDER_RESPONSE_INVALID",
                Some(backoff(lease.attempts())),
            ),
            WorkError::Code(code, delay) => (code, delay),
        };
        match self.journal.defer(lease, code, delay).await {
            Ok(true) => Ok(PrSyncWork::Deferred(lease.job_id())),
            Ok(false) => Ok(PrSyncWork::Stopped(lease.job_id())),
            Err(PrSyncStoreError::LeaseLost) => Ok(PrSyncWork::LeaseLost(lease.job_id())),
            Err(e) => Err(e),
        }
    }
    pub async fn run(&self, mut stop: watch::Receiver<bool>) {
        loop {
            if *stop.borrow() {
                break;
            }
            let result =
                tokio::select! {result=self.run_once()=>result,_=stop.wait_for(|s|*s)=>break};
            match result {
                Ok(PrSyncWork::Idle) => {}
                Ok(work) => tracing::info!(?work, "PR sync work completed"),
                Err(error) => tracing::warn!(?error, "PR sync journal unavailable"),
            }
            tokio::select! {_=tokio::time::sleep(Duration::from_secs(2))=>{},_=stop.wait_for(|s|*s)=>break}
        }
    }
}
fn exact(lease: &PrSyncLease, pr: &PullRequest) -> bool {
    let s = lease.intent().source();
    pr.id > 0
        && pr.repository_id == s.repository.remote_id
        && pr.branch == s.branch
        && pr.state == PullRequestState::Open
        && pr.draft
        && pr.title == lease.intent().content().title()
        && pr.body == lease.intent().content().body()
}
fn backoff(attempt: i32) -> u32 {
    2u32.saturating_pow(attempt.clamp(1, 6) as u32).min(60)
}
fn provider(error: GitError, attempt: i32) -> (&'static str, Option<u32>) {
    let delay = |s: Option<u64>| {
        u32::try_from(s.unwrap_or(u64::from(backoff(attempt))).max(1))
            .ok()
            .filter(|n| *n <= 86400)
    };
    match error {
        GitError::RateLimited {
            retry_after_seconds,
        } => ("PR_PROVIDER_RATE_LIMIT", delay(retry_after_seconds)),
        GitError::ResultUnknownWithRetryAfter {
            retry_after_seconds,
        } => ("PR_RESULT_UNKNOWN", delay(Some(retry_after_seconds))),
        GitError::ProviderUnavailable | GitError::ResultUnknown => {
            ("PR_PROVIDER_UNAVAILABLE", Some(backoff(attempt)))
        }
        GitError::Unauthorized => ("PR_PROVIDER_UNAUTHORIZED", None),
        _ => ("PR_PROVIDER_REJECTED", None),
    }
}
fn book(error: BookError, attempt: i32) -> WorkError {
    match error {
        BookError::Provider(e) => {
            let (c, d) = provider(e, attempt);
            WorkError::Code(c, d)
        }
        BookError::AuthorizationUnavailable | BookError::ProcessingUnavailable => {
            WorkError::Code("PR_CONNECTION_UNAVAILABLE", Some(backoff(attempt)))
        }
        _ => WorkError::Code("PR_SETUP_REQUIRED", None),
    }
}
fn comparison(error: ComparisonReadError, attempt: i32) -> WorkError {
    match error {
        ComparisonReadError::ContextChanged => WorkError::Code("PR_BINDING_CHANGED", None),
        ComparisonReadError::Source(e) => book(e, attempt),
        ComparisonReadError::Provider(e) => WorkError::Git(e),
        ComparisonReadError::NotFound => WorkError::Code("PR_FORBIDDEN", None),
        _ => WorkError::Code("PR_CONNECTION_UNAVAILABLE", Some(backoff(attempt))),
    }
}
fn classify_store(error: PrSyncStoreError, attempt: i32) -> (&'static str, Option<u32>) {
    match error {
        PrSyncStoreError::Busy | PrSyncStoreError::Access(SubmissionError::Busy) => {
            ("PR_CONTEXT_BUSY", Some(backoff(attempt)))
        }
        PrSyncStoreError::Access(SubmissionError::Context(
            ReviewContextError::Unavailable
            | ReviewContextError::Read(ComparisonReadError::Source(
                BookError::AuthorizationUnavailable,
            )),
        ))
        | PrSyncStoreError::Access(SubmissionError::Unavailable) => {
            ("PR_AUTHORIZATION_UNAVAILABLE", Some(backoff(attempt)))
        }
        PrSyncStoreError::Access(SubmissionError::Forbidden | SubmissionError::NotFound) => {
            ("PR_FORBIDDEN", None)
        }
        PrSyncStoreError::Stale
        | PrSyncStoreError::Access(SubmissionError::Context(
            ReviewContextError::Stale | ReviewContextError::StateChanged,
        )) => ("PR_CONTEXT_CHANGED", None),
        _ => ("PR_JOURNAL_CONTEXT_INVALID", None),
    }
}
