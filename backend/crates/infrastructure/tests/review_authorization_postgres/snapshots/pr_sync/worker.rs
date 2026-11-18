use super::*;
use specspace_application::pr_sync::{PrProviderError, PrProviderFactory, PrSyncIntent};
use specspace_git_adapter::pull_requests::{
    PullRequest, PullRequestContent, PullRequestError, PullRequestListing, PullRequestNumber,
    PullRequestPage, PullRequestProvider, PullRequestScope, PullRequestState,
};
use specspace_infrastructure::{
    pr_sync::PrSyncResolution,
    pr_sync_worker::{PrSyncWork, PrSyncWorker},
};

fn copy(pr: &PullRequest) -> PullRequest {
    PullRequest {
        id: pr.id,
        node_id: pr.node_id.clone(),
        number: pr.number,
        repository_id: pr.repository_id,
        branch: pr.branch.clone(),
        head: pr.head.clone(),
        target: pr.target.clone(),
        state: pr.state,
        draft: pr.draft,
        title: pr.title.clone(),
        body: pr.body.clone(),
        url: pr.url.clone(),
    }
}
fn record(intent: &PrSyncIntent) -> PullRequest {
    let s = intent.source();
    PullRequest {
        id: 77,
        node_id: "PR_SYNTHETIC_9".into(),
        number: PullRequestNumber::new(9).unwrap(),
        repository_id: s.repository.remote_id,
        branch: s.branch.clone(),
        head: s.head.clone(),
        target: s.target.clone(),
        state: PullRequestState::Open,
        draft: true,
        title: intent.content().title().into(),
        body: intent.content().body().into(),
        url: "https://github.com/synthetic/specs/pull/9".into(),
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn saved_review_publication_dispatches_and_synchronizes_a_draft_without_manual_admission(
    pool: PgPool,
) {
    let (f, source, snapshot, event) = publication(&pool).await;
    let journal = Arc::new(PgPrSync::new(pool.clone()));
    let dispatch =
        specspace_infrastructure::pr_sync::PrSyncDispatcher::new(journal.clone(), site());
    let specspace_infrastructure::pr_sync::PrDispatchWork::Enqueued(e, job) =
        dispatch.run_once().await.unwrap()
    else {
        panic!("expected automatic admission");
    };
    assert_eq!(e, event);
    let prs = Arc::new(Provider::new(pool.clone()));
    let factory = Arc::new(WorkerFactory {
        source,
        prs: prs.clone(),
        repository: f.repository,
    });
    let worker =
        PrSyncWorker::new(journal, factory.clone(), factory, Duration::from_secs(30)).unwrap();
    assert_eq!(
        worker.run_once().await.unwrap(),
        PrSyncWork::Completed(job, PrSyncResolution::Synced)
    );
    assert_eq!(
        (
            prs.creates.load(Ordering::SeqCst),
            prs.updates.load(Ordering::SeqCst)
        ),
        (1, 0)
    );
    assert_eq!(receipts(&pool).await, (1, 1, 1));
    let current: (String, Option<Uuid>) =
        sqlx::query_as("SELECT state,active_review_snapshot_id FROM proposals WHERE id=$1")
            .bind(f.proposal)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(current, ("IN_REVIEW".into(), Some(snapshot.snapshot_id)));
}
struct Provider {
    pool: PgPool,
    pr: Mutex<Option<PullRequest>>,
    lists: AtomicUsize,
    gets: AtomicUsize,
    creates: AtomicUsize,
    updates: AtomicUsize,
    list_mode: AtomicUsize,
    get_mode: AtomicUsize,
    write_mode: AtomicUsize,
    hook: Mutex<Option<(&'static str, &'static str, Uuid)>>,
    lease_to_check: Mutex<Option<Uuid>>,
    gate: AtomicUsize,
    started: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
impl Provider {
    fn new(pool: PgPool) -> Self {
        Self {
            pool,
            pr: Mutex::new(None),
            lists: AtomicUsize::new(0),
            gets: AtomicUsize::new(0),
            creates: AtomicUsize::new(0),
            updates: AtomicUsize::new(0),
            list_mode: AtomicUsize::new(0),
            get_mode: AtomicUsize::new(0),
            write_mode: AtomicUsize::new(0),
            hook: Mutex::new(None),
            lease_to_check: Mutex::new(None),
            gate: AtomicUsize::new(0),
            started: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        }
    }
    async fn hook(&self, stage: &str) {
        let hook = {
            let mut h = self.hook.lock().unwrap();
            if h.as_ref().is_some_and(|h| h.0 == stage) {
                h.take()
            } else {
                None
            }
        };
        if let Some((_, sql, id)) = hook {
            sqlx::query(sql).bind(id).execute(&self.pool).await.unwrap();
        }
    }
    async fn gate(&self, stage: usize) {
        if self.gate.load(Ordering::SeqCst) == stage {
            self.started.notify_one();
            self.release.notified().await;
        }
    }
    fn writes(&self) -> usize {
        self.creates.load(Ordering::SeqCst) + self.updates.load(Ordering::SeqCst)
    }
}
#[async_trait]
impl PullRequestProvider for Provider {
    async fn pull_requests(
        &self,
        scope: &PullRequestScope,
        page: PullRequestPage,
    ) -> Result<PullRequestListing, PullRequestError> {
        assert_eq!(scope.repository_id(), 42);
        assert_eq!((page.page(), page.limit()), (1, 2));
        self.lists.fetch_add(1, Ordering::SeqCst);
        let job = *self.lease_to_check.lock().unwrap();
        if let Some(job) = job {
            let renewed: bool = sqlx::query_scalar(
                "SELECT leased_until>clock_timestamp()+interval '30 seconds' FROM jobs WHERE id=$1",
            )
            .bind(job)
            .fetch_one(&self.pool)
            .await
            .unwrap();
            assert!(
                renewed,
                "the preceding provider return must renew the shared lease window"
            );
        }
        self.gate(1).await;
        let mode = self.list_mode.load(Ordering::SeqCst);
        let mut items = if mode == 1 {
            vec![]
        } else {
            self.pr
                .lock()
                .unwrap()
                .as_ref()
                .map(copy)
                .into_iter()
                .collect::<Vec<_>>()
        };
        if mode == 2
            && let Some(pr) = items.first()
        {
            let mut other = copy(pr);
            other.id += 1;
            other.number = PullRequestNumber::new(10).unwrap();
            items.push(other);
        }
        self.hook("list").await;
        Ok(PullRequestListing {
            items,
            next: if mode == 3 {
                Some(PullRequestPage::new(2, 2).unwrap())
            } else {
                None
            },
        })
    }
    async fn pull_request(
        &self,
        scope: &PullRequestScope,
        number: PullRequestNumber,
    ) -> Result<PullRequest, PullRequestError> {
        assert_eq!(scope.repository_id(), 42);
        self.gets.fetch_add(1, Ordering::SeqCst);
        self.gate(3).await;
        if self.get_mode.load(Ordering::SeqCst) == 1 {
            return Err(PullRequestError::NotFound);
        }
        let result = self
            .pr
            .lock()
            .unwrap()
            .as_ref()
            .filter(|p| p.number == number)
            .map(copy)
            .ok_or(PullRequestError::NotFound);
        self.hook("get").await;
        result
    }
    async fn create_pull_request(
        &self,
        scope: &PullRequestScope,
        content: &PullRequestContent,
    ) -> Result<PullRequest, PullRequestError> {
        self.creates.fetch_add(1, Ordering::SeqCst);
        if self.write_mode.load(Ordering::SeqCst) == 3 {
            return Err(PullRequestError::Provider(GitError::ProviderRejected));
        }
        let pr = PullRequest {
            id: 77,
            node_id: "PR_SYNTHETIC_9".into(),
            number: PullRequestNumber::new(9).unwrap(),
            repository_id: scope.repository_id(),
            branch: scope.branch().clone(),
            head: sha(1),
            target: sha(2),
            state: PullRequestState::Open,
            draft: true,
            title: content.title().into(),
            body: content.body().into(),
            url: "https://github.com/synthetic/specs/pull/9".into(),
        };
        if self.write_mode.load(Ordering::SeqCst) != 2 {
            *self.pr.lock().unwrap() = Some(copy(&pr));
        }
        self.hook("create").await;
        self.gate(2).await;
        match self.write_mode.load(Ordering::SeqCst) {
            1 | 2 => Err(PullRequestError::Provider(GitError::ResultUnknown)),
            4 => {
                let mut bad = pr;
                bad.body.push_str("invalid reply");
                Ok(bad)
            }
            _ => Ok(pr),
        }
    }
    async fn update_pull_request(
        &self,
        _: &PullRequestScope,
        number: PullRequestNumber,
        content: &PullRequestContent,
    ) -> Result<PullRequest, PullRequestError> {
        self.updates.fetch_add(1, Ordering::SeqCst);
        let pr = {
            let mut stored = self.pr.lock().unwrap();
            let p = stored.as_mut().ok_or(PullRequestError::NotFound)?;
            assert_eq!(number, p.number);
            p.title = content.title().into();
            p.body = content.body().into();
            copy(p)
        };
        self.hook("update").await;
        if self.write_mode.load(Ordering::SeqCst) == 1 {
            Err(PullRequestError::Provider(GitError::ResultUnknown))
        } else {
            Ok(pr)
        }
    }
}
struct WorkerFactory {
    source: Arc<Source>,
    prs: Arc<Provider>,
    repository: Uuid,
}
#[async_trait]
impl BoundProviderFactory for WorkerFactory {
    async fn provider_bound(
        &self,
        binding: &RepositoryBinding,
    ) -> Result<Arc<dyn GitProvider>, ComparisonReadError> {
        assert_eq!(binding.repository, self.repository);
        assert_eq!((binding.remote_id, binding.installation_id), (42, 7));
        Ok(self.source.clone())
    }
}
#[async_trait]
impl PrProviderFactory for WorkerFactory {
    async fn pr_provider_bound(
        &self,
        binding: &RepositoryBinding,
    ) -> Result<Arc<dyn PullRequestProvider>, PrProviderError> {
        assert_eq!(binding.repository, self.repository);
        assert_eq!((binding.remote_id, binding.installation_id), (42, 7));
        self.prs.hook("connect").await;
        Ok(self.prs.clone())
    }
}
struct Harness {
    f: Fixture,
    source: Arc<Source>,
    prs: Arc<Provider>,
    factory: Arc<WorkerFactory>,
    job: Uuid,
    intent: PrSyncIntent,
}
impl Harness {
    fn worker(&self, pool: &PgPool) -> Arc<PrSyncWorker> {
        Arc::new(
            PrSyncWorker::new(
                Arc::new(PgPrSync::new(pool.clone())),
                self.factory.clone(),
                self.factory.clone(),
                Duration::from_secs(30),
            )
            .unwrap(),
        )
    }
}
async fn harness(pool: &PgPool) -> Harness {
    let (f, source, _, event) = publication(pool).await;
    let receipt = PgPrSync::new(pool.clone())
        .reserve(f.owner, f.proposal, event, &site())
        .await
        .unwrap();
    let intent = receipt.intent().clone();
    let prs = Arc::new(Provider::new(pool.clone()));
    let factory = Arc::new(WorkerFactory {
        source: source.clone(),
        prs: prs.clone(),
        repository: f.repository,
    });
    Harness {
        f,
        source,
        prs,
        factory,
        job: receipt.job_id,
        intent,
    }
}
async fn due(pool: &PgPool, job: Uuid) {
    sqlx::query("UPDATE jobs SET available_at=clock_timestamp()-interval '1 second' WHERE id=$1")
        .bind(job)
        .execute(pool)
        .await
        .unwrap();
}
async fn state(pool: &PgPool, job: Uuid) -> (String, String) {
    sqlx::query_as("SELECT o.state,j.state FROM jobs j JOIN git_operations o ON o.id=j.operation_id WHERE j.id=$1").bind(job).fetch_one(pool).await.unwrap()
}
async fn receipts(pool: &PgPool) -> (i64, i64, i64) {
    sqlx::query_as("SELECT (SELECT count(*) FROM pr_sync_writes),(SELECT count(*) FROM proposal_pull_requests),(SELECT count(*) FROM pr_sync_results)").fetch_one(pool).await.unwrap()
}
async fn next_job(pool: &PgPool, h: &Harness) -> Uuid {
    let reader = Arc::new(
        ComparisonReader::new(
            Arc::new(PgComparisonAuthorization::new(pool.clone())),
            h.factory.clone(),
            Duration::from_secs(30),
        )
        .unwrap(),
    );
    let preparer = ReviewPreparer::new(
        Arc::new(PgReviewAuthorization::new(pool.clone())),
        reader,
        h.factory.clone(),
        Duration::from_secs(30),
    )
    .unwrap();
    let (lease, verified) = prepared(pool, &h.f, &preparer, 3).await;
    PgReviewSnapshots::new(pool.clone())
        .publish(&lease, &verified, &preparer)
        .await
        .unwrap();
    let event:Uuid=sqlx::query_scalar("SELECT id FROM outbox WHERE event_type='REVIEW_SNAPSHOT_PUBLISHED' AND entity_id=$1 AND payload->>'operation_id'=$2").bind(h.f.proposal).bind(lease.operation_id().to_string()).fetch_one(pool).await.unwrap();
    PgPrSync::new(pool.clone())
        .reserve(h.f.owner, h.f.proposal, event, &site())
        .await
        .unwrap()
        .job_id
}

#[sqlx::test(migrations = "../../migrations")]
async fn new_projection_updates_the_bound_pr_and_lost_update_replies_only_trigger_reads(
    pool: PgPool,
) {
    for lose_reply in [false, true] {
        let h = harness(&pool).await;
        h.worker(&pool).run_once().await.unwrap();
        let second = next_job(&pool, &h).await;
        if lose_reply {
            h.prs.write_mode.store(1, Ordering::SeqCst);
        }
        let result = h.worker(&pool).run_once().await.unwrap();
        if lose_reply {
            assert_eq!(result, PrSyncWork::Deferred(second));
            due(&pool, second).await;
            assert_eq!(
                h.worker(&pool).run_once().await.unwrap(),
                PrSyncWork::Completed(second, PrSyncResolution::Synced)
            );
        } else {
            assert_eq!(
                result,
                PrSyncWork::Completed(second, PrSyncResolution::Synced)
            );
        }
        assert_eq!(
            (
                h.prs.creates.load(Ordering::SeqCst),
                h.prs.updates.load(Ordering::SeqCst)
            ),
            (1, 1)
        );
        let identity: (i64, String) = sqlx::query_as(
            "SELECT pr_number,provider_id FROM proposal_pull_requests WHERE proposal_id=$1",
        )
        .bind(h.f.proposal)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(identity, (9, "77".into()));
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn missing_bound_pr_never_authorizes_replacement_creation(pool: PgPool) {
    let h = harness(&pool).await;
    h.worker(&pool).run_once().await.unwrap();
    let second = next_job(&pool, &h).await;
    *h.prs.pr.lock().unwrap() = None;
    assert_eq!(
        h.worker(&pool).run_once().await.unwrap(),
        PrSyncWork::Deferred(second)
    );
    assert_eq!(
        (
            h.prs.creates.load(Ordering::SeqCst),
            h.prs.updates.load(Ordering::SeqCst)
        ),
        (1, 0)
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn successive_short_provider_calls_renew_the_lease_between_boundaries(pool: PgPool) {
    let h = harness(&pool).await;
    *h.prs.hook.lock().unwrap() = Some((
        "connect",
        "UPDATE jobs SET leased_until=clock_timestamp()+interval '5 seconds' WHERE id=$1",
        h.job,
    ));
    *h.prs.lease_to_check.lock().unwrap() = Some(h.job);
    assert_eq!(
        h.worker(&pool).run_once().await.unwrap(),
        PrSyncWork::Completed(h.job, PrSyncResolution::Synced)
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn worker_creates_one_draft_verifies_refs_and_publishes_an_exact_receipt(pool: PgPool) {
    let h = harness(&pool).await;
    let worker = h.worker(&pool);
    assert_eq!(
        worker.run_once().await.unwrap(),
        PrSyncWork::Completed(h.job, PrSyncResolution::Synced)
    );
    assert_eq!(
        (
            h.prs.creates.load(Ordering::SeqCst),
            h.prs.updates.load(Ordering::SeqCst)
        ),
        (1, 0)
    );
    assert_eq!(receipts(&pool).await, (1, 1, 1));
    assert_eq!(
        state(&pool, h.job).await,
        ("SUCCEEDED".into(), "SUCCEEDED".into())
    );
    assert_eq!(worker.run_once().await.unwrap(), PrSyncWork::Idle);
    assert_eq!(h.prs.lists.load(Ordering::SeqCst), 2);
    assert_eq!(h.prs.gets.load(Ordering::SeqCst), 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn lost_create_reply_reconciles_by_reads_without_a_second_write(pool: PgPool) {
    let h = harness(&pool).await;
    h.prs.write_mode.store(1, Ordering::SeqCst);
    assert_eq!(
        h.worker(&pool).run_once().await.unwrap(),
        PrSyncWork::Deferred(h.job)
    );
    assert_eq!(
        state(&pool, h.job).await,
        ("UNKNOWN".into(), "QUEUED".into())
    );
    due(&pool, h.job).await;
    assert_eq!(
        h.worker(&pool).run_once().await.unwrap(),
        PrSyncWork::Completed(h.job, PrSyncResolution::Synced)
    );
    assert_eq!(h.prs.writes(), 1);
    assert_eq!(receipts(&pool).await, (1, 1, 1));
}

#[sqlx::test(migrations = "../../migrations")]
async fn unknown_create_and_eventually_empty_discovery_never_authorize_another_create(
    pool: PgPool,
) {
    let h = harness(&pool).await;
    h.prs.write_mode.store(2, Ordering::SeqCst);
    assert_eq!(
        h.worker(&pool).run_once().await.unwrap(),
        PrSyncWork::Deferred(h.job)
    );
    due(&pool, h.job).await;
    assert_eq!(
        h.worker(&pool).run_once().await.unwrap(),
        PrSyncWork::Deferred(h.job)
    );
    assert_eq!(h.prs.writes(), 1);
    assert_eq!(receipts(&pool).await, (1, 0, 0));
}

#[sqlx::test(migrations = "../../migrations")]
async fn positive_write_receipt_can_be_verified_even_while_the_list_is_eventually_empty(
    pool: PgPool,
) {
    let h = harness(&pool).await;
    h.prs.list_mode.store(1, Ordering::SeqCst);
    assert_eq!(
        h.worker(&pool).run_once().await.unwrap(),
        PrSyncWork::Completed(h.job, PrSyncResolution::Synced)
    );
    assert_eq!(h.prs.writes(), 1);
    assert_eq!(h.prs.gets.load(Ordering::SeqCst), 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn list_presence_followed_by_get_404_is_not_proof_of_absence(pool: PgPool) {
    let h = harness(&pool).await;
    *h.prs.pr.lock().unwrap() = Some(record(&h.intent));
    h.prs.get_mode.store(1, Ordering::SeqCst);
    assert_eq!(
        h.worker(&pool).run_once().await.unwrap(),
        PrSyncWork::Deferred(h.job)
    );
    assert_eq!(h.prs.writes(), 0);
    assert_eq!(receipts(&pool).await, (0, 0, 0));
}

#[sqlx::test(migrations = "../../migrations")]
async fn conflicting_foreign_or_incomplete_discovery_stops_without_writing(pool: PgPool) {
    for mode in 0..4 {
        let h = harness(&pool).await;
        let mut pr = record(&h.intent);
        match mode {
            0 => h.prs.list_mode.store(2, Ordering::SeqCst),
            1 => h.prs.list_mode.store(3, Ordering::SeqCst),
            2 => pr.body = "Unmanaged human PR".into(),
            _ => pr.branch = BranchName::parse("change/foreign").unwrap(),
        }
        *h.prs.pr.lock().unwrap() = Some(pr);
        assert_eq!(
            h.worker(&pool).run_once().await.unwrap(),
            PrSyncWork::Stopped(h.job)
        );
        assert_eq!(h.prs.writes(), 0);
        assert_eq!(receipts(&pool).await, (0, 0, 0));
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn current_provider_head_and_main_are_checked_before_the_write_boundary(pool: PgPool) {
    for head_changed in [false, true] {
        let h = harness(&pool).await;
        if head_changed {
            h.source.branch_head.store(9, Ordering::SeqCst);
        } else {
            h.source.target.store(9, Ordering::SeqCst);
        }
        assert_eq!(
            h.worker(&pool).run_once().await.unwrap(),
            PrSyncWork::Stopped(h.job)
        );
        assert_eq!(h.prs.writes(), 0);
        assert_eq!(receipts(&pool).await, (0, 0, 0));
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn connecting_or_discovering_with_revoked_scope_never_reaches_a_write(pool: PgPool) {
    for stage in ["connect", "list"] {
        let h = harness(&pool).await;
        *h.prs.hook.lock().unwrap() = Some((
            stage,
            "UPDATE users SET status='DISABLED' WHERE id=$1",
            h.f.owner,
        ));
        assert_eq!(
            h.worker(&pool).run_once().await.unwrap(),
            PrSyncWork::Stopped(h.job)
        );
        assert_eq!(h.prs.writes(), 0);
        assert_eq!(receipts(&pool).await, (0, 0, 0));
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn revocation_after_a_write_retains_uncertainty_and_never_publishes_a_binding(pool: PgPool) {
    let h = harness(&pool).await;
    *h.prs.hook.lock().unwrap() = Some((
        "create",
        "UPDATE users SET status='DISABLED' WHERE id=$1",
        h.f.owner,
    ));
    assert_eq!(
        h.worker(&pool).run_once().await.unwrap(),
        PrSyncWork::Stopped(h.job)
    );
    assert_eq!(h.prs.writes(), 1);
    assert_eq!(
        state(&pool, h.job).await,
        ("UNKNOWN".into(), "FAILED".into())
    );
    assert_eq!(receipts(&pool).await, (1, 0, 0));
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_local_change_after_write_resolves_the_old_projection_as_superseded(pool: PgPool) {
    let h = harness(&pool).await;
    *h.prs.hook.lock().unwrap() = Some((
        "create",
        "UPDATE proposals SET description='Changed after provider write',version=version+1 WHERE id=$1",
        h.f.proposal,
    ));
    assert_eq!(
        h.worker(&pool).run_once().await.unwrap(),
        PrSyncWork::Completed(h.job, PrSyncResolution::Superseded)
    );
    assert_eq!(h.prs.writes(), 1);
    assert_eq!(
        state(&pool, h.job).await,
        ("SUCCEEDED".into(), "CANCELLED".into())
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn provider_ref_disagreement_after_write_is_observed_again_before_any_success(pool: PgPool) {
    let h = harness(&pool).await;
    h.prs.write_mode.store(1, Ordering::SeqCst);
    h.worker(&pool).run_once().await.unwrap();
    h.source.target.store(9, Ordering::SeqCst);
    due(&pool, h.job).await;
    assert_eq!(
        h.worker(&pool).run_once().await.unwrap(),
        PrSyncWork::Deferred(h.job)
    );
    assert_eq!(receipts(&pool).await, (1, 0, 0));
    h.prs.pr.lock().unwrap().as_mut().unwrap().target = sha(9);
    due(&pool, h.job).await;
    assert_eq!(
        h.worker(&pool).run_once().await.unwrap(),
        PrSyncWork::Completed(h.job, PrSyncResolution::Superseded)
    );
    assert_eq!(h.prs.writes(), 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn provider_backoff_is_preserved_and_no_write_occurs_during_the_delay(pool: PgPool) {
    let h = harness(&pool).await;
    *h.source.failure.lock().unwrap() = Some(GitError::RateLimited {
        retry_after_seconds: Some(37),
    });
    assert_eq!(
        h.worker(&pool).run_once().await.unwrap(),
        PrSyncWork::Deferred(h.job)
    );
    let delayed: bool = sqlx::query_scalar(
        "SELECT available_at>clock_timestamp()+interval '30 seconds' FROM jobs WHERE id=$1",
    )
    .bind(h.job)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(delayed);
    assert_eq!(h.worker(&pool).run_once().await.unwrap(), PrSyncWork::Idle);
    assert_eq!(h.prs.writes(), 0);
    *h.source.failure.lock().unwrap() = None;
    due(&pool, h.job).await;
    assert_eq!(
        h.worker(&pool).run_once().await.unwrap(),
        PrSyncWork::Completed(h.job, PrSyncResolution::Synced)
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn malformed_write_reply_reconciles_the_real_exact_metadata_without_repeating_the_write(
    pool: PgPool,
) {
    let h = harness(&pool).await;
    h.prs.write_mode.store(4, Ordering::SeqCst);
    assert_eq!(
        h.worker(&pool).run_once().await.unwrap(),
        PrSyncWork::Deferred(h.job)
    );
    due(&pool, h.job).await;
    assert_eq!(
        h.worker(&pool).run_once().await.unwrap(),
        PrSyncWork::Completed(h.job, PrSyncResolution::Synced)
    );
    assert_eq!(h.prs.writes(), 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn shutdown_during_creation_recovers_through_read_only_observation(pool: PgPool) {
    let h = harness(&pool).await;
    h.prs.gate.store(2, Ordering::SeqCst);
    let worker = h.worker(&pool);
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(async move { worker.run(stopped).await });
    tokio::time::timeout(Duration::from_secs(10), h.prs.started.notified())
        .await
        .unwrap();
    stop.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(h.prs.writes(), 1);
    assert_eq!(
        state(&pool, h.job).await,
        ("RUNNING".into(), "RUNNING".into())
    );
    sqlx::query("UPDATE jobs SET leased_until=clock_timestamp()-interval '1 second' WHERE id=$1")
        .bind(h.job)
        .execute(&pool)
        .await
        .unwrap();
    h.prs.gate.store(0, Ordering::SeqCst);
    assert_eq!(
        h.worker(&pool).run_once().await.unwrap(),
        PrSyncWork::Completed(h.job, PrSyncResolution::Synced)
    );
    assert_eq!(h.prs.writes(), 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn long_provider_reads_renew_the_lease_without_restarting_or_writing_early(pool: PgPool) {
    let h = harness(&pool).await;
    // Snapshot preparation may have left one notification permit behind.
    let _ = tokio::time::timeout(Duration::from_millis(5), h.source.started.notified()).await;
    h.source.blocked.store(true, Ordering::SeqCst);
    let worker = h.worker(&pool);
    let task = tokio::spawn(async move { worker.run_once().await });
    tokio::time::timeout(Duration::from_secs(5), h.source.started.notified())
        .await
        .unwrap();
    let before: sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc> =
        sqlx::query_scalar("SELECT heartbeat_at FROM jobs WHERE id=$1")
            .bind(h.job)
            .fetch_one(&pool)
            .await
            .unwrap();
    tokio::time::sleep(Duration::from_secs(16)).await;
    let renewed: bool = sqlx::query_scalar("SELECT heartbeat_at>$2 FROM jobs WHERE id=$1")
        .bind(h.job)
        .bind(before)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(renewed);
    assert_eq!(h.prs.writes(), 0);
    h.source.blocked.store(false, Ordering::SeqCst);
    h.source.release.notify_one();
    assert_eq!(
        task.await.unwrap().unwrap(),
        PrSyncWork::Completed(h.job, PrSyncResolution::Synced)
    );
}
