use super::*;
use serde_json::json;
use specspace_infrastructure::review_checks::{
    CheckDispatchWork as D, CheckStoreError as E, ReviewCheckDispatcher,
};
#[path = "dispatch/evidence.rs"]
mod evidence;
struct Frame {
    f: Fixture,
    git: Arc<Source>,
    snapshot: Uuid,
    event: Uuid,
    store: Arc<PgReviewChecks>,
}
async fn prepared(pool: &PgPool, approved: bool) -> Frame {
    let (f, git, snapshot, store) = super::super::check_admission::prepared(pool, approved).await;
    let event=sqlx::query_scalar("SELECT id FROM outbox WHERE entity_id=$1 AND event_type='REVIEW_SNAPSHOT_PUBLISHED' AND payload->>'snapshot_id'=$2")
        .bind(f.proposal).bind(snapshot.to_string()).fetch_one(pool).await.unwrap();
    Frame {
        f,
        git,
        snapshot,
        event,
        store,
    }
}
fn provider(pool: &PgPool, f: &Frame) -> Arc<Provider> {
    Arc::new(Provider {
        run_id: 81,
        rows: Mutex::new(vec![]),
        creates: AtomicUsize::new(0),
        updates: AtomicUsize::new(0),
        lists: AtomicUsize::new(0),
        mode: AtomicUsize::new(0),
        pool: pool.clone(),
        actor: f.f.owner,
        operation: f.event,
        git: f.git.clone(),
        started: tokio::sync::Notify::new(),
    })
}
async fn dispatch_state(pool: &PgPool, event: Uuid) -> (String, String, i32) {
    sqlx::query_as("SELECT state,reason,attempts FROM review_check_dispatch WHERE event_id=$1")
        .bind(event)
        .fetch_one(pool)
        .await
        .unwrap()
}
#[sqlx::test(migrations = "../../migrations")]
async fn saved_snapshot_dispatches_and_publishes_one_pending_or_success_check_without_approval_or_merge(
    pool: PgPool,
) {
    for approved in [false, true] {
        let f = prepared(&pool, approved).await;
        let dispatcher = ReviewCheckDispatcher::new(f.store.clone());
        let job = match dispatcher.run_once().await.unwrap() {
            D::Enqueued(event, job) => {
                assert_eq!(event, f.event);
                job
            }
            v => panic!("{v:?}"),
        };
        // The two real approval events also become delivery candidates. While
        // the snapshot check owns the lane they retain independent timed retries.
        if approved {
            for _ in 0..2 {
                assert!(matches!(
                    dispatcher.run_once().await.unwrap(),
                    D::Deferred(_)
                ));
            }
        }
        assert_eq!(dispatcher.run_once().await.unwrap(), D::Idle);
        assert_eq!(
            dispatch_state(&pool, f.event).await,
            ("ENQUEUED".into(), "QUEUED".into(), 1)
        );
        let port = provider(&pool, &f);
        let worker = ReviewCheckWorker::new(
            f.store.clone(),
            Arc::new(Factory(port.clone())),
            Duration::from_secs(60),
        )
        .unwrap();
        assert_eq!(worker.run_once().await.unwrap(), CheckWork::Completed(job));
        assert_eq!(port.creates.load(Ordering::SeqCst), 1);
        assert_eq!(port.updates.load(Ordering::SeqCst), 1);
        assert_eq!(
            port.rows.lock().unwrap()[0].state,
            if approved {
                CheckState::Success
            } else {
                CheckState::Pending
            }
        );
        let(source,worker,attempt):(Uuid,Uuid,i32)=sqlx::query_as("SELECT source_event,dispatch_worker,dispatch_attempt FROM review_check_intents WHERE operation_id=$1").bind(f.event).fetch_one(&pool).await.unwrap();
        assert_eq!(source, f.event);
        assert!(!worker.is_nil());
        assert_eq!(attempt, 1);
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT state FROM proposals WHERE id=$1")
                .bind(f.f.proposal)
                .fetch_one(&pool)
                .await
                .unwrap(),
            "IN_REVIEW"
        );
        assert!(matches!(
            f.store
                .evaluate(f.f.owner, f.f.proposal, f.snapshot, f.event)
                .await,
            Err(E::IdempotencyMismatch)
        ));
    }
    assert_eq!(count(&pool, "review_check_results").await, 2);
}
#[sqlx::test(migrations = "../../migrations")]
async fn check_and_pr_consumers_each_deliver_the_publication_in_either_order(pool: PgPool) {
    use specspace_application::pr_sync::PrSiteOrigin;
    use specspace_infrastructure::pr_sync::{PgPrSync, PrDispatchWork, PrSyncDispatcher};
    for pr_first in [false, true] {
        let f = prepared(&pool, false).await;
        let pr = PrSyncDispatcher::new(
            Arc::new(PgPrSync::new(pool.clone())),
            PrSiteOrigin::parse("https://specs.example", false).unwrap(),
        );
        let check = ReviewCheckDispatcher::new(f.store.clone());
        if pr_first {
            assert!(
                matches!(pr.run_once().await.unwrap(),PrDispatchWork::Enqueued(event,_) if event==f.event)
            );
        }
        let before: Option<String> =
            sqlx::query_scalar("SELECT sent_at::text FROM outbox WHERE id=$1")
                .bind(f.event)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(matches!(check.run_once().await.unwrap(),D::Enqueued(event,_) if event==f.event));
        assert_eq!(
            sqlx::query_scalar::<_, Option<String>>("SELECT sent_at::text FROM outbox WHERE id=$1")
                .bind(f.event)
                .fetch_one(&pool)
                .await
                .unwrap(),
            before
        );
        if !pr_first {
            assert!(
                matches!(pr.run_once().await.unwrap(),PrDispatchWork::Enqueued(event,_) if event==f.event)
            );
        }
        assert_eq!(check.run_once().await.unwrap(), D::Idle);
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM review_check_intents WHERE source_event=$1"
            )
            .bind(f.event)
            .fetch_one(&pool)
            .await
            .unwrap(),
            1
        );
    }
}
#[sqlx::test(migrations = "../../migrations")]
async fn forged_publications_are_quarantined_without_reading_provider_data_or_changing_delivery(
    pool: PgPool,
) {
    let f = prepared(&pool, false).await;
    let dispatcher = ReviewCheckDispatcher::new(f.store.clone());
    assert!(matches!(
        dispatcher.run_once().await.unwrap(),
        D::Enqueued(_, _)
    ));
    let reads = f.git.reads.load(Ordering::SeqCst);
    for payload in [
        json!({"snapshot_id":f.snapshot,"operation_id":Uuid::new_v4(),"pr_sync_required":true,"approvals_granted":false,"activated":true}),
        json!({"snapshot_id":"not-a-uuid","source":"PRIVATE SOURCE SENTINEL"}),
    ] {
        let event = Uuid::new_v4();
        sqlx::query("INSERT INTO outbox(id,organization_id,entity_id,event_type,payload) VALUES($1,$2,$3,'REVIEW_SNAPSHOT_PUBLISHED',$4)")
            .bind(event).bind(f.f.org).bind(f.f.proposal).bind(payload).execute(&pool).await.unwrap();
        assert_eq!(dispatcher.run_once().await.unwrap(), D::Rejected(event));
        assert_eq!(dispatch_state(&pool, event).await.1, "PUBLICATION_INVALID");
        assert!(
            sqlx::query_scalar::<_, bool>("SELECT sent_at IS NULL FROM outbox WHERE id=$1")
                .bind(event)
                .fetch_one(&pool)
                .await
                .unwrap()
        );
    }
    assert_eq!(f.git.reads.load(Ordering::SeqCst), reads);
    assert_eq!(count(&pool, "review_check_intents").await, 1);
}
#[sqlx::test(migrations = "../../migrations")]
async fn superseded_snapshots_skip_without_provider_io_or_reopening_the_proposal(pool: PgPool) {
    let f = prepared(&pool, false).await;
    let reads = f.git.reads.load(Ordering::SeqCst);
    sqlx::query("UPDATE proposals SET state='CLOSED' WHERE id=$1")
        .bind(f.f.proposal)
        .execute(&pool)
        .await
        .unwrap();
    let dispatcher = ReviewCheckDispatcher::new(f.store);
    assert_eq!(dispatcher.run_once().await.unwrap(), D::Skipped(f.event));
    assert_eq!(dispatcher.run_once().await.unwrap(), D::Idle);
    assert_eq!(f.git.reads.load(Ordering::SeqCst), reads);
    assert_eq!(count(&pool, "review_check_intents").await, 0);
}
#[sqlx::test(migrations = "../../migrations")]
async fn missing_setup_or_revoked_publisher_defers_without_provider_reads_or_fake_authority(
    pool: PgPool,
) {
    for revoke in [false, true] {
        let f = prepared(&pool, false).await;
        let reads = f.git.reads.load(Ordering::SeqCst);
        if revoke {
            sqlx::query("UPDATE memberships SET role='READER' WHERE user_id=$1")
                .bind(f.f.owner)
                .execute(&pool)
                .await
                .unwrap();
        } else {
            sqlx::query("UPDATE repositories SET check_app_id=NULL WHERE id=$1")
                .bind(f.f.repository)
                .execute(&pool)
                .await
                .unwrap();
        }
        let dispatcher = ReviewCheckDispatcher::new(f.store);
        assert_eq!(dispatcher.run_once().await.unwrap(), D::Deferred(f.event));
        assert_eq!(dispatcher.run_once().await.unwrap(), D::Idle);
        assert_eq!(
            dispatch_state(&pool, f.event).await.1,
            if revoke {
                "AUTHORITY_REQUIRED"
            } else {
                "SETUP_REQUIRED"
            }
        );
        assert_eq!(f.git.reads.load(Ordering::SeqCst), reads);
    }
    assert_eq!(count(&pool, "review_check_intents").await, 0);
}
#[sqlx::test(migrations = "../../migrations")]
async fn transient_provider_backoff_is_persistent_and_retries_the_same_source_operation(
    pool: PgPool,
) {
    let f = prepared(&pool, false).await;
    *f.git.failure.lock().unwrap() = Some(GitError::RateLimited {
        retry_after_seconds: Some(2),
    });
    let dispatcher = ReviewCheckDispatcher::new(f.store.clone());
    assert_eq!(dispatcher.run_once().await.unwrap(), D::Deferred(f.event));
    let reads = f.git.reads.load(Ordering::SeqCst);
    assert_eq!(dispatcher.run_once().await.unwrap(), D::Idle);
    assert_eq!(f.git.reads.load(Ordering::SeqCst), reads);
    assert!(matches!(
        f.store
            .evaluate(f.f.owner, f.f.proposal, f.snapshot, Uuid::new_v4())
            .await,
        Err(E::Cooldown(1..=2))
    ));
    *f.git.failure.lock().unwrap() = None;
    tokio::time::sleep(Duration::from_millis(2100)).await;
    assert!(matches!(dispatcher.run_once().await.unwrap(),D::Enqueued(event,_) if event==f.event));
    assert_eq!(dispatch_state(&pool, f.event).await.2, 2);
    assert_eq!(count(&pool, "review_check_intents").await, 1);
}
#[sqlx::test(migrations = "../../migrations")]
async fn duplicate_dispatchers_cannot_claim_the_same_live_publication_and_io_holds_no_transaction(
    pool: PgPool,
) {
    let f = prepared(&pool, false).await;
    let _ = tokio::time::timeout(Duration::from_millis(5), f.git.started.notified()).await;
    f.git.blocked.store(true, Ordering::SeqCst);
    let first = ReviewCheckDispatcher::new(f.store.clone());
    let second = ReviewCheckDispatcher::new(f.store.clone());
    let running = tokio::spawn(async move { first.run_once().await });
    tokio::time::timeout(Duration::from_secs(10), f.git.started.notified())
        .await
        .unwrap();
    assert_eq!(second.run_once().await.unwrap(), D::Idle);
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND state LIKE 'idle in transaction%'").fetch_one(&pool).await.unwrap(),0);
    let mut lock = pool.begin().await.unwrap();
    sqlx::query("SET LOCAL lock_timeout='1s'")
        .execute(&mut *lock)
        .await
        .unwrap();
    sqlx::query("SELECT id FROM proposals WHERE id=$1 FOR UPDATE")
        .bind(f.f.proposal)
        .execute(&mut *lock)
        .await
        .unwrap();
    sqlx::query("SELECT event_id FROM review_check_dispatch WHERE event_id=$1 FOR UPDATE")
        .bind(f.event)
        .execute(&mut *lock)
        .await
        .unwrap();
    lock.commit().await.unwrap();
    f.git.blocked.store(false, Ordering::SeqCst);
    f.git.release.notify_one();
    assert!(matches!(running.await.unwrap().unwrap(),D::Enqueued(event,_) if event==f.event));
    assert_eq!(count(&pool, "review_check_intents").await, 1);
}
#[sqlx::test(migrations = "../../migrations")]
async fn delivery_acknowledgement_failure_rolls_back_the_check_reservation_and_preserves_the_source(
    pool: PgPool,
) {
    let f = prepared(&pool, false).await;
    sqlx::raw_sql("CREATE FUNCTION reject_check_delivery() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='CHECK_OUTBOX_DISPATCH' AND NEW.result LIKE 'ENQUEUED:%' THEN RAISE EXCEPTION 'synthetic failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_check_delivery BEFORE INSERT ON audit_events FOR EACH ROW EXECUTE FUNCTION reject_check_delivery();").execute(&pool).await.unwrap();
    let dispatcher = ReviewCheckDispatcher::new(f.store);
    assert_eq!(dispatcher.run_once().await.unwrap(), D::Deferred(f.event));
    assert_eq!(count(&pool, "review_check_intents").await, 0);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM git_operations WHERE id=$1")
            .bind(f.event)
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        dispatch_state(&pool, f.event).await.1,
        "JOURNAL_UNAVAILABLE"
    );
    assert!(
        sqlx::query_scalar::<_, bool>("SELECT sent_at IS NULL FROM outbox WHERE id=$1")
            .bind(f.event)
            .fetch_one(&pool)
            .await
            .unwrap()
    );
}

async fn short_lease(pool: &PgPool, f: &Frame) -> Uuid {
    let worker = Uuid::new_v4();
    let audit = Uuid::new_v4();
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("INSERT INTO review_check_dispatch(event_id,organization_id,proposal_id,state,reason,attempts,lease_owner,leased_until,audit_id) VALUES($1,$2,$3,'RUNNING','CLAIMED',1,$4,clock_timestamp()+interval '100 milliseconds',$5)")
        .bind(f.event).bind(f.f.org).bind(f.f.proposal).bind(worker).bind(audit).execute(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO audit_events(id,organization_id,action,entity_id,after_ref,result) VALUES($1,$2,'CHECK_OUTBOX_DISPATCH',$3,$4,'RUNNING:CLAIMED:1')")
        .bind(audit).bind(f.f.org).bind(f.f.proposal).bind(f.event.to_string()).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    tokio::time::sleep(Duration::from_millis(110)).await;
    worker
}
async fn stale_insert(pool: &PgPool, f: &Frame, worker: Uuid, attempt: i32) {
    let mut tx = pool.begin().await.unwrap();
    let hash = "f".repeat(64);
    sqlx::query("INSERT INTO git_operations(id,organization_id,proposal_id,actor_id,kind,idempotency_key,request_hash,expected_sha) SELECT $1,$2,$3,$4,'REVIEW_CHECK',$1::text,$5,head_sha FROM review_snapshots WHERE id=$6")
        .bind(f.event).bind(f.f.org).bind(f.f.proposal).bind(f.f.owner).bind(&hash).bind(f.snapshot).execute(&mut *tx).await.unwrap();
    let job = Uuid::new_v4();
    sqlx::query("INSERT INTO jobs(id,organization_id,operation_id,kind,payload,max_attempts) VALUES($1,$2,$3,'PUBLISH_REVIEW_CHECK',jsonb_build_object('operation_id',$3::uuid),8)")
        .bind(job).bind(f.f.org).bind(f.event).execute(&mut *tx).await.unwrap();
    let error=sqlx::query("INSERT INTO review_check_intents(operation_id,job_id,organization_id,proposal_id,repository_id,actor_id,snapshot_id,remote_id,installation_id,app_id,branch,head_sha,target_sha,merge_base_sha,proposal_version,configuration_version,policy_version,fingerprint,evidence_digest,authority_digest,request_hash,purpose,result,report_digest,audit_id,event_id,source_event,dispatch_worker,dispatch_attempt)
        SELECT $1,$2,h.organization_id,h.proposal_id,h.repository_id,$3,h.id,42,7,7,h.branch,h.head_sha,h.target_sha,h.merge_base_sha,p.version,h.configuration_version,h.policy_version,h.fingerprint,h.evidence_digest,$4,$4,'EVALUATION','PENDING',$4,$5,$6,$1,$7,$8 FROM review_snapshots h JOIN proposals p ON p.id=h.proposal_id AND p.organization_id=h.organization_id WHERE h.id=$9")
        .bind(f.event).bind(job).bind(f.f.owner).bind(hash).bind(Uuid::new_v4()).bind(Uuid::new_v4()).bind(worker).bind(attempt).bind(f.snapshot).execute(&mut *tx).await.unwrap_err();
    assert!(error.to_string().contains("live delivery fence"), "{error}");
    tx.rollback().await.unwrap();
}
#[sqlx::test(migrations = "../../migrations")]
async fn expired_delivery_reclaims_the_next_attempt_and_rejects_old_owner_or_attempt_at_the_database_boundary(
    pool: PgPool,
) {
    let f = prepared(&pool, false).await;
    let old = short_lease(&pool, &f).await;
    let _ = tokio::time::timeout(Duration::from_millis(5), f.git.started.notified()).await;
    f.git.blocked.store(true, Ordering::SeqCst);
    let dispatcher = ReviewCheckDispatcher::new(f.store.clone());
    let running = tokio::spawn(async move { dispatcher.run_once().await });
    tokio::time::timeout(Duration::from_secs(10), f.git.started.notified())
        .await
        .unwrap();
    let (owner, attempt): (Uuid, i32) =
        sqlx::query_as("SELECT lease_owner,attempts FROM review_check_dispatch WHERE event_id=$1")
            .bind(f.event)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_ne!(owner, old);
    assert_eq!(attempt, 2);
    stale_insert(&pool, &f, old, 2).await;
    stale_insert(&pool, &f, owner, 1).await;
    f.git.blocked.store(false, Ordering::SeqCst);
    f.git.release.notify_one();
    assert!(matches!(running.await.unwrap().unwrap(),D::Enqueued(event,_) if event==f.event));
    assert_eq!(count(&pool, "review_check_intents").await, 1);
    for sql in [
        "UPDATE review_check_dispatch SET state='WAITING',next_at=clock_timestamp()",
        "DELETE FROM review_check_dispatch",
        "TRUNCATE review_check_dispatch CASCADE",
        "UPDATE outbox SET payload='{}' WHERE event_type='REVIEW_SNAPSHOT_PUBLISHED'",
    ] {
        assert!(sqlx::query(sql).execute(&pool).await.is_err(), "{sql}");
    }
}
#[sqlx::test(migrations = "../../migrations")]
async fn revocation_or_context_change_during_provider_reads_cannot_publish_a_delivery_reservation(
    pool: PgPool,
) {
    for revoke in [false, true] {
        let f = prepared(&pool, false).await;
        let _ = tokio::time::timeout(Duration::from_millis(5), f.git.started.notified()).await;
        f.git.blocked.store(true, Ordering::SeqCst);
        let dispatcher = ReviewCheckDispatcher::new(f.store.clone());
        let running = tokio::spawn(async move { dispatcher.run_once().await });
        tokio::time::timeout(Duration::from_secs(10), f.git.started.notified())
            .await
            .unwrap();
        if revoke {
            sqlx::query("UPDATE memberships SET role='READER' WHERE user_id=$1")
                .bind(f.f.owner)
                .execute(&pool)
                .await
                .unwrap();
        } else {
            sqlx::query("UPDATE proposals SET state='CLOSED' WHERE id=$1")
                .bind(f.f.proposal)
                .execute(&pool)
                .await
                .unwrap();
        }
        f.git.blocked.store(false, Ordering::SeqCst);
        f.git.release.notify_one();
        let result = running.await.unwrap().unwrap();
        assert!(
            matches!(result, D::Deferred(_) | D::Skipped(_)),
            "{result:?}"
        );
    }
    assert_eq!(count(&pool, "review_check_intents").await, 0);
}
#[sqlx::test(migrations = "../../migrations")]
async fn shutdown_cancels_the_read_only_attempt_and_preserves_the_lease_for_expiry_recovery(
    pool: PgPool,
) {
    let f = prepared(&pool, false).await;
    let _ = tokio::time::timeout(Duration::from_millis(5), f.git.started.notified()).await;
    f.git.blocked.store(true, Ordering::SeqCst);
    let dispatcher = ReviewCheckDispatcher::new(f.store.clone());
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let running = tokio::spawn(async move { dispatcher.run(stopped).await });
    tokio::time::timeout(Duration::from_secs(10), f.git.started.notified())
        .await
        .unwrap();
    stop.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(2), running)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(dispatch_state(&pool, f.event).await.0, "RUNNING");
    assert_eq!(count(&pool, "review_check_intents").await, 0);
    assert_eq!(
        ReviewCheckDispatcher::new(f.store)
            .run_once()
            .await
            .unwrap(),
        D::Idle
    );
}
