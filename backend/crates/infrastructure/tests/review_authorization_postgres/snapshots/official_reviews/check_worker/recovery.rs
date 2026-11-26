use super::*;
use specspace_infrastructure::review_checks::{
    CheckLease, CheckRecoveryMode as M, CheckStoreError as E,
};
struct Frame {
    store: Arc<PgReviewChecks>,
    provider: Arc<Provider>,
    worker: ReviewCheckWorker,
    admin: Uuid,
    proposal: Uuid,
    job: Uuid,
    hash: String,
    lease: CheckLease,
}
async fn stopped(pool: &PgPool, started: bool) -> Frame {
    let (store, provider, worker, job, proposal) = setup_worker(pool, true, 0).await;
    let org = sqlx::query_scalar("SELECT organization_id FROM proposals WHERE id=$1")
        .bind(proposal)
        .fetch_one(pool)
        .await
        .unwrap();
    let admin = user(pool, org, "ADMIN", vec![]).await;
    let lease = store.claim(Uuid::new_v4(), 120).await.unwrap().unwrap();
    if started {
        store.start_write(&lease, &[], true).await.unwrap();
        provider
            .create_check(&lease.intent().scope(), provider.operation)
            .await
            .unwrap();
    }
    let hash = lease.intent().request_hash().to_owned();
    assert!(
        !store
            .defer(&lease, "OPERATOR_STOPPED", None, None)
            .await
            .unwrap()
    );
    Frame {
        store,
        provider,
        worker,
        admin,
        proposal,
        job,
        hash,
        lease,
    }
}
#[sqlx::test(migrations = "../../migrations")]
async fn admin_recovery_retries_unstarted_or_observes_started_without_replacing_the_intent(
    pool: PgPool,
) {
    for started in [false, true] {
        let f = stopped(&pool, started).await;
        let request = Uuid::new_v4();
        let accepted = f
            .store
            .recover(f.admin, f.proposal, f.provider.operation, request, &f.hash)
            .await
            .unwrap();
        assert_eq!(accepted.job_id, f.job);
        assert_eq!(accepted.max_attempts, 16);
        assert_eq!(
            accepted.mode,
            if started {
                M::ObserveStarted
            } else {
                M::RetryUnstarted
            }
        );
        assert!(!accepted.replayed);
        assert!(matches!(
            f.store.heartbeat(&f.lease, 120).await,
            Err(E::LeaseLost)
        ));
        assert_eq!(
            f.worker.run_once().await.unwrap(),
            CheckWork::Completed(f.job)
        );
        assert_eq!(f.provider.creates.load(Ordering::SeqCst), 1);
        assert_eq!(f.provider.updates.load(Ordering::SeqCst), 1);
        let replay = f
            .store
            .recover(f.admin, f.proposal, f.provider.operation, request, &f.hash)
            .await
            .unwrap();
        assert!(replay.replayed);
        assert_eq!(replay.max_attempts, 16);
        assert_eq!(
            sqlx::query_scalar::<_, i32>("SELECT attempts FROM jobs WHERE id=$1")
                .bind(f.job)
                .fetch_one(&pool)
                .await
                .unwrap(),
            2
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT request_hash FROM review_check_intents WHERE operation_id=$1"
            )
            .bind(f.provider.operation)
            .fetch_one(&pool)
            .await
            .unwrap(),
            f.hash
        );
    }
    assert_eq!(count(&pool, "review_check_recoveries").await, 2);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM outbox WHERE event_type='REVIEW_CHECK_RECOVERY_QUEUED'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        2
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn stale_unstarted_work_cannot_be_rearmed_but_started_identity_can_be_observed_after_revocation(
    pool: PgPool,
) {
    for started in [false, true] {
        let f = stopped(&pool, started).await;
        sqlx::query("UPDATE memberships SET role='READER' WHERE user_id=$1")
            .bind(f.provider.actor)
            .execute(&pool)
            .await
            .unwrap();
        let result = f
            .store
            .recover(
                f.admin,
                f.proposal,
                f.provider.operation,
                Uuid::new_v4(),
                &f.hash,
            )
            .await;
        if started {
            let accepted = result.unwrap();
            assert_eq!(accepted.mode, M::ObserveStarted);
            assert_eq!(
                f.worker.run_once().await.unwrap(),
                CheckWork::Stopped(f.job)
            );
            assert_eq!(count(&pool, "review_check_runs").await, 1);
            assert_eq!(f.provider.creates.load(Ordering::SeqCst), 1);
            assert_eq!(f.provider.updates.load(Ordering::SeqCst), 0);
        } else {
            assert!(matches!(result, Err(E::Stale) | Err(E::Forbidden)));
        }
    }
    assert_eq!(count(&pool, "review_check_results").await, 0);
}
#[sqlx::test(migrations = "../../migrations")]
async fn recovery_is_exact_admin_scoped_and_cannot_displace_active_or_completed_work(pool: PgPool) {
    let f = stopped(&pool, false).await;
    assert!(matches!(
        f.store
            .recover(
                f.provider.actor,
                f.proposal,
                f.provider.operation,
                Uuid::new_v4(),
                &f.hash
            )
            .await,
        Err(E::Forbidden)
    ));
    assert!(matches!(
        f.store
            .recover(
                f.admin,
                f.proposal,
                f.provider.operation,
                Uuid::new_v4(),
                &"0".repeat(64)
            )
            .await,
        Err(E::Stale)
    ));
    assert!(matches!(
        f.store
            .recover(
                f.admin,
                f.proposal,
                f.provider.operation,
                Uuid::nil(),
                &f.hash
            )
            .await,
        Err(E::Invalid)
    ));
    let request = Uuid::new_v4();
    f.store
        .recover(f.admin, f.proposal, f.provider.operation, request, &f.hash)
        .await
        .unwrap();
    assert!(matches!(
        f.store
            .recover(
                f.admin,
                f.proposal,
                f.provider.operation,
                request,
                &"0".repeat(64)
            )
            .await,
        Err(E::IdempotencyMismatch)
    ));
    assert!(matches!(
        f.store
            .recover(
                f.admin,
                f.proposal,
                f.provider.operation,
                Uuid::new_v4(),
                &f.hash
            )
            .await,
        Err(E::Busy)
    ));
    assert_eq!(
        f.worker.run_once().await.unwrap(),
        CheckWork::Completed(f.job)
    );
    assert!(matches!(
        f.store
            .recover(
                f.admin,
                f.proposal,
                f.provider.operation,
                Uuid::new_v4(),
                &f.hash
            )
            .await,
        Err(E::Busy)
    ));
    sqlx::query("UPDATE memberships SET role='MAINTAINER' WHERE user_id=$1")
        .bind(f.admin)
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        f.store
            .recover(f.admin, f.proposal, f.provider.operation, request, &f.hash)
            .await,
        Err(E::Forbidden)
    ));
}
#[sqlx::test(migrations = "../../migrations")]
async fn recovery_rolls_back_if_audit_or_event_is_missing_and_rows_are_immutable(pool: PgPool) {
    let f = stopped(&pool, true).await;
    sqlx::raw_sql("CREATE FUNCTION reject_check_recovery() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='REVIEW_CHECK_RECOVERY_QUEUED' THEN RAISE EXCEPTION 'synthetic failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_check_recovery BEFORE INSERT ON audit_events FOR EACH ROW EXECUTE FUNCTION reject_check_recovery();").execute(&pool).await.unwrap();
    let request = Uuid::new_v4();
    assert!(matches!(
        f.store
            .recover(f.admin, f.proposal, f.provider.operation, request, &f.hash)
            .await,
        Err(E::Unavailable)
    ));
    assert_eq!(count(&pool, "review_check_recoveries").await, 0);
    sqlx::query("DROP TRIGGER reject_check_recovery ON audit_events")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql("CREATE FUNCTION reject_recovery_event() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.event_type='REVIEW_CHECK_RECOVERY_QUEUED' THEN RAISE EXCEPTION 'synthetic failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_recovery_event BEFORE INSERT ON outbox FOR EACH ROW EXECUTE FUNCTION reject_recovery_event();").execute(&pool).await.unwrap();
    assert!(matches!(
        f.store
            .recover(f.admin, f.proposal, f.provider.operation, request, &f.hash)
            .await,
        Err(E::Unavailable)
    ));
    assert_eq!(count(&pool, "review_check_recoveries").await, 0);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM audit_events WHERE action='REVIEW_CHECK_RECOVERY_QUEUED'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        0
    );
    sqlx::query("DROP TRIGGER reject_recovery_event ON outbox")
        .execute(&pool)
        .await
        .unwrap();
    f.store
        .recover(f.admin, f.proposal, f.provider.operation, request, &f.hash)
        .await
        .unwrap();
    for sql in [
        "UPDATE review_check_recoveries SET mode='RETRY_UNSTARTED'",
        "DELETE FROM review_check_recoveries",
        "TRUNCATE review_check_recoveries CASCADE",
        "UPDATE outbox SET payload='{}' WHERE event_type='REVIEW_CHECK_RECOVERY_QUEUED'",
    ] {
        assert!(sqlx::query(sql).execute(&pool).await.is_err(), "{sql}");
    }
    assert!(
        sqlx::query("UPDATE jobs SET max_attempts=max_attempts+8 WHERE id=$1")
            .bind(f.job)
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("UPDATE jobs SET attempts=0 WHERE id=$1")
            .bind(f.job)
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("UPDATE git_operations SET state='PENDING' WHERE id=$1")
            .bind(f.provider.operation)
            .execute(&pool)
            .await
            .is_err()
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn recovery_quota_is_shared_by_requests_and_replay_is_free(pool: PgPool) {
    let f = stopped(&pool, true).await;
    let first = Uuid::new_v4();
    for request in [first, Uuid::new_v4()] {
        f.store
            .recover(f.admin, f.proposal, f.provider.operation, request, &f.hash)
            .await
            .unwrap();
        let lease = f.store.claim(Uuid::new_v4(), 120).await.unwrap().unwrap();
        assert!(
            !f.store
                .defer(&lease, "OBSERVATION_UNAVAILABLE", None, None)
                .await
                .unwrap()
        );
    }
    assert!(matches!(
        f.store
            .recover(
                f.admin,
                f.proposal,
                f.provider.operation,
                Uuid::new_v4(),
                &f.hash
            )
            .await,
        Err(E::Quota)
    ));
    let replay = f
        .store
        .recover(f.admin, f.proposal, f.provider.operation, first, &f.hash)
        .await
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.max_attempts, 16);
    assert_eq!(
        sqlx::query_scalar::<_, i32>("SELECT max_attempts FROM jobs WHERE id=$1")
            .bind(f.job)
            .fetch_one(&pool)
            .await
            .unwrap(),
        24
    );
    assert_eq!(count(&pool, "review_check_writes").await, 1);
}
#[sqlx::test(migrations = "../../migrations")]
async fn concurrent_exact_recoveries_win_once_and_changed_bindings_stop_new_admission(
    pool: PgPool,
) {
    let f = stopped(&pool, true).await;
    sqlx::query("UPDATE repositories SET check_app_id=8 WHERE id=$1")
        .bind(f.lease.intent().source().repository.repository)
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        f.store
            .recover(
                f.admin,
                f.proposal,
                f.provider.operation,
                Uuid::new_v4(),
                &f.hash
            )
            .await,
        Err(E::Stale)
    ));
    sqlx::query("UPDATE repositories SET check_app_id=7 WHERE id=$1")
        .bind(f.lease.intent().source().repository.repository)
        .execute(&pool)
        .await
        .unwrap();
    let request = Uuid::new_v4();
    let (a, b) = tokio::join!(
        f.store
            .recover(f.admin, f.proposal, f.provider.operation, request, &f.hash),
        f.store
            .recover(f.admin, f.proposal, f.provider.operation, request, &f.hash)
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_ne!(a.replayed, b.replayed);
    assert_eq!(a.job_id, b.job_id);
    assert_eq!(count(&pool, "review_check_recoveries").await, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn failed_unstarted_invalidation_recovers_its_same_failure_only_intent(pool: PgPool) {
    let (store, provider, worker, job, proposal) = setup_worker(&pool, true, 0).await;
    assert_eq!(worker.run_once().await.unwrap(), CheckWork::Completed(job));
    let org = sqlx::query_scalar("SELECT organization_id FROM proposals WHERE id=$1")
        .bind(proposal)
        .fetch_one(&pool)
        .await
        .unwrap();
    let admin = user(&pool, org, "ADMIN", vec![]).await;
    let ticket = store
        .invalidate(
            admin,
            proposal,
            provider.operation,
            specspace_application::review_check_publication::CheckInvalidation::Superseded,
            Uuid::new_v4(),
        )
        .await
        .unwrap();
    let lease = store.claim(Uuid::new_v4(), 120).await.unwrap().unwrap();
    store
        .defer(&lease, "PROVIDER_SETUP_REQUIRED", None, None)
        .await
        .unwrap();
    let recovery = store
        .recover(
            admin,
            proposal,
            ticket.operation_id(),
            Uuid::new_v4(),
            ticket.intent().request_hash(),
        )
        .await
        .unwrap();
    assert_eq!(recovery.mode, M::RetryUnstarted);
    assert_eq!(
        worker.run_once().await.unwrap(),
        CheckWork::Completed(ticket.job_id)
    );
    assert_eq!(provider.creates.load(Ordering::SeqCst), 1);
    assert_eq!(provider.updates.load(Ordering::SeqCst), 2);
    assert_eq!(provider.rows.lock().unwrap()[0].state, CheckState::Failure);
}
#[sqlx::test(migrations = "../../migrations")]
async fn organization_recovery_quota_is_shared_across_admins(pool: PgPool) {
    let f = stopped(&pool, true).await;
    let org = sqlx::query_scalar("SELECT organization_id FROM proposals WHERE id=$1")
        .bind(f.proposal)
        .fetch_one(&pool)
        .await
        .unwrap();
    for _ in 0..6 {
        let admin = user(&pool, org, "ADMIN", vec![]).await;
        for _ in 0..2 {
            f.store
                .recover(
                    admin,
                    f.proposal,
                    f.provider.operation,
                    Uuid::new_v4(),
                    &f.hash,
                )
                .await
                .unwrap();
            let lease = f.store.claim(Uuid::new_v4(), 120).await.unwrap().unwrap();
            f.store
                .defer(&lease, "OBSERVATION_UNAVAILABLE", None, None)
                .await
                .unwrap();
        }
    }
    assert!(matches!(
        f.store
            .recover(
                f.admin,
                f.proposal,
                f.provider.operation,
                Uuid::new_v4(),
                &f.hash
            )
            .await,
        Err(E::Quota)
    ));
    assert_eq!(count(&pool, "review_check_recoveries").await, 12);
    assert_eq!(count(&pool, "review_check_writes").await, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn database_guards_require_the_atomic_recovery_receipt_before_reopening_failed_work(
    pool: PgPool,
) {
    let f = stopped(&pool, false).await;
    assert!(
        sqlx::query("UPDATE jobs SET state='QUEUED',max_attempts=16 WHERE id=$1")
            .bind(f.job)
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("UPDATE git_operations SET state='PENDING' WHERE id=$1")
            .bind(f.provider.operation)
            .execute(&pool)
            .await
            .is_err()
    );
    let org = sqlx::query_scalar::<_, Uuid>("SELECT organization_id FROM proposals WHERE id=$1")
        .bind(f.proposal)
        .fetch_one(&pool)
        .await
        .unwrap();
    let mut partial = pool.begin().await.unwrap();
    let request = Uuid::new_v4();
    let audit = Uuid::new_v4();
    let event = Uuid::new_v4();
    sqlx::query("INSERT INTO review_check_recoveries(organization_id,request_id,proposal_id,operation_id,actor_id,request_hash,mode,previous_attempts,previous_max_attempts,max_attempts,audit_id,event_id) VALUES($1,$2,$3,$4,$5,$6,'RETRY_UNSTARTED',1,8,16,$7,$8)")
        .bind(org).bind(request).bind(f.proposal).bind(f.provider.operation).bind(f.admin).bind(&f.hash).bind(audit).bind(event).execute(&mut *partial).await.unwrap();
    sqlx::query("INSERT INTO audit_events(id,organization_id,actor_id,action,entity_id,after_ref,operation_id,result) VALUES($1,$2,$3,'REVIEW_CHECK_RECOVERY_QUEUED',$4,$5,$6,'RETRY_UNSTARTED')").bind(audit).bind(org).bind(f.admin).bind(f.proposal).bind(request.to_string()).bind(f.provider.operation).execute(&mut *partial).await.unwrap();
    sqlx::query("INSERT INTO outbox(id,organization_id,event_type,entity_id,payload) VALUES($1,$2,'REVIEW_CHECK_RECOVERY_QUEUED',$3,$4)").bind(event).bind(org).bind(f.proposal).bind(serde_json::json!({"operation_id":f.provider.operation,"request_id":request,"mode":"RETRY_UNSTARTED"})).execute(&mut *partial).await.unwrap();
    assert!(partial.commit().await.is_err());
    assert_eq!(count(&pool, "review_check_recoveries").await, 0);
    assert_eq!(
        sqlx::query_scalar::<_, i32>("SELECT max_attempts FROM jobs WHERE id=$1")
            .bind(f.job)
            .fetch_one(&pool)
            .await
            .unwrap(),
        8
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn exhausted_started_job_recovers_at_attempt_nine_and_never_repeats_creation(pool: PgPool) {
    let (store, provider, worker, job, proposal) = setup_worker(&pool, true, 0).await;
    let org = sqlx::query_scalar("SELECT organization_id FROM proposals WHERE id=$1")
        .bind(proposal)
        .fetch_one(&pool)
        .await
        .unwrap();
    let admin = user(&pool, org, "ADMIN", vec![]).await;
    let mut lease = store.claim(Uuid::new_v4(), 120).await.unwrap().unwrap();
    store.start_write(&lease, &[], true).await.unwrap();
    provider
        .create_check(&lease.intent().scope(), provider.operation)
        .await
        .unwrap();
    let hash = lease.intent().request_hash().to_owned();
    for attempt in 1..=8 {
        assert_eq!(lease.attempts(), attempt);
        let again = store
            .defer(&lease, "PROVIDER_OBSERVATION_UNAVAILABLE", Some(1), None)
            .await
            .unwrap();
        assert_eq!(again, attempt < 8);
        if attempt < 8 {
            retry(&pool, job).await;
            lease = store.claim(Uuid::new_v4(), 120).await.unwrap().unwrap();
        }
    }
    assert_eq!(worker.run_once().await.unwrap(), CheckWork::Idle);
    store
        .recover(admin, proposal, provider.operation, Uuid::new_v4(), &hash)
        .await
        .unwrap();
    assert_eq!(worker.run_once().await.unwrap(), CheckWork::Completed(job));
    assert_eq!(
        sqlx::query_scalar::<_, i32>("SELECT attempts FROM jobs WHERE id=$1")
            .bind(job)
            .fetch_one(&pool)
            .await
            .unwrap(),
        9
    );
    assert_eq!(provider.creates.load(Ordering::SeqCst), 1);
    assert_eq!(provider.updates.load(Ordering::SeqCst), 1);
    assert!(matches!(
        store.heartbeat(&lease, 120).await,
        Err(E::LeaseLost)
    ));
}
