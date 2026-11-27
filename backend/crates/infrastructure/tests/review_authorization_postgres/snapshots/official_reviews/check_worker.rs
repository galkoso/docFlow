//! Provider doubles return typed, validated receipts; HTTP parsing is covered by
//! the adapter tests. These cases exercise real journal/lease transactions.
use super::*;
use specspace_application::review_check_publication::{CheckProviderError, CheckProviderFactory};
use specspace_git_adapter::checks::*;
use specspace_infrastructure::{
    review_check_worker::{CheckWork, ReviewCheckWorker},
    review_checks::PgReviewChecks,
};

#[path = "check_worker/dispatch.rs"]
mod dispatch;
#[path = "check_worker/invalidation.rs"]
mod invalidation;
#[path = "check_worker/recovery.rs"]
mod recovery;

struct Provider {
    run_id: u64,
    rows: Mutex<Vec<CheckRun>>,
    creates: AtomicUsize,
    updates: AtomicUsize,
    lists: AtomicUsize,
    mode: AtomicUsize,
    pool: PgPool,
    actor: Uuid,
    operation: Uuid,
    git: Arc<Source>,
    started: tokio::sync::Notify,
}
fn copy(row: &CheckRun) -> CheckRun {
    CheckRun {
        id: row.id,
        scope: row.scope.clone(),
        operation: row.operation,
        state: row.state,
    }
}
struct Factory(Arc<Provider>);
#[async_trait]
impl CheckProviderFactory for Factory {
    async fn check_provider_bound(
        &self,
        binding: &RepositoryBinding,
    ) -> Result<Arc<dyn ReviewCheckProvider>, CheckProviderError> {
        assert_eq!((binding.remote_id, binding.installation_id), (42, 7));
        if self.0.mode.load(Ordering::SeqCst) == 11 {
            sqlx::query("UPDATE memberships SET role='READER' WHERE user_id=$1")
                .bind(self.0.actor)
                .execute(&self.0.pool)
                .await
                .unwrap();
        }
        Ok(self.0.clone())
    }
}
#[async_trait]
impl ReviewCheckProvider for Provider {
    async fn check_runs(
        &self,
        scope: &CheckScope,
        _: CheckPage,
    ) -> Result<CheckListing, CheckError> {
        self.lists.fetch_add(1, Ordering::SeqCst);
        let mode = self.mode.load(Ordering::SeqCst);
        if mode == 5 {
            return Err(CheckError::Provider(GitError::RateLimited {
                retry_after_seconds: Some(17),
            }));
        }
        let mut items: Vec<_> = self.rows.lock().unwrap().iter().map(copy).collect();
        if mode == 6 {
            items = (81..83)
                .map(|id| CheckRun {
                    id: CheckRunId::new(id).unwrap(),
                    scope: scope.clone(),
                    operation: self.actor,
                    state: CheckState::Pending,
                })
                .collect();
            // Duplicate IDs must be rejected before any write regardless of nonce.
            items[1].id = items[0].id;
        }
        if mode == 12 {
            items = (81..83)
                .map(|id| CheckRun {
                    id: CheckRunId::new(id).unwrap(),
                    scope: scope.clone(),
                    operation: self.operation,
                    state: CheckState::Pending,
                })
                .collect();
        }
        if mode == 13 {
            items = vec![CheckRun {
                id: CheckRunId::new(81).unwrap(),
                scope: CheckScope::new(
                    scope.repository_id(),
                    scope.app_id() + 1,
                    scope.head().clone(),
                )
                .unwrap(),
                operation: self.operation,
                state: CheckState::Pending,
            }];
        }
        let total_count = items.len() as u64 + u64::from(mode == 7);
        Ok(CheckListing {
            items,
            next: None,
            total_count,
        })
    }
    async fn check_run(&self, _: &CheckScope, id: CheckRunId) -> Result<CheckRun, CheckError> {
        self.rows
            .lock()
            .unwrap()
            .iter()
            .find(|r| r.id == id)
            .map(copy)
            .ok_or(CheckError::NotFound)
    }
    async fn create_check(
        &self,
        scope: &CheckScope,
        operation: Uuid,
    ) -> Result<CheckRun, CheckError> {
        assert_eq!(
            self.creates.fetch_add(1, Ordering::SeqCst),
            0,
            "POST repeated"
        );
        let mode = self.mode.load(Ordering::SeqCst);
        let row = CheckRun {
            id: CheckRunId::new(self.run_id).unwrap(),
            scope: scope.clone(),
            operation,
            state: CheckState::Pending,
        };
        if mode != 3 {
            self.rows.lock().unwrap().push(copy(&row));
        }
        if mode == 4 {
            sqlx::query("UPDATE memberships SET role='READER' WHERE user_id=$1")
                .bind(self.actor)
                .execute(&self.pool)
                .await
                .unwrap();
        }
        if mode == 9 {
            self.git.branch_head.store(3, Ordering::SeqCst);
        }
        if mode == 8 {
            self.started.notify_one();
            std::future::pending::<()>().await;
        }
        if mode == 1 || mode == 3 {
            Err(CheckError::Provider(GitError::ResultUnknown))
        } else {
            Ok(row)
        }
    }
    async fn update_check(
        &self,
        scope: &CheckScope,
        id: CheckRunId,
        operation: Uuid,
        result: CheckResult,
    ) -> Result<CheckRun, CheckError> {
        let prior = self.updates.fetch_add(1, Ordering::SeqCst);
        assert!(
            prior == 0 || (prior == 1 && result == CheckResult::Failure),
            "PATCH repeated"
        );
        let row = CheckRun {
            id,
            scope: scope.clone(),
            operation,
            state: result.into(),
        };
        *self.rows.lock().unwrap() = vec![copy(&row)];
        if self.mode.load(Ordering::SeqCst) == 10 {
            self.git.branch_head.store(3, Ordering::SeqCst);
        }
        if self.mode.load(Ordering::SeqCst) == 2 {
            Err(CheckError::Provider(GitError::ResultUnknown))
        } else {
            Ok(row)
        }
    }
}
async fn setup_worker(
    pool: &PgPool,
    approved: bool,
    mode: usize,
) -> (
    Arc<PgReviewChecks>,
    Arc<Provider>,
    ReviewCheckWorker,
    Uuid,
    Uuid,
) {
    let (f, git, h, store) = super::check_admission::prepared(pool, approved).await;
    let ticket = store
        .evaluate(f.owner, f.proposal, h, Uuid::new_v4())
        .await
        .unwrap();
    let provider = Arc::new(Provider {
        run_id: 81,
        rows: Mutex::new(vec![]),
        creates: AtomicUsize::new(0),
        updates: AtomicUsize::new(0),
        lists: AtomicUsize::new(0),
        mode: AtomicUsize::new(mode),
        pool: pool.clone(),
        actor: f.owner,
        operation: ticket.operation_id(),
        git,
        started: tokio::sync::Notify::new(),
    });
    let worker = ReviewCheckWorker::new(
        store.clone(),
        Arc::new(Factory(provider.clone())),
        Duration::from_secs(60),
    )
    .unwrap();
    (store, provider, worker, ticket.job_id, f.proposal)
}
async fn retry(pool: &PgPool, job: Uuid) {
    sqlx::query("UPDATE jobs SET available_at=clock_timestamp()-interval '1 second' WHERE id=$1")
        .bind(job)
        .execute(pool)
        .await
        .unwrap();
}
async fn count(pool: &PgPool, table: &str) -> i64 {
    let query = format!("SELECT count(*) FROM {table}");
    sqlx::query_scalar(&query).fetch_one(pool).await.unwrap()
}
#[sqlx::test(migrations = "../../migrations")]
async fn publishes_pending_and_success_without_approving_or_merging(pool: PgPool) {
    for approved in [false, true] {
        let (_, provider, worker, job, proposal) = setup_worker(&pool, approved, 0).await;
        assert_eq!(worker.run_once().await.unwrap(), CheckWork::Completed(job));
        assert_eq!(worker.run_once().await.unwrap(), CheckWork::Idle);
        assert_eq!(provider.creates.load(Ordering::SeqCst), 1);
        assert_eq!(provider.updates.load(Ordering::SeqCst), 1);
        assert_eq!(
            provider.rows.lock().unwrap()[0].state,
            if approved {
                CheckState::Success
            } else {
                CheckState::Pending
            }
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT state FROM proposals WHERE id=$1")
                .bind(proposal)
                .fetch_one(&pool)
                .await
                .unwrap(),
            "IN_REVIEW"
        );
    }
    assert_eq!(count(&pool, "review_check_results").await, 2);
    assert_eq!(count(&pool, "review_check_writes").await, 4);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM outbox WHERE event_type='REVIEW_CHECK_PUBLISHED'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        2
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn lost_create_and_update_responses_are_observed_without_repeating_writes(pool: PgPool) {
    for mode in [1, 2] {
        let (_, provider, worker, job, _) = setup_worker(&pool, true, mode).await;
        assert_eq!(worker.run_once().await.unwrap(), CheckWork::Deferred(job));
        assert_eq!(worker.run_once().await.unwrap(), CheckWork::Idle);
        retry(&pool, job).await;
        assert_eq!(worker.run_once().await.unwrap(), CheckWork::Completed(job));
        assert_eq!(provider.creates.load(Ordering::SeqCst), 1);
        assert_eq!(provider.updates.load(Ordering::SeqCst), 1);
    }
    assert_eq!(count(&pool, "review_check_writes").await, 4);
}
#[sqlx::test(migrations = "../../migrations")]
async fn absent_unknown_create_is_never_repeated(pool: PgPool) {
    let (_, provider, worker, job, _) = setup_worker(&pool, true, 3).await;
    for _ in 0..3 {
        assert_eq!(worker.run_once().await.unwrap(), CheckWork::Deferred(job));
        retry(&pool, job).await;
    }
    assert_eq!(provider.creates.load(Ordering::SeqCst), 1);
    assert_eq!(provider.updates.load(Ordering::SeqCst), 0);
    assert_eq!(count(&pool, "review_check_writes").await, 1);
    assert_eq!(count(&pool, "review_check_results").await, 0);
}
#[sqlx::test(migrations = "../../migrations")]
async fn revoked_actor_can_preserve_create_identity_but_cannot_update_or_publish(pool: PgPool) {
    let (_, provider, worker, job, _) = setup_worker(&pool, true, 4).await;
    assert_eq!(worker.run_once().await.unwrap(), CheckWork::Stopped(job));
    assert_eq!(provider.creates.load(Ordering::SeqCst), 1);
    assert_eq!(provider.updates.load(Ordering::SeqCst), 0);
    assert_eq!(count(&pool, "review_check_runs").await, 1);
    assert_eq!(count(&pool, "review_check_results").await, 0);
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT o.state FROM jobs j JOIN git_operations o ON o.id=j.operation_id WHERE j.id=$1"
        )
        .bind(job)
        .fetch_one(&pool)
        .await
        .unwrap(),
        "UNKNOWN"
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn rate_limit_persists_connection_cooldown_and_does_not_write(pool: PgPool) {
    let (_, provider, worker, job, _) = setup_worker(&pool, true, 5).await;
    assert_eq!(worker.run_once().await.unwrap(), CheckWork::Deferred(job));
    assert_eq!(worker.run_once().await.unwrap(), CheckWork::Idle);
    let reads = provider.lists.load(Ordering::SeqCst);
    retry(&pool, job).await;
    assert_eq!(worker.run_once().await.unwrap(), CheckWork::Deferred(job));
    assert_eq!(provider.lists.load(Ordering::SeqCst), reads);
    assert_eq!(provider.creates.load(Ordering::SeqCst), 0);
    assert_eq!(count(&pool, "review_check_writes").await, 0);
}
#[sqlx::test(migrations = "../../migrations")]
async fn duplicate_ids_and_incomplete_discovery_never_authorize_creation(pool: PgPool) {
    for mode in [6, 7, 12, 13] {
        let (_, provider, worker, job, _) = setup_worker(&pool, true, mode).await;
        assert_eq!(
            worker.run_once().await.unwrap(),
            if mode != 7 {
                CheckWork::Stopped(job)
            } else {
                CheckWork::Deferred(job)
            }
        );
        assert_eq!(provider.creates.load(Ordering::SeqCst), 0);
        assert_eq!(provider.updates.load(Ordering::SeqCst), 0);
    }
    assert_eq!(count(&pool, "review_check_writes").await, 0);
}
#[sqlx::test(migrations = "../../migrations")]
async fn shutdown_during_create_keeps_marker_for_reconciliation_after_expiry(pool: PgPool) {
    let (store, provider, worker, job, _) = setup_worker(&pool, true, 8).await;
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(async move { worker.run(stopped).await });
    tokio::time::timeout(Duration::from_secs(10), provider.started.notified())
        .await
        .unwrap();
    stop.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(count(&pool, "review_check_writes").await, 1);
    sqlx::query("UPDATE jobs SET leased_until=clock_timestamp()-interval '1 second' WHERE id=$1")
        .bind(job)
        .execute(&pool)
        .await
        .unwrap();
    provider.mode.store(0, Ordering::SeqCst);
    let next = ReviewCheckWorker::new(
        store,
        Arc::new(Factory(provider.clone())),
        Duration::from_secs(60),
    )
    .unwrap();
    assert_eq!(next.run_once().await.unwrap(), CheckWork::Completed(job));
    assert_eq!(provider.creates.load(Ordering::SeqCst), 1);
    assert_eq!(provider.updates.load(Ordering::SeqCst), 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn ref_moves_after_create_or_update_preserve_identity_without_stale_receipts(pool: PgPool) {
    for mode in [9, 10] {
        let (_, provider, worker, job, _) = setup_worker(&pool, true, mode).await;
        assert_eq!(worker.run_once().await.unwrap(), CheckWork::Stopped(job));
        assert_eq!(provider.creates.load(Ordering::SeqCst), 1);
        assert_eq!(
            provider.updates.load(Ordering::SeqCst),
            usize::from(mode == 10)
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM review_check_results r JOIN review_check_intents i USING(operation_id) WHERE i.job_id=$1"
            )
            .bind(job)
            .fetch_one(&pool)
            .await
            .unwrap(),
            0
        );
    }
    assert_eq!(count(&pool, "review_check_runs").await, 2);
}
#[sqlx::test(migrations = "../../migrations")]
async fn authority_change_during_connection_prevents_even_the_create_marker(pool: PgPool) {
    let (_, provider, worker, job, _) = setup_worker(&pool, true, 11).await;
    assert_eq!(worker.run_once().await.unwrap(), CheckWork::Stopped(job));
    assert_eq!(provider.creates.load(Ordering::SeqCst), 0);
    assert_eq!(count(&pool, "review_check_writes").await, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn blocked_provider_io_renews_the_live_lease_without_repeating_create(pool: PgPool) {
    let (_, provider, worker, job, _) = setup_worker(&pool, true, 8).await;
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(async move { worker.run(stopped).await });
    tokio::time::timeout(Duration::from_secs(30), provider.started.notified())
        .await
        .unwrap();
    sqlx::query("UPDATE jobs SET leased_until=clock_timestamp()+interval '25 seconds' WHERE id=$1")
        .bind(job)
        .execute(&pool)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(17)).await;
    assert!(
        sqlx::query_scalar::<_, bool>(
            "SELECT leased_until>clock_timestamp()+interval '90 seconds' FROM jobs WHERE id=$1"
        )
        .bind(job)
        .fetch_one(&pool)
        .await
        .unwrap()
    );
    stop.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(provider.creates.load(Ordering::SeqCst), 1);
    assert_eq!(count(&pool, "review_check_results").await, 0);
}
