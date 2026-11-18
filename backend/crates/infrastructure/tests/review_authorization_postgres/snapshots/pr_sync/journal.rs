use super::*;
#[path = "status.rs"]
mod status;
use specspace_application::pr_sync::{PrSyncDecision, PrWritePhase};
use specspace_git_adapter::pull_requests::{PullRequest, PullRequestNumber, PullRequestState};
use specspace_infrastructure::pr_sync::{PrSyncLease, PrSyncResolution};
async fn job(pool: &PgPool) -> (Fixture, PgPrSync, PrSyncLease) {
    let (f, _, _, event) = publication(pool).await;
    let store = PgPrSync::new(pool.clone());
    store
        .reserve(f.owner, f.proposal, event, &site())
        .await
        .unwrap();
    let lease = store.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    (f, store, lease)
}
fn observed(lease: &PrSyncLease) -> PullRequest {
    let s = lease.intent().source();
    PullRequest {
        id: 77,
        node_id: "PR_SYNTHETIC_9+/=".into(),
        number: PullRequestNumber::new(9).unwrap(),
        repository_id: s.repository.remote_id,
        branch: s.branch.clone(),
        head: s.head.clone(),
        target: s.target.clone(),
        state: PullRequestState::Open,
        draft: true,
        title: lease.intent().content().title().into(),
        body: lease.intent().content().body().into(),
        url: "https://github.com/synthetic/specs/pull/9".into(),
    }
}
async fn expire(pool: &PgPool, lease: &PrSyncLease) {
    sqlx::query("UPDATE jobs SET leased_until=clock_timestamp()-interval '1 second' WHERE id=$1")
        .bind(lease.job_id())
        .execute(pool)
        .await
        .unwrap();
}
async fn due(pool: &PgPool, lease: &PrSyncLease) {
    sqlx::query("UPDATE jobs SET available_at=clock_timestamp()-interval '1 second' WHERE id=$1")
        .bind(lease.job_id())
        .execute(pool)
        .await
        .unwrap();
}
async fn state(pool: &PgPool, lease: &PrSyncLease) -> (String, String) {
    sqlx::query_as("SELECT o.state,j.state FROM git_operations o JOIN jobs j ON j.operation_id=o.id WHERE o.id=$1").bind(lease.operation_id()).fetch_one(pool).await.unwrap()
}
async fn receipts(pool: &PgPool) -> (i64, i64, i64) {
    sqlx::query_as("SELECT (SELECT count(*) FROM pr_sync_writes),(SELECT count(*) FROM proposal_pull_requests),(SELECT count(*) FROM pr_sync_results)").fetch_one(pool).await.unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
async fn exact_creation_receipt_atomically_binds_one_pr_and_completes_the_job(pool: PgPool) {
    let (_, store, lease) = job(&pool).await;
    let view = store.check(&lease).await.unwrap();
    assert_eq!(view.phase, PrWritePhase::NotStarted);
    assert!(view.binding.is_none());
    assert!(matches!(
        store.complete(&lease, &observed(&lease)).await,
        Err(PrSyncStoreError::InvalidReceipt)
    ));
    assert_eq!(
        store.start_write(&lease, &[]).await.unwrap(),
        PrWritePhase::CreateStarted
    );
    assert_eq!(
        store.complete(&lease, &observed(&lease)).await.unwrap(),
        PrSyncResolution::Synced
    );
    assert_eq!(
        state(&pool, &lease).await,
        ("SUCCEEDED".into(), "SUCCEEDED".into())
    );
    assert_eq!(receipts(&pool).await, (1, 1, 1));
    assert!(store.claim(Uuid::new_v4(), 60).await.unwrap().is_none());
    for sql in [
        "UPDATE pr_sync_writes SET kind='UPDATE' WHERE operation_id=$1",
        "DELETE FROM pr_sync_results WHERE operation_id=$1",
        "UPDATE proposal_pull_requests SET provider_id='88' WHERE created_operation=$1",
        "UPDATE jobs SET result='{}' WHERE operation_id=$1",
    ] {
        assert!(
            sqlx::query(sql)
                .bind(lease.operation_id())
                .execute(&pool)
                .await
                .is_err(),
            "{sql}"
        );
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn same_worker_reclaim_is_attempt_fenced_and_an_unknown_creation_is_observation_only(
    pool: PgPool,
) {
    let (_, store, first) = job(&pool).await;
    store.start_write(&first, &[]).await.unwrap();
    expire(&pool, &first).await;
    // Recover with the same UUID to prove fencing includes the attempt number.
    let worker: Uuid = sqlx::query_scalar("SELECT lease_owner FROM jobs WHERE id=$1")
        .bind(first.job_id())
        .fetch_one(&pool)
        .await
        .unwrap();
    let second = store.claim(worker, 60).await.unwrap().unwrap();
    assert_eq!(second.attempts(), 2);
    assert_eq!(
        second.intent().request_hash(),
        first.intent().request_hash()
    );
    let view = store.check(&second).await.unwrap();
    assert_eq!(view.phase, PrWritePhase::CreateStarted);
    assert_eq!(
        second.intent().decide(&[], view.binding, view.phase),
        PrSyncDecision::ObserveAgain
    );
    assert_eq!(state(&pool, &second).await.0, "UNKNOWN");
    assert!(matches!(
        store.heartbeat(&first, 60).await,
        Err(PrSyncStoreError::LeaseLost)
    ));
    assert!(matches!(
        store.complete(&first, &observed(&first)).await,
        Err(PrSyncStoreError::LeaseLost)
    ));
    assert!(matches!(
        store.start_write(&second, &[]).await,
        Err(PrSyncStoreError::WriteAlreadyStarted)
    ));
    assert_eq!(
        store.complete(&second, &observed(&second)).await.unwrap(),
        PrSyncResolution::Synced
    );
    assert_eq!(receipts(&pool).await, (1, 1, 1));
}

#[sqlx::test(migrations = "../../migrations")]
async fn not_started_leases_can_be_reclaimed_but_expired_workers_cannot_start_or_clean_up(
    pool: PgPool,
) {
    let (_, store, first) = job(&pool).await;
    expire(&pool, &first).await;
    assert!(matches!(
        store.start_write(&first, &[]).await,
        Err(PrSyncStoreError::LeaseLost)
    ));
    assert!(matches!(
        store.defer(&first, "STALE", None).await,
        Err(PrSyncStoreError::LeaseLost)
    ));
    let next = store.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    assert_eq!(
        store.check(&next).await.unwrap().phase,
        PrWritePhase::NotStarted
    );
    store.heartbeat(&next, 120).await.unwrap();
    store.start_write(&next, &[]).await.unwrap();
    assert_eq!(receipts(&pool).await, (1, 0, 0));
}

#[sqlx::test(migrations = "../../migrations")]
async fn stale_metadata_or_revoked_actor_stops_new_writes_but_allows_safe_prewrite_cleanup(
    pool: PgPool,
) {
    for revoked in [false, true] {
        let (f, store, lease) = job(&pool).await;
        if revoked {
            sqlx::query("UPDATE users SET status='DISABLED' WHERE id=$1")
                .bind(f.owner)
                .execute(&pool)
                .await
                .unwrap();
        } else {
            sqlx::query("UPDATE proposals SET links=$2 WHERE id=$1")
                .bind(f.proposal)
                .bind(json!([{"kind":"ticket","url":"https://tracker.example/new"}]))
                .execute(&pool)
                .await
                .unwrap();
        }
        assert!(store.check(&lease).await.is_err());
        assert!(store.start_write(&lease, &[]).await.is_err());
        assert!(store.heartbeat(&lease, 60).await.is_err());
        assert!(
            !store
                .defer(&lease, "AUTHORITY_CHANGED", None)
                .await
                .unwrap()
        );
        assert_eq!(
            state(&pool, &lease).await,
            ("FAILED".into(), "FAILED".into())
        );
    }
    assert_eq!(receipts(&pool).await, (0, 0, 0));
}

#[sqlx::test(migrations = "../../migrations")]
async fn changed_content_after_create_resolves_as_superseded_without_losing_pr_identity(
    pool: PgPool,
) {
    let (f, store, lease) = job(&pool).await;
    store.start_write(&lease, &[]).await.unwrap();
    sqlx::query("UPDATE proposals SET head_sha=$2,version=version+1 WHERE id=$1")
        .bind(f.proposal)
        .bind(sha(9).as_str())
        .execute(&pool)
        .await
        .unwrap();
    let mut pr = observed(&lease);
    pr.head = sha(9);
    assert!(store.check(&lease).await.is_err());
    assert_eq!(
        store.observe(&lease).await.unwrap().phase,
        PrWritePhase::CreateStarted
    );
    store.heartbeat(&lease, 60).await.unwrap();
    assert_eq!(
        store.complete(&lease, &pr).await.unwrap(),
        PrSyncResolution::Superseded
    );
    assert_eq!(
        state(&pool, &lease).await,
        ("SUCCEEDED".into(), "CANCELLED".into())
    );
    assert_eq!(receipts(&pool).await, (1, 1, 1));
    let pointer: Option<Uuid> =
        sqlx::query_scalar("SELECT active_review_snapshot_id FROM proposals WHERE id=$1")
            .bind(f.proposal)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(pointer.is_none());
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_next_snapshot_updates_the_bound_pr_instead_of_creating_another(pool: PgPool) {
    let (f, _, preparer) = setup(&pool).await;
    let (source, verified) = prepared(&pool, &f, &preparer, 2).await;
    PgReviewSnapshots::new(pool.clone())
        .publish(&source, &verified, &preparer)
        .await
        .unwrap();
    let event: Uuid = sqlx::query_scalar(
        "SELECT id FROM outbox WHERE entity_id=$1 AND event_type='REVIEW_SNAPSHOT_PUBLISHED'",
    )
    .bind(f.proposal)
    .fetch_one(&pool)
    .await
    .unwrap();
    let store = PgPrSync::new(pool.clone());
    store
        .reserve(f.owner, f.proposal, event, &site())
        .await
        .unwrap();
    let first = store.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    store.start_write(&first, &[]).await.unwrap();
    let old = observed(&first);
    store.complete(&first, &old).await.unwrap();
    let (source, verified) = prepared(&pool, &f, &preparer, 3).await;
    PgReviewSnapshots::new(pool.clone())
        .publish(&source, &verified, &preparer)
        .await
        .unwrap();
    let event2:Uuid=sqlx::query_scalar("SELECT id FROM outbox WHERE entity_id=$1 AND event_type='REVIEW_SNAPSHOT_PUBLISHED' AND id<>$2").bind(f.proposal).bind(event).fetch_one(&pool).await.unwrap();
    store
        .reserve(f.owner, f.proposal, event2, &site())
        .await
        .unwrap();
    let next = store.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    assert!(store.check(&next).await.unwrap().binding.is_some());
    assert!(matches!(
        store.start_write(&next, &[]).await,
        Err(PrSyncStoreError::InvalidReceipt)
    ));
    assert_eq!(
        store.start_write(&next, &[old]).await.unwrap(),
        PrWritePhase::UpdateStarted
    );
    store.complete(&next, &observed(&next)).await.unwrap();
    assert_eq!(receipts(&pool).await, (2, 1, 2));
}

#[sqlx::test(migrations = "../../migrations")]
async fn definite_prewrite_exhaustion_releases_the_slot_but_started_uncertainty_never_does(
    pool: PgPool,
) {
    for started in [false, true] {
        let (_, store, mut lease) = job(&pool).await;
        if started {
            store.start_write(&lease, &[]).await.unwrap();
        }
        for attempt in 1..=8 {
            assert_eq!(lease.attempts(), attempt);
            let retry = store
                .defer(&lease, "PROVIDER_BACKOFF", Some(5))
                .await
                .unwrap();
            assert_eq!(retry, attempt < 8);
            if retry {
                due(&pool, &lease).await;
                lease = store.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
            }
        }
        assert_eq!(
            state(&pool, &lease).await,
            (
                (if started { "UNKNOWN" } else { "FAILED" }).into(),
                "FAILED".into()
            )
        );
        assert!(store.claim(Uuid::new_v4(), 60).await.unwrap().is_none());
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn malformed_receipts_and_revoked_publication_never_bind_or_complete_an_unknown_write(
    pool: PgPool,
) {
    let (f, store, lease) = job(&pool).await;
    store.start_write(&lease, &[]).await.unwrap();
    for mode in 0..6 {
        let mut pr = observed(&lease);
        match mode {
            0 => pr.repository_id = 99,
            1 => pr.branch = BranchName::parse("change/foreign").unwrap(),
            2 => pr.state = PullRequestState::Closed,
            3 => pr.draft = false,
            4 => pr.body.push_str("changed"),
            _ => pr.title = "Changed title".into(),
        }
        assert!(matches!(
            store.complete(&lease, &pr).await,
            Err(PrSyncStoreError::InvalidReceipt)
        ));
    }
    sqlx::query("UPDATE users SET status='DISABLED' WHERE id=$1")
        .bind(f.owner)
        .execute(&pool)
        .await
        .unwrap();
    assert!(store.complete(&lease, &observed(&lease)).await.is_err());
    store.defer(&lease, "ACTOR_REVOKED", None).await.unwrap();
    assert_eq!(
        state(&pool, &lease).await,
        ("UNKNOWN".into(), "FAILED".into())
    );
    assert_eq!(receipts(&pool).await, (1, 0, 0));
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_binding_redirect_cannot_publish_a_receipt_for_the_old_repository(pool: PgPool) {
    let (f, store, lease) = job(&pool).await;
    store.start_write(&lease, &[]).await.unwrap();
    sqlx::query("UPDATE repositories SET installation_id='8' WHERE id=$1")
        .bind(f.repository)
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        store.complete(&lease, &observed(&lease)).await,
        Err(PrSyncStoreError::Stale)
    ));
    store.defer(&lease, "BINDING_CHANGED", None).await.unwrap();
    assert_eq!(receipts(&pool).await, (1, 0, 0));
}

#[sqlx::test(migrations = "../../migrations")]
async fn failure_after_receipt_insertion_rolls_back_binding_result_and_job_together(pool: PgPool) {
    let (_, store, lease) = job(&pool).await;
    store.start_write(&lease, &[]).await.unwrap();
    sqlx::query("CREATE FUNCTION reject_pr_resolution() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='PR_SYNC_RESOLVED' THEN RAISE EXCEPTION 'synthetic failure'; END IF; RETURN NEW; END $$").execute(&pool).await.unwrap();
    sqlx::query("CREATE TRIGGER reject_pr_resolution BEFORE INSERT ON audit_events FOR EACH ROW EXECUTE FUNCTION reject_pr_resolution()").execute(&pool).await.unwrap();
    assert!(matches!(
        store.complete(&lease, &observed(&lease)).await,
        Err(PrSyncStoreError::Unavailable)
    ));
    assert_eq!(receipts(&pool).await, (1, 0, 0));
    assert_eq!(
        state(&pool, &lease).await,
        ("RUNNING".into(), "RUNNING".into())
    );
    sqlx::query("DROP TRIGGER reject_pr_resolution ON audit_events")
        .execute(&pool)
        .await
        .unwrap();
    store.complete(&lease, &observed(&lease)).await.unwrap();
    assert_eq!(receipts(&pool).await, (1, 1, 1));
}

#[sqlx::test(migrations = "../../migrations")]
async fn lease_expiry_during_final_audit_discards_the_entire_receipt(pool: PgPool) {
    let (_, store, lease) = job(&pool).await;
    store.start_write(&lease, &[]).await.unwrap();
    sqlx::query("UPDATE jobs SET leased_until=clock_timestamp()+interval '1 second' WHERE id=$1")
        .bind(lease.job_id())
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("CREATE FUNCTION delay_pr_resolution() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='PR_SYNC_RESOLVED' THEN PERFORM pg_sleep(2); END IF; RETURN NEW; END $$").execute(&pool).await.unwrap();
    sqlx::query("CREATE TRIGGER delay_pr_resolution BEFORE INSERT ON audit_events FOR EACH ROW EXECUTE FUNCTION delay_pr_resolution()").execute(&pool).await.unwrap();
    assert!(matches!(
        store.complete(&lease, &observed(&lease)).await,
        Err(PrSyncStoreError::LeaseLost)
    ));
    assert_eq!(receipts(&pool).await, (1, 0, 0));
}

#[sqlx::test(migrations = "../../migrations")]
async fn closed_proposal_allows_read_only_reconciliation_but_never_a_new_write(pool: PgPool) {
    let (f, store, lease) = job(&pool).await;
    store.start_write(&lease, &[]).await.unwrap();
    sqlx::query("UPDATE proposals SET state='CLOSED' WHERE id=$1")
        .bind(f.proposal)
        .execute(&pool)
        .await
        .unwrap();
    assert!(store.check(&lease).await.is_err());
    assert!(store.start_write(&lease, &[]).await.is_err());
    assert!(store.observe(&lease).await.is_ok());
    store.heartbeat(&lease, 60).await.unwrap();
    assert_eq!(
        store.complete(&lease, &observed(&lease)).await.unwrap(),
        PrSyncResolution::Superseded
    );
    assert_eq!(
        state(&pool, &lease).await,
        ("SUCCEEDED".into(), "CANCELLED".into())
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn database_requires_current_write_fences_and_atomic_phase_and_receipt_evidence(
    pool: PgPool,
) {
    let (_, store, lease) = job(&pool).await;
    let worker: Uuid = sqlx::query_scalar("SELECT lease_owner FROM jobs WHERE id=$1")
        .bind(lease.job_id())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(sqlx::query("INSERT INTO pr_sync_writes(operation_id,kind,fence_worker,fence_attempt) VALUES($1,'CREATE',$2,99)").bind(lease.operation_id()).bind(worker).execute(&pool).await.is_err());
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("INSERT INTO pr_sync_writes(operation_id,kind,fence_worker,fence_attempt) VALUES($1,'CREATE',$2,1)").bind(lease.operation_id()).bind(worker).execute(&mut *tx).await.unwrap();
    assert!(tx.commit().await.is_err());
    assert!(
        sqlx::query("UPDATE git_operations SET state='RUNNING' WHERE id=$1")
            .bind(lease.operation_id())
            .execute(&pool)
            .await
            .is_err()
    );
    store.start_write(&lease, &[]).await.unwrap();
    expire(&pool, &lease).await;
    assert!(store.complete(&lease, &observed(&lease)).await.is_err());
    assert_eq!(receipts(&pool).await, (1, 0, 0));
}

#[sqlx::test(migrations = "../../migrations")]
async fn crashing_on_the_final_attempt_is_reaped_without_losing_write_uncertainty(pool: PgPool) {
    for started in [false, true] {
        let (_, store, mut lease) = job(&pool).await;
        if started {
            store.start_write(&lease, &[]).await.unwrap();
        }
        for _ in 1..8 {
            store.defer(&lease, "BACKOFF", Some(1)).await.unwrap();
            due(&pool, &lease).await;
            lease = store.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
        }
        expire(&pool, &lease).await;
        assert!(store.claim(Uuid::new_v4(), 60).await.unwrap().is_none());
        assert_eq!(
            state(&pool, &lease).await,
            (
                (if started { "UNKNOWN" } else { "FAILED" }).into(),
                "FAILED".into()
            )
        );
        let audits:i64=sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE operation_id=$1 AND action='PR_SYNC_EXHAUSTED'").bind(lease.operation_id()).fetch_one(&pool).await.unwrap();
        assert_eq!(audits, 1);
    }
}
