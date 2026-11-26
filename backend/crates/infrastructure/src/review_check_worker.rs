//! Bounded Check Run orchestration. Each side effect is journaled first and is
//! never repeated after uncertainty. Completion does not approve or merge.
use crate::{
    review_checks::{CheckLease, CheckStoreError, PgReviewChecks},
    review_readiness::FreshReadinessError,
};
use specspace_application::{
    book::BookError,
    comparison_reader::ComparisonReadError,
    review_check_publication::{
        CheckProviderError, CheckProviderFactory, CheckPublicationDecision as D,
        CheckPublicationPhase as P,
    },
};
use specspace_git_adapter::{
    GitError,
    checks::{CheckError, CheckPage, CheckRun, CheckState, MAX_CHECK_PAGES, ReviewCheckProvider},
};
use std::{collections::HashSet, future::Future, sync::Arc, time::Duration};
use tokio::sync::watch;
use uuid::Uuid;

#[derive(Debug, PartialEq, Eq)]
pub enum CheckWork {
    Idle,
    Completed(Uuid),
    Deferred(Uuid),
    Stopped(Uuid),
    LeaseLost(Uuid),
}
enum WorkError {
    Store(CheckStoreError),
    Provider(CheckError),
    Connection(CheckProviderError),
    Code(&'static str, Option<u32>),
}
impl From<CheckStoreError> for WorkError {
    fn from(e: CheckStoreError) -> Self {
        Self::Store(e)
    }
}
impl From<CheckError> for WorkError {
    fn from(e: CheckError) -> Self {
        Self::Provider(e)
    }
}
impl From<CheckProviderError> for WorkError {
    fn from(e: CheckProviderError) -> Self {
        Self::Connection(e)
    }
}
pub struct ReviewCheckWorker {
    journal: Arc<PgReviewChecks>,
    source: Arc<dyn CheckProviderFactory>,
    worker: Uuid,
    timeout: Duration,
}
impl ReviewCheckWorker {
    pub fn new(
        journal: Arc<PgReviewChecks>,
        source: Arc<dyn CheckProviderFactory>,
        timeout: Duration,
    ) -> Result<Self, CheckStoreError> {
        if !(Duration::from_secs(30)..=Duration::from_secs(900)).contains(&timeout) {
            return Err(CheckStoreError::Invalid);
        }
        Ok(Self {
            journal,
            source,
            worker: Uuid::new_v4(),
            timeout,
        })
    }
    pub async fn run_once(&self) -> Result<CheckWork, CheckStoreError> {
        let Some(lease) = self.journal.claim(self.worker, 120).await? else {
            return Ok(CheckWork::Idle);
        };
        match tokio::time::timeout(self.timeout, self.process(&lease)).await {
            Ok(Ok(())) => Ok(CheckWork::Completed(lease.job_id())),
            Ok(Err(error)) => self.failure(&lease, error).await,
            Err(_) => {
                self.failure(
                    &lease,
                    WorkError::Code("CHECK_WORK_TIMEOUT", Some(backoff(lease.attempts()))),
                )
                .await
            }
        }
    }
    // Only provider IO runs alongside a heartbeat. Journal operations renew at
    // their boundaries and never compete with their own DB transaction locks.
    async fn io<T, E, F>(&self, lease: &CheckLease, future: F) -> Result<T, WorkError>
    where
        F: Future<Output = Result<T, E>>,
        WorkError: From<E>,
    {
        self.journal.heartbeat(lease, 120).await?;
        tokio::pin!(future);
        let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        heartbeat.tick().await;
        loop {
            tokio::select! {
                result=&mut future=>{self.journal.heartbeat(lease,120).await?;return result.map_err(WorkError::from);},
                _=heartbeat.tick()=>self.journal.heartbeat(lease,120).await?,
            }
        }
    }
    async fn policy(&self, lease: &CheckLease) -> Result<(), WorkError> {
        self.journal.heartbeat(lease, 120).await?;
        self.journal.check(lease).await?;
        self.journal.heartbeat(lease, 120).await?;
        Ok(())
    }
    async fn process(&self, lease: &CheckLease) -> Result<(), WorkError> {
        self.journal.observe(lease).await?;
        // There are at most four forward transitions: create, adopt, update,
        // publish. Reconciliation may start at any of those durable phases.
        for _ in 0..5 {
            let view = self.journal.observe(lease).await?;
            if matches!(view.phase, P::NotStarted | P::Created(_)) {
                self.policy(lease).await?;
            }
            let provider = self
                .io(
                    lease,
                    self.source
                        .check_provider_bound(&lease.intent().source().repository),
                )
                .await?;
            self.journal.observe(lease).await?;
            let observed = self.discover(lease, provider.as_ref()).await?;
            let phase = self.journal.observe(lease).await?.phase;
            match lease.intent().decide(&observed, true, phase) {
                D::CreatePending => {
                    self.journal.heartbeat(lease, 120).await?;
                    if self.journal.start_write(lease, &observed, true).await? != P::CreateStarted {
                        return Err(WorkError::Code("CHECK_JOURNAL_PHASE_INVALID", None));
                    }
                    self.policy(lease).await?;
                    let provider = self
                        .io(
                            lease,
                            self.source
                                .check_provider_bound(&lease.intent().source().repository),
                        )
                        .await?;
                    self.policy(lease).await?;
                    let receipt = self
                        .io(
                            lease,
                            provider.create_check(
                                &lease.intent().scope(),
                                lease.intent().check_operation(),
                            ),
                        )
                        .await?;
                    if !exact(lease, &receipt) || receipt.state != CheckState::Pending {
                        return Err(WorkError::Code(
                            "CHECK_WRITE_RECEIPT_INVALID",
                            Some(backoff(lease.attempts())),
                        ));
                    }
                    self.journal.adopt(lease, &receipt).await?;
                }
                D::RecordPending(id) => {
                    let receipt = observed
                        .iter()
                        .find(|r| r.id == id)
                        .ok_or(WorkError::Code("CHECK_DISCOVERY_INVALID", None))?;
                    self.journal.adopt(lease, receipt).await?;
                }
                D::Update(id, result) => {
                    self.journal.heartbeat(lease, 120).await?;
                    if self.journal.start_write(lease, &observed, true).await?
                        != P::UpdateStarted(id)
                    {
                        return Err(WorkError::Code("CHECK_JOURNAL_PHASE_INVALID", None));
                    }
                    self.policy(lease).await?;
                    let provider = self
                        .io(
                            lease,
                            self.source
                                .check_provider_bound(&lease.intent().source().repository),
                        )
                        .await?;
                    self.policy(lease).await?;
                    let receipt = self
                        .io(
                            lease,
                            provider.update_check(
                                &lease.intent().scope(),
                                id,
                                lease.intent().check_operation(),
                                result,
                            ),
                        )
                        .await?;
                    if !exact(lease, &receipt)
                        || receipt.id != id
                        || receipt.state != CheckState::from(result)
                    {
                        return Err(WorkError::Code(
                            "CHECK_WRITE_RECEIPT_INVALID",
                            Some(backoff(lease.attempts())),
                        ));
                    }
                }
                D::Published(_) => {
                    self.journal.heartbeat(lease, 120).await?;
                    self.journal.complete(lease, &observed, true).await?;
                    return Ok(());
                }
                D::ObserveAgain => {
                    return Err(WorkError::Code(
                        "CHECK_OBSERVATION_PENDING",
                        Some(backoff(lease.attempts())),
                    ));
                }
                D::Conflict => return Err(WorkError::Code("CHECK_IDENTITY_CONFLICT", None)),
            }
        }
        Err(WorkError::Code(
            "CHECK_PROGRESS_UNSETTLED",
            Some(backoff(lease.attempts())),
        ))
    }
    async fn discover(
        &self,
        lease: &CheckLease,
        provider: &dyn ReviewCheckProvider,
    ) -> Result<Vec<CheckRun>, WorkError> {
        let scope = lease.intent().scope();
        let mut page = CheckPage::new(1, 100).expect("fixed bounded page");
        let mut total = None;
        let mut ids = HashSet::new();
        let mut rows = Vec::new();
        loop {
            self.journal.observe(lease).await?;
            let listing = self.io(lease, provider.check_runs(&scope, page)).await?;
            self.journal.observe(lease).await?;
            if listing.total_count > u64::from(MAX_CHECK_PAGES) * 100 {
                return Err(WorkError::Code("CHECK_DISCOVERY_TOO_LARGE", None));
            }
            if total.is_some_and(|old| old != listing.total_count) {
                return Err(WorkError::Code(
                    "CHECK_DISCOVERY_UNSETTLED",
                    Some(backoff(lease.attempts())),
                ));
            }
            total = Some(listing.total_count);
            if listing.items.len() > usize::from(page.limit()) {
                return Err(WorkError::Code("CHECK_DISCOVERY_INVALID", None));
            }
            for row in listing.items {
                if row.scope != scope || row.operation.is_nil() || !ids.insert(row.id) {
                    return Err(WorkError::Code("CHECK_IDENTITY_CONFLICT", None));
                }
                rows.push(row);
            }
            if rows.len() as u64 > listing.total_count {
                return Err(WorkError::Code("CHECK_DISCOVERY_INVALID", None));
            }
            match listing.next {
                Some(next)
                    if page.page() < MAX_CHECK_PAGES
                        && next.page() == page.page() + 1
                        && next.limit() == page.limit()
                        && (rows.len() as u64) < listing.total_count =>
                {
                    page = next
                }
                Some(_) => return Err(WorkError::Code("CHECK_DISCOVERY_INVALID", None)),
                None if rows.len() as u64 == listing.total_count => break,
                None => {
                    return Err(WorkError::Code(
                        "CHECK_DISCOVERY_UNSETTLED",
                        Some(backoff(lease.attempts())),
                    ));
                }
            }
        }
        let owned: Vec<_> = rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.operation == lease.intent().check_operation())
            .map(|(n, _)| n)
            .collect();
        if owned.len() > 1 {
            return Err(WorkError::Code("CHECK_IDENTITY_CONFLICT", None));
        }
        if let Some(index) = owned.first().copied() {
            let id = rows[index].id;
            let read = self.io(lease, provider.check_run(&scope, id)).await?;
            self.journal.observe(lease).await?;
            if !exact(lease, &read) || read.id != id {
                return Err(WorkError::Code("CHECK_IDENTITY_CONFLICT", None));
            }
            if read.state != rows[index].state {
                return Err(WorkError::Code(
                    "CHECK_DISCOVERY_UNSETTLED",
                    Some(backoff(lease.attempts())),
                ));
            }
            rows[index] = read;
        }
        Ok(rows)
    }
    async fn failure(
        &self,
        lease: &CheckLease,
        error: WorkError,
    ) -> Result<CheckWork, CheckStoreError> {
        let (code, retry, cooldown) = match error {
            WorkError::Store(CheckStoreError::Unavailable) => {
                return Err(CheckStoreError::Unavailable);
            }
            WorkError::Store(CheckStoreError::LeaseLost) => {
                return Ok(CheckWork::LeaseLost(lease.job_id()));
            }
            WorkError::Store(CheckStoreError::Cooldown(n)) => {
                ("CHECK_PROVIDER_COOLDOWN", Some(n), None)
            }
            WorkError::Store(CheckStoreError::Fresh(e)) => fresh(e, lease.attempts()),
            WorkError::Store(CheckStoreError::Busy) => {
                ("CHECK_CONTEXT_BUSY", Some(backoff(lease.attempts())), None)
            }
            WorkError::Store(CheckStoreError::Forbidden | CheckStoreError::NotFound) => {
                ("CHECK_AUTHORITY_LOST", None, None)
            }
            WorkError::Store(CheckStoreError::Stale) => ("CHECK_CONTEXT_CHANGED", None, None),
            WorkError::Store(CheckStoreError::NotConfigured) => {
                ("CHECK_SETUP_REQUIRED", None, None)
            }
            WorkError::Store(_) => ("CHECK_JOURNAL_INVALID", None, None),
            WorkError::Provider(CheckError::Provider(e)) => provider(e, lease.attempts()),
            WorkError::Provider(CheckError::NotFound) => (
                "CHECK_OBSERVATION_PENDING",
                Some(backoff(lease.attempts())),
                None,
            ),
            WorkError::Provider(_) => ("CHECK_PROVIDER_EVIDENCE_INVALID", None, None),
            WorkError::Connection(CheckProviderError::BindingChanged) => {
                ("CHECK_BINDING_CHANGED", None, None)
            }
            WorkError::Connection(CheckProviderError::Source(e)) => book(e, lease.attempts()),
            WorkError::Code(code, retry) => (code, retry, None),
        };
        match self.journal.defer(lease, code, retry, cooldown).await {
            Ok(true) => Ok(CheckWork::Deferred(lease.job_id())),
            Ok(false) => Ok(CheckWork::Stopped(lease.job_id())),
            Err(CheckStoreError::LeaseLost) => Ok(CheckWork::LeaseLost(lease.job_id())),
            Err(e) => Err(e),
        }
    }
    pub async fn run(&self, mut stop: watch::Receiver<bool>) {
        loop {
            if *stop.borrow() {
                break;
            }
            let result = tokio::select! {biased;_=stop.wait_for(|s|*s)=>break,result=self.run_once()=>result};
            match result {
                Ok(CheckWork::Idle) => {}
                Ok(work) => tracing::info!(?work, "review check work completed"),
                Err(error) => tracing::warn!(?error, "review check journal unavailable"),
            }
            tokio::select! {_=stop.wait_for(|s|*s)=>break,_=tokio::time::sleep(Duration::from_secs(2))=>{}}
        }
    }
}
fn exact(lease: &CheckLease, run: &CheckRun) -> bool {
    run.scope == lease.intent().scope() && run.operation == lease.intent().check_operation()
}
fn backoff(attempt: i32) -> u32 {
    2u32.saturating_pow(attempt.clamp(1, 6) as u32).min(60)
}
type Failure = (&'static str, Option<u32>, Option<u32>);
fn provider(error: GitError, attempt: i32) -> Failure {
    match error {
        GitError::RateLimited {
            retry_after_seconds,
        } => delay("CHECK_PROVIDER_RATE_LIMITED", retry_after_seconds, attempt),
        GitError::ResultUnknownWithRetryAfter {
            retry_after_seconds,
        } => delay("CHECK_RESULT_UNKNOWN", Some(retry_after_seconds), attempt),
        GitError::ResultUnknown => ("CHECK_RESULT_UNKNOWN", Some(backoff(attempt)), None),
        GitError::ProviderUnavailable => {
            ("CHECK_PROVIDER_UNAVAILABLE", Some(backoff(attempt)), None)
        }
        GitError::Unauthorized => ("CHECK_PROVIDER_UNAUTHORIZED", None, None),
        _ => ("CHECK_PROVIDER_REJECTED", None, None),
    }
}
fn delay(code: &'static str, seconds: Option<u64>, attempt: i32) -> Failure {
    match u32::try_from(seconds.unwrap_or(u64::from(backoff(attempt))).max(1)) {
        Ok(n) => (code, Some(n), Some(n)),
        Err(_) => ("CHECK_PROVIDER_BACKOFF_TOO_LARGE", None, Some(u32::MAX)),
    }
}
fn book(error: BookError, attempt: i32) -> Failure {
    match error {
        BookError::Provider(e) => provider(e, attempt),
        BookError::AuthorizationUnavailable | BookError::ProcessingUnavailable => {
            ("CHECK_CONNECTION_UNAVAILABLE", Some(backoff(attempt)), None)
        }
        _ => ("CHECK_SETUP_REQUIRED", None, None),
    }
}
fn fresh(error: FreshReadinessError, attempt: i32) -> Failure {
    match error {
        FreshReadinessError::Provider(e) => provider(e, attempt),
        FreshReadinessError::Connection(ComparisonReadError::Provider(e)) => provider(e, attempt),
        FreshReadinessError::Connection(ComparisonReadError::Source(e)) => book(e, attempt),
        FreshReadinessError::Busy | FreshReadinessError::TimedOut => {
            ("CHECK_POLICY_UNAVAILABLE", Some(backoff(attempt)), None)
        }
        FreshReadinessError::Stored(crate::review_readiness::ReadinessError::Unavailable) => {
            ("CHECK_POLICY_UNAVAILABLE", Some(backoff(attempt)), None)
        }
        _ => ("CHECK_CONTEXT_CHANGED", None, None),
    }
}
