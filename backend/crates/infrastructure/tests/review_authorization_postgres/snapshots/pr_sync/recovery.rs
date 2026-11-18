use super::*;
use serde_json::Value;
use specspace_application::pr_recovery::{PrRecoveryMode, PrRecoveryRequest};

#[sqlx::test(migrations = "../../migrations")]
async fn resubmission_keeps_stopped_previous_work_recoverable_without_claiming_new_work(
    pool: PgPool,
) {
    let (f, _, preparer) = setup(&pool).await;
    let snapshots = PgReviewSnapshots::new(pool.clone());
    let (lease, verified) = prepared(&pool, &f, &preparer, 2).await;
    let first = snapshots
        .publish(&lease, &verified, &preparer)
        .await
        .unwrap();
    let event = sqlx::query_scalar(
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
    let old = store.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    store.start_write(&old, &[]).await.unwrap();
    store.defer(&old, "PR_RESULT_UNKNOWN", None).await.unwrap();
    let (lease, verified) = prepared(&pool, &f, &preparer, 3).await;
    let second = snapshots
        .publish(&lease, &verified, &preparer)
        .await
        .unwrap();
    assert_eq!(first.snapshot_id, second.snapshot_id);
    let admin = user(&pool, f.org, "ADMIN", vec![]).await;
    let status = store
        .status(admin, f.proposal, first.snapshot_id)
        .await
        .unwrap();
    assert!(status.sync.is_none() && status.uncertain_write);
    assert_eq!(
        status.dispatch.phase,
        specspace_application::review_pr_status::ReviewPrDispatchPhase::Awaiting
    );
    assert_eq!(status.recovery, Some(PrRecoveryMode::ObserveStarted));
    recover(&store, admin, &old, &request(&old)).await.unwrap();
    let resumed = store.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    assert_eq!(resumed.operation_id(), old.operation_id());
    assert!(store.start_write(&resumed, &[]).await.is_err());
}

fn request(lease: &PrSyncLease) -> PrRecoveryRequest {
    PrRecoveryRequest {
        request_id: Uuid::new_v4(),
        expected_fingerprint: lease.intent().source().fingerprint.clone(),
    }
}
async fn recover(
    store: &PgPrSync,
    admin: Uuid,
    lease: &PrSyncLease,
    request: &PrRecoveryRequest,
) -> Result<specspace_application::pr_recovery::PrRecoveryAccepted, PrSyncStoreError> {
    store
        .recover(
            admin,
            lease.intent().source().proposal,
            lease.intent().source().snapshot,
            request,
        )
        .await
}
async fn recovery_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM pr_sync_recoveries")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
async fn recovery_receipts_require_atomic_queue_audit_and_are_immutable_after_commit(pool: PgPool) {
    let (f, store, old) = job(&pool).await;
    let admin = user(&pool, f.org, "ADMIN", vec![]).await;
    store
        .defer(&old, "PR_PROVIDER_UNAVAILABLE", None)
        .await
        .unwrap();
    let req = request(&old);
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("INSERT INTO pr_sync_recoveries(organization_id,request_id,proposal_id,snapshot_id,operation_id,actor_id,fingerprint,mode,previous_attempts,previous_max_attempts,max_attempts) SELECT i.organization_id,$2,i.proposal_id,i.snapshot_id,i.operation_id,$3,i.fingerprint,'RETRY_UNSTARTED',j.attempts,j.max_attempts,j.max_attempts+8 FROM pr_sync_intents i JOIN jobs j ON j.id=i.job_id WHERE i.operation_id=$1")
        .bind(old.operation_id()).bind(req.request_id).bind(admin).execute(&mut *tx).await.unwrap();
    assert!(tx.commit().await.is_err());
    assert_eq!(recovery_count(&pool).await, 0);
    recover(&store, admin, &old, &req).await.unwrap();
    for sql in [
        "UPDATE pr_sync_recoveries SET actor_id=$1",
        "DELETE FROM pr_sync_recoveries WHERE actor_id=$1",
    ] {
        assert!(sqlx::query(sql).bind(admin).execute(&pool).await.is_err());
    }
    assert!(
        sqlx::query("TRUNCATE pr_sync_recoveries")
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("UPDATE jobs SET max_attempts=max_attempts+8 WHERE operation_id=$1")
            .bind(old.operation_id())
            .execute(&pool)
            .await
            .is_err()
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn recovery_migration_preserves_old_failed_unknown_work_before_explicit_admission(
    pool: PgPool,
) {
    let (f, store, old) = job(&pool).await;
    store.start_write(&old, &[]).await.unwrap();
    store.defer(&old, "PR_RESULT_UNKNOWN", None).await.unwrap();
    let query = "SELECT jsonb_build_object('intent',to_jsonb(i),'job',to_jsonb(j),'operation',to_jsonb(o),'write',to_jsonb(w)) FROM pr_sync_intents i JOIN jobs j ON j.id=i.job_id JOIN git_operations o ON o.id=i.operation_id JOIN pr_sync_writes w ON w.operation_id=i.operation_id WHERE i.operation_id=$1";
    let before: Value = sqlx::query_scalar(query)
        .bind(old.operation_id())
        .fetch_one(&pool)
        .await
        .unwrap();
    sqlx::raw_sql("DROP TABLE pr_sync_recoveries CASCADE;DROP FUNCTION pr_sync_recovery_insert();DROP FUNCTION pr_sync_recovery_complete();DELETE FROM _sqlx_migrations WHERE version=202611180017;").execute(&pool).await.unwrap();
    let migration = include_str!("../../../../../../migrations/202611180014_pr_sync_journal.sql");
    let old_guard = migration
        .split("CREATE OR REPLACE FUNCTION pr_sync_scope_guard()")
        .nth(1)
        .unwrap()
        .split("CREATE OR REPLACE FUNCTION pr_sync_complete()")
        .next()
        .unwrap();
    sqlx::raw_sql(&format!(
        "CREATE OR REPLACE FUNCTION pr_sync_scope_guard(){old_guard}"
    ))
    .execute(&pool)
    .await
    .unwrap();
    specspace_infrastructure::MIGRATOR.run(&pool).await.unwrap();
    let after: Value = sqlx::query_scalar(query)
        .bind(old.operation_id())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(before, after);
    assert_eq!(recovery_count(&pool).await, 0);
    let admin = user(&pool, f.org, "ADMIN", vec![]).await;
    recover(&store, admin, &old, &request(&old)).await.unwrap();
    let next = store.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    assert_eq!(
        store.observe(&next).await.unwrap().phase,
        PrWritePhase::CreateStarted
    );
    assert!(store.start_write(&next, &[]).await.is_err());
}

#[sqlx::test(migrations = "../../migrations")]
async fn earlier_admin_grants_cannot_replace_a_revoked_latest_recovery_window(pool: PgPool) {
    let (f, store, old) = job(&pool).await;
    let first = user(&pool, f.org, "ADMIN", vec![]).await;
    let second = user(&pool, f.org, "ADMIN", vec![]).await;
    store.start_write(&old, &[]).await.unwrap();
    store.defer(&old, "PR_RESULT_UNKNOWN", None).await.unwrap();
    sqlx::query("UPDATE users SET status='DISABLED' WHERE id=$1")
        .bind(f.owner)
        .execute(&pool)
        .await
        .unwrap();
    recover(&store, first, &old, &request(&old)).await.unwrap();
    let next = store.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    store.defer(&next, "PR_RESULT_UNKNOWN", None).await.unwrap();
    recover(&store, second, &next, &request(&next))
        .await
        .unwrap();
    let last = store.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    store.observe(&last).await.unwrap();
    sqlx::query("UPDATE users SET status='DISABLED' WHERE id=$1")
        .bind(second)
        .execute(&pool)
        .await
        .unwrap();
    assert!(store.observe(&last).await.is_err());
    assert!(store.complete(&last, &observed(&last)).await.is_err());
}

#[sqlx::test(migrations = "../../migrations")]
async fn untouched_failure_recovers_same_intent_without_resetting_fences_and_replays_exactly(
    pool: PgPool,
) {
    let (f, store, old) = job(&pool).await;
    let admin = user(&pool, f.org, "ADMIN", vec![]).await;
    store
        .defer(&old, "PR_PROVIDER_UNAVAILABLE", None)
        .await
        .unwrap();
    let req = request(&old);
    let selected = old.intent().source().snapshot;
    assert_eq!(
        store
            .status(admin, f.proposal, selected)
            .await
            .unwrap()
            .recovery,
        Some(PrRecoveryMode::RetryUnstarted)
    );
    let reader = user(&pool, f.org, "READER", vec![]).await;
    assert!(
        store
            .status(reader, f.proposal, selected)
            .await
            .unwrap()
            .recovery
            .is_none()
    );
    let accepted = recover(&store, admin, &old, &req).await.unwrap();
    assert_eq!(accepted.mode, PrRecoveryMode::RetryUnstarted);
    assert!(!accepted.replayed);
    assert!(
        store
            .status(admin, f.proposal, selected)
            .await
            .unwrap()
            .recovery
            .is_none()
    );
    let next = store.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    assert_eq!(next.operation_id(), old.operation_id());
    assert_eq!(next.attempts(), old.attempts() + 1);
    assert_eq!(next.intent().request_hash(), old.intent().request_hash());
    assert!(matches!(
        store.start_write(&old, &[]).await,
        Err(PrSyncStoreError::LeaseLost)
    ));
    store.start_write(&next, &[]).await.unwrap();
    store.complete(&next, &observed(&next)).await.unwrap();
    assert!(recover(&store, admin, &old, &req).await.unwrap().replayed);
    assert_eq!(recovery_count(&pool).await, 1);
    let audits: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE action='PR_SYNC_RECOVERY_QUEUED' AND actor_id=$1",
    )
    .bind(admin)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(audits, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn started_failure_after_revocation_and_closure_is_admin_delegated_observation_only(
    pool: PgPool,
) {
    let (f, store, old) = job(&pool).await;
    let admin = user(&pool, f.org, "ADMIN", vec![]).await;
    store.start_write(&old, &[]).await.unwrap();
    store.defer(&old, "PR_RESULT_UNKNOWN", None).await.unwrap();
    sqlx::query("UPDATE users SET status='DISABLED' WHERE id=$1")
        .bind(f.owner)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE proposals SET state='CLOSED' WHERE id=$1")
        .bind(f.proposal)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        recover(&store, admin, &old, &request(&old))
            .await
            .unwrap()
            .mode,
        PrRecoveryMode::ObserveStarted
    );
    let next = store.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    assert_eq!(state(&pool, &next).await.0, "UNKNOWN");
    assert_eq!(
        store.observe(&next).await.unwrap().phase,
        PrWritePhase::CreateStarted
    );
    assert!(store.check(&next).await.is_err());
    assert!(store.start_write(&next, &[]).await.is_err());
    store.heartbeat(&next, 60).await.unwrap();
    assert_eq!(
        store.complete(&next, &observed(&next)).await.unwrap(),
        PrSyncResolution::Superseded
    );
    assert_eq!(receipts(&pool).await, (1, 1, 1));
}

#[sqlx::test(migrations = "../../migrations")]
async fn revoked_recovery_admin_cannot_renew_observe_or_publish_for_revoked_editor(pool: PgPool) {
    let (f, store, old) = job(&pool).await;
    let admin = user(&pool, f.org, "ADMIN", vec![]).await;
    store.start_write(&old, &[]).await.unwrap();
    store.defer(&old, "PR_RESULT_UNKNOWN", None).await.unwrap();
    sqlx::query("UPDATE users SET status='DISABLED' WHERE id=$1")
        .bind(f.owner)
        .execute(&pool)
        .await
        .unwrap();
    recover(&store, admin, &old, &request(&old)).await.unwrap();
    let next = store.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    store.observe(&next).await.unwrap();
    sqlx::query("UPDATE memberships SET role='READER' WHERE organization_id=$1 AND user_id=$2")
        .bind(f.org)
        .bind(admin)
        .execute(&pool)
        .await
        .unwrap();
    assert!(store.observe(&next).await.is_err());
    assert!(store.heartbeat(&next, 60).await.is_err());
    assert!(store.complete(&next, &observed(&next)).await.is_err());
    assert_eq!(receipts(&pool).await, (1, 0, 0));
    store.defer(&next, "PR_FORBIDDEN", None).await.unwrap();
    assert_eq!(state(&pool, &next).await.0, "UNKNOWN");
}

#[sqlx::test(migrations = "../../migrations")]
async fn unstarted_recovery_requires_original_editor_current_scope_and_binding(pool: PgPool) {
    for mode in 0..4 {
        let (f, store, old) = job(&pool).await;
        let admin = user(&pool, f.org, "ADMIN", vec![]).await;
        store
            .defer(&old, "PR_PROVIDER_UNAVAILABLE", None)
            .await
            .unwrap();
        match mode {
            0 => {
                sqlx::query("UPDATE users SET status='DISABLED' WHERE id=$1")
                    .bind(f.owner)
                    .execute(&pool)
                    .await
                    .unwrap();
            }
            1 => {
                sqlx::query(
                    "UPDATE proposals SET description='Changed description after failure',version=version+1 WHERE id=$1",
                )
                .bind(f.proposal)
                .execute(&pool)
                .await
                .unwrap();
            }
            2 => {
                sqlx::query("UPDATE proposals SET links=$2 WHERE id=$1")
                    .bind(f.proposal)
                    .bind(json!([{"kind":"ticket","url":"https://example.com/new"}]))
                    .execute(&pool)
                    .await
                    .unwrap();
            }
            _ => {
                sqlx::query("UPDATE repositories SET installation_id='8' WHERE id=$1")
                    .bind(f.repository)
                    .execute(&pool)
                    .await
                    .unwrap();
            }
        }
        assert!(recover(&store, admin, &old, &request(&old)).await.is_err());
    }
    assert_eq!(recovery_count(&pool).await, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn recovery_is_admin_scoped_idempotent_and_cannot_restart_live_or_completed_work(
    pool: PgPool,
) {
    let (f, store, old) = job(&pool).await;
    let admin = user(&pool, f.org, "ADMIN", vec![]).await;
    assert!(matches!(
        recover(&store, admin, &old, &request(&old)).await,
        Err(PrSyncStoreError::Busy)
    ));
    store
        .defer(&old, "PR_PROVIDER_UNAVAILABLE", None)
        .await
        .unwrap();
    for role in ["READER", "CONTRIBUTOR", "REVIEWER", "MAINTAINER"] {
        let actor = user(&pool, f.org, role, vec![]).await;
        assert!(matches!(
            recover(&store, actor, &old, &request(&old)).await,
            Err(PrSyncStoreError::Access(SubmissionError::Forbidden))
        ));
    }
    let foreign = fixture(&pool).await;
    assert!(matches!(
        recover(&store, foreign.owner, &old, &request(&old)).await,
        Err(PrSyncStoreError::Access(SubmissionError::NotFound))
    ));
    let req = request(&old);
    recover(&store, admin, &old, &req).await.unwrap();
    let another = user(&pool, f.org, "ADMIN", vec![]).await;
    assert!(matches!(
        recover(&store, another, &old, &req).await,
        Err(PrSyncStoreError::Access(
            SubmissionError::IdempotencyMismatch
        ))
    ));
    let mut mismatch = req.clone();
    mismatch.expected_fingerprint = "a".repeat(64);
    assert!(matches!(
        recover(&store, admin, &old, &mismatch).await,
        Err(PrSyncStoreError::Access(
            SubmissionError::IdempotencyMismatch
        ))
    ));
    let next = store.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    store.start_write(&next, &[]).await.unwrap();
    store.complete(&next, &observed(&next)).await.unwrap();
    assert!(matches!(
        recover(&store, admin, &old, &request(&old)).await,
        Err(PrSyncStoreError::Busy)
    ));
    sqlx::query("UPDATE users SET status='DISABLED' WHERE id=$1")
        .bind(admin)
        .execute(&pool)
        .await
        .unwrap();
    assert!(recover(&store, admin, &old, &req).await.is_err());
}

#[sqlx::test(migrations = "../../migrations")]
async fn concurrent_recovery_replays_share_one_window_and_quota_does_not_block_exact_replay(
    pool: PgPool,
) {
    let (f, store, old) = job(&pool).await;
    let admin = user(&pool, f.org, "ADMIN", vec![]).await;
    store
        .defer(&old, "PR_PROVIDER_UNAVAILABLE", None)
        .await
        .unwrap();
    let req = request(&old);
    let (a, b) = tokio::join!(
        recover(&store, admin, &old, &req),
        recover(&store, admin, &old, &req)
    );
    assert_ne!(a.unwrap().replayed, b.unwrap().replayed);
    let next = store.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    store
        .defer(&next, "PR_PROVIDER_UNAVAILABLE", None)
        .await
        .unwrap();
    recover(&store, admin, &next, &request(&next))
        .await
        .unwrap();
    let last = store.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    store
        .defer(&last, "PR_PROVIDER_UNAVAILABLE", None)
        .await
        .unwrap();
    assert!(matches!(
        recover(&store, admin, &last, &request(&last)).await,
        Err(PrSyncStoreError::QuotaExceeded)
    ));
    assert!(recover(&store, admin, &old, &req).await.unwrap().replayed);
    assert_eq!(recovery_count(&pool).await, 2);
}

#[sqlx::test(migrations = "../../migrations")]
async fn recovery_audit_failure_rolls_back_all_changes_and_database_rejects_blind_resets(
    pool: PgPool,
) {
    let (f, store, old) = job(&pool).await;
    let admin = user(&pool, f.org, "ADMIN", vec![]).await;
    store
        .defer(&old, "PR_PROVIDER_UNAVAILABLE", None)
        .await
        .unwrap();
    for sql in [
        "UPDATE jobs SET state='QUEUED' WHERE operation_id=$1",
        "UPDATE jobs SET max_attempts=max_attempts+8 WHERE operation_id=$1",
        "UPDATE git_operations SET state='PENDING' WHERE id=$1",
    ] {
        assert!(
            sqlx::query(sql)
                .bind(old.operation_id())
                .execute(&pool)
                .await
                .is_err()
        );
    }
    sqlx::query("CREATE FUNCTION reject_recovery() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='PR_SYNC_RECOVERY_QUEUED' THEN RAISE EXCEPTION 'synthetic'; END IF; RETURN NEW; END $$").execute(&pool).await.unwrap();
    sqlx::query("CREATE TRIGGER reject_recovery BEFORE INSERT ON audit_events FOR EACH ROW EXECUTE FUNCTION reject_recovery()").execute(&pool).await.unwrap();
    let req = request(&old);
    assert!(recover(&store, admin, &old, &req).await.is_err());
    assert_eq!(recovery_count(&pool).await, 0);
    assert_eq!(state(&pool, &old).await, ("FAILED".into(), "FAILED".into()));
    sqlx::query("DROP TRIGGER reject_recovery ON audit_events")
        .execute(&pool)
        .await
        .unwrap();
    recover(&store, admin, &old, &req).await.unwrap();
    for sql in [
        "DELETE FROM pr_sync_recoveries WHERE operation_id=$1",
        "UPDATE pr_sync_recoveries SET mode='OBSERVE_STARTED' WHERE operation_id=$1",
    ] {
        assert!(
            sqlx::query(sql)
                .bind(old.operation_id())
                .execute(&pool)
                .await
                .is_err()
        );
    }
}
