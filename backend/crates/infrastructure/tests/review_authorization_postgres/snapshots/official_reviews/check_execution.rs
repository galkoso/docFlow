//! Typed observations model validated check-provider receipts. The journal itself
//! mutates only PostgreSQL and reads current Git refs through the content port.
use super::*;
use specspace_application::review_check_publication::CheckPublicationPhase as P;
use specspace_git_adapter::checks::{CheckRun, CheckRunId, CheckScope, CheckState};
use specspace_infrastructure::review_checks::{CheckLease, CheckStoreError as E};

fn run(lease: &CheckLease, id: u64, state: CheckState) -> CheckRun {
    CheckRun {
        id: CheckRunId::new(id).unwrap(),
        scope: lease.intent().scope(),
        operation: lease.intent().check_operation(),
        state,
    }
}
async fn expire(pool: &PgPool, job: Uuid) {
    sqlx::query("UPDATE jobs SET leased_until=clock_timestamp()-interval '1 second' WHERE id=$1")
        .bind(job)
        .execute(pool)
        .await
        .unwrap();
}
#[sqlx::test(migrations = "../../migrations")]
async fn execution_records_both_phases_and_an_exact_replayable_receipt_without_approving_or_merging(
    pool: PgPool,
) {
    let (f, source, h, store) = super::check_admission::prepared(&pool, true).await;
    let ticket = store
        .evaluate(f.owner, f.proposal, h, Uuid::new_v4())
        .await
        .unwrap();
    let lease = store.claim(Uuid::new_v4(), 120).await.unwrap().unwrap();
    assert_eq!(lease.job_id(), ticket.job_id);
    assert_eq!(store.check(&lease).await.unwrap().phase, P::NotStarted);
    assert_eq!(
        store.start_write(&lease, &[], true).await.unwrap(),
        P::CreateStarted
    );
    assert!(matches!(
        store.start_write(&lease, &[], true).await,
        Err(E::WriteAlreadyStarted)
    ));
    let pending = run(&lease, 81, CheckState::Pending);
    assert!(matches!(
        store
            .complete(&lease, std::slice::from_ref(&pending), true)
            .await,
        Err(E::InvalidReceipt)
    ));
    for (repo, app, head) in [
        (43, 7, lease.intent().source().head.clone()),
        (42, 8, lease.intent().source().head.clone()),
        (42, 7, lease.intent().source().target.clone()),
    ] {
        let wrong = CheckRun {
            id: pending.id,
            scope: CheckScope::new(repo, app, head).unwrap(),
            operation: pending.operation,
            state: CheckState::Pending,
        };
        assert!(matches!(
            store.adopt(&lease, &wrong).await,
            Err(E::InvalidReceipt)
        ));
    }
    let mut wrong_nonce = run(&lease, 81, CheckState::Pending);
    wrong_nonce.operation = Uuid::new_v4();
    assert!(matches!(
        store.adopt(&lease, &wrong_nonce).await,
        Err(E::InvalidReceipt)
    ));
    store.adopt(&lease, &pending).await.unwrap();
    store.adopt(&lease, &pending).await.unwrap();
    assert_eq!(
        store.observe(&lease).await.unwrap().phase,
        P::Created(pending.id)
    );
    assert_eq!(
        store
            .start_write(&lease, std::slice::from_ref(&pending), true)
            .await
            .unwrap(),
        P::UpdateStarted(pending.id)
    );
    let success = run(&lease, 81, CheckState::Success);
    assert!(matches!(
        store
            .complete(&lease, std::slice::from_ref(&success), false)
            .await,
        Err(E::InvalidReceipt)
    ));
    assert!(matches!(
        store
            .complete(
                &lease,
                &[
                    run(&lease, 81, CheckState::Success),
                    run(&lease, 82, CheckState::Success)
                ],
                true
            )
            .await,
        Err(E::InvalidReceipt)
    ));
    store
        .complete(&lease, std::slice::from_ref(&success), true)
        .await
        .unwrap();
    let reads = source.reads.load(Ordering::SeqCst);
    store
        .complete(&lease, std::slice::from_ref(&success), true)
        .await
        .unwrap();
    assert_eq!(source.reads.load(Ordering::SeqCst), reads);
    assert!(matches!(store.observe(&lease).await, Err(E::LeaseLost)));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM review_check_results")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM audit_events WHERE action='REVIEW_CHECK_WRITE_STARTED'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        2
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM audit_events WHERE action='REVIEW_CHECK_RUN_OBSERVED'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM outbox WHERE event_type='REVIEW_CHECK_PUBLISHED'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM proposals WHERE id=$1")
            .bind(f.proposal)
            .fetch_one(&pool)
            .await
            .unwrap(),
        "IN_REVIEW"
    );
    // Damage only this isolated fixture, simulating an incomplete restore. A
    // previously recorded receipt must not hide a substituted run binding.
    sqlx::raw_sql("ALTER TABLE review_check_runs DISABLE TRIGGER USER; UPDATE review_check_runs SET app_id=8; ALTER TABLE review_check_runs ENABLE TRIGGER USER;").execute(&pool).await.unwrap();
    assert!(matches!(
        store
            .complete(&lease, std::slice::from_ref(&success), true)
            .await,
        Err(E::InvalidRecord)
    ));
    assert_eq!(source.reads.load(Ordering::SeqCst), reads);
}
#[sqlx::test(migrations = "../../migrations")]
async fn reclaimed_same_owner_attempt_can_observe_but_never_rearm_creation_or_use_the_old_fence(
    pool: PgPool,
) {
    let (f, _, h, store) = super::check_admission::prepared(&pool, true).await;
    store
        .evaluate(f.owner, f.proposal, h, Uuid::new_v4())
        .await
        .unwrap();
    let worker = Uuid::new_v4();
    let old = store.claim(worker, 120).await.unwrap().unwrap();
    store.start_write(&old, &[], true).await.unwrap();
    expire(&pool, old.job_id()).await;
    assert!(matches!(store.observe(&old).await, Err(E::LeaseLost)));
    let next = store.claim(worker, 120).await.unwrap().unwrap();
    assert_eq!(next.attempts(), 2);
    assert_eq!(store.observe(&next).await.unwrap().phase, P::CreateStarted);
    assert!(matches!(
        store.heartbeat(&old, 120).await,
        Err(E::LeaseLost)
    ));
    assert!(matches!(
        store.adopt(&old, &run(&old, 81, CheckState::Pending)).await,
        Err(E::LeaseLost)
    ));
    assert!(matches!(
        store.defer(&old, "OLD_WORKER", None, None).await,
        Err(E::LeaseLost)
    ));
    assert!(matches!(
        store.start_write(&next, &[], true).await,
        Err(E::WriteAlreadyStarted)
    ));
    let pending = run(&next, 81, CheckState::Pending);
    store.adopt(&next, &pending).await.unwrap();
    store
        .start_write(&next, std::slice::from_ref(&pending), true)
        .await
        .unwrap();
    let success = run(&next, 81, CheckState::Success);
    assert!(matches!(
        store
            .complete(&old, std::slice::from_ref(&success), true)
            .await,
        Err(E::LeaseLost)
    ));
    store
        .complete(&next, std::slice::from_ref(&success), true)
        .await
        .unwrap();
    assert!(matches!(
        store
            .complete(&old, std::slice::from_ref(&success), true)
            .await,
        Err(E::LeaseLost)
    ));
    assert_eq!(
        sqlx::query_scalar::<_, i32>(
            "SELECT fence_attempt FROM review_check_writes WHERE phase='CREATE'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i32>("SELECT fence_attempt FROM review_check_results")
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn revoked_actor_keeps_known_side_effect_identity_but_cannot_send_or_finish_another_write(
    pool: PgPool,
) {
    let (f, _, h, store) = super::check_admission::prepared(&pool, true).await;
    store
        .evaluate(f.owner, f.proposal, h, Uuid::new_v4())
        .await
        .unwrap();
    let lease = store.claim(Uuid::new_v4(), 120).await.unwrap().unwrap();
    store.start_write(&lease, &[], true).await.unwrap();
    sqlx::query("UPDATE users SET status='DISABLED' WHERE id=$1")
        .bind(f.owner)
        .execute(&pool)
        .await
        .unwrap();
    let pending = run(&lease, 81, CheckState::Pending);
    store.adopt(&lease, &pending).await.unwrap();
    store.heartbeat(&lease, 120).await.unwrap();
    assert_eq!(
        store.observe(&lease).await.unwrap().phase,
        P::Created(pending.id)
    );
    assert!(
        store
            .start_write(&lease, std::slice::from_ref(&pending), true)
            .await
            .is_err()
    );
    assert!(
        store
            .complete(&lease, &[run(&lease, 81, CheckState::Success)], true)
            .await
            .is_err()
    );
    assert!(
        !store
            .defer(&lease, "AUTHORITY_LOST", None, None)
            .await
            .unwrap()
    );
    let (job,op):(String,String)=sqlx::query_as("SELECT j.state,o.state FROM jobs j JOIN git_operations o ON o.id=j.operation_id WHERE j.id=$1").bind(lease.job_id()).fetch_one(&pool).await.unwrap();
    assert_eq!((job.as_str(), op.as_str()), ("FAILED", "UNKNOWN"));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM review_check_writes")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM review_check_runs")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM review_check_results")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn changed_provider_target_blocks_a_start_before_any_durable_write_marker(pool: PgPool) {
    let (f, source, h, store) = super::check_admission::prepared(&pool, true).await;
    store
        .evaluate(f.owner, f.proposal, h, Uuid::new_v4())
        .await
        .unwrap();
    let lease = store.claim(Uuid::new_v4(), 120).await.unwrap().unwrap();
    source.target.store(3, Ordering::SeqCst);
    assert!(matches!(
        store.start_write(&lease, &[], true).await,
        Err(E::Fresh(
            specspace_infrastructure::review_readiness::FreshReadinessError::TargetChanged
        ))
    ));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM review_check_writes")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    assert!(
        !store
            .defer(&lease, "TARGET_CHANGED", None, None)
            .await
            .unwrap()
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM git_operations WHERE id=$1")
            .bind(lease.operation_id())
            .fetch_one(&pool)
            .await
            .unwrap(),
        "FAILED"
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn equal_policy_results_cannot_reuse_stale_question_evidence_for_an_update(pool: PgPool) {
    use specspace_application::product_question::CreateProductQuestion;
    use specspace_infrastructure::product_questions::PgProductQuestions;
    let (f, _, h, store) = super::check_admission::prepared(&pool, false).await;
    store
        .evaluate(f.owner, f.proposal, h, Uuid::new_v4())
        .await
        .unwrap();
    let lease = store.claim(Uuid::new_v4(), 120).await.unwrap().unwrap();
    store.start_write(&lease, &[], true).await.unwrap();
    let pending = run(&lease, 81, CheckState::Pending);
    store.adopt(&lease, &pending).await.unwrap();
    PgProductQuestions::new(pool.clone())
        .create(
            f.owner,
            f.proposal,
            CreateProductQuestion {
                request_id: Uuid::new_v4(),
                expected_proposal_version: lease.intent().source().proposal_version,
                text: "Private nonblocking revision".into(),
                blocking: false,
                proposed_answer: None,
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        store
            .start_write(&lease, std::slice::from_ref(&pending), true)
            .await,
        Err(E::Stale)
    ));
    assert!(matches!(
        store
            .complete(&lease, std::slice::from_ref(&pending), true)
            .await,
        Err(E::Stale)
    ));
    assert_eq!(
        store.observe(&lease).await.unwrap().phase,
        P::Created(pending.id)
    );
    assert!(
        !store
            .defer(&lease, "POLICY_CHANGED", None, None)
            .await
            .unwrap()
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM git_operations WHERE id=$1")
            .bind(lease.operation_id())
            .fetch_one(&pool)
            .await
            .unwrap(),
        "UNKNOWN"
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn retry_scheduling_and_connection_cooldown_do_not_erase_or_rearm_effects(pool: PgPool) {
    let (f, source, h, store) = super::check_admission::prepared(&pool, true).await;
    let op = Uuid::new_v4();
    store.evaluate(f.owner, f.proposal, h, op).await.unwrap();
    let lease = store.claim(Uuid::new_v4(), 120).await.unwrap().unwrap();
    assert!(matches!(
        store.defer(&lease, "private body", Some(1), None).await,
        Err(E::Invalid)
    ));
    assert!(
        store
            .defer(&lease, "READ_UNAVAILABLE", Some(7), None)
            .await
            .unwrap()
    );
    assert!(store.claim(Uuid::new_v4(), 120).await.unwrap().is_none());
    sqlx::query("UPDATE jobs SET available_at=clock_timestamp()-interval '1 second' WHERE id=$1")
        .bind(lease.job_id())
        .execute(&pool)
        .await
        .unwrap();
    let next = store.claim(Uuid::new_v4(), 120).await.unwrap().unwrap();
    assert_eq!(store.observe(&next).await.unwrap().phase, P::NotStarted);
    store.start_write(&next, &[], true).await.unwrap();
    assert!(
        store
            .defer(&next, "RESULT_UNKNOWN", Some(7), None)
            .await
            .unwrap()
    );
    sqlx::query("UPDATE jobs SET available_at=clock_timestamp()-interval '1 second' WHERE id=$1")
        .bind(lease.job_id())
        .execute(&pool)
        .await
        .unwrap();
    let observer = store.claim(Uuid::new_v4(), 120).await.unwrap().unwrap();
    assert_eq!(
        store.observe(&observer).await.unwrap().phase,
        P::CreateStarted
    );
    assert!(
        !store
            .defer(&observer, "PROVIDER_RATE_LIMITED", None, Some(17))
            .await
            .unwrap()
    );
    let reads = source.reads.load(Ordering::SeqCst);
    assert!(
        store
            .evaluate(f.owner, f.proposal, h, op)
            .await
            .unwrap()
            .replayed
    );
    assert_eq!(source.reads.load(Ordering::SeqCst), reads);
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM git_operations WHERE id=$1")
            .bind(op)
            .fetch_one(&pool)
            .await
            .unwrap(),
        "UNKNOWN"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM review_check_writes")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn unstarted_provider_cooldown_prevents_new_admission_reads_and_invalid_claims_are_rejected(
    pool: PgPool,
) {
    let (f, source, h, store) = super::check_admission::prepared(&pool, true).await;
    assert!(matches!(
        store.claim(Uuid::nil(), 60).await,
        Err(E::Invalid)
    ));
    assert!(matches!(
        store.claim(Uuid::new_v4(), 301).await,
        Err(E::Invalid)
    ));
    store
        .evaluate(f.owner, f.proposal, h, Uuid::new_v4())
        .await
        .unwrap();
    let lease = store.claim(Uuid::new_v4(), 120).await.unwrap().unwrap();
    assert!(matches!(store.heartbeat(&lease, 0).await, Err(E::Invalid)));
    assert!(
        !store
            .defer(&lease, "PROVIDER_RATE_LIMITED", None, Some(17))
            .await
            .unwrap()
    );
    let reads = source.reads.load(Ordering::SeqCst);
    assert!(
        matches!(store.evaluate(f.owner,f.proposal,h,Uuid::new_v4()).await,Err(E::Cooldown(n)) if (1..=17).contains(&n))
    );
    assert_eq!(source.reads.load(Ordering::SeqCst), reads);
}
#[sqlx::test(migrations = "../../migrations")]
async fn exhausted_attempts_end_automatic_work_but_preserve_started_uncertainty(pool: PgPool) {
    for started in [false, true] {
        let (f, _, h, store) = super::check_admission::prepared(&pool, true).await;
        store
            .evaluate(f.owner, f.proposal, h, Uuid::new_v4())
            .await
            .unwrap();
        let worker = Uuid::new_v4();
        let mut final_lease = None;
        for attempt in 1..=8 {
            let lease = store.claim(worker, 120).await.unwrap().unwrap();
            assert_eq!(lease.attempts(), attempt);
            if started && attempt == 1 {
                store.start_write(&lease, &[], true).await.unwrap();
            }
            expire(&pool, lease.job_id()).await;
            final_lease = Some(lease);
        }
        let final_lease = final_lease.unwrap();
        assert!(store.claim(Uuid::new_v4(), 120).await.unwrap().is_none());
        let (job,op):(String,String)=sqlx::query_as("SELECT j.state,o.state FROM jobs j JOIN git_operations o ON o.id=j.operation_id WHERE j.id=$1").bind(final_lease.job_id()).fetch_one(&pool).await.unwrap();
        assert_eq!(job, "FAILED");
        assert_eq!(op, if started { "UNKNOWN" } else { "FAILED" });
        assert!(matches!(
            store.observe(&final_lease).await,
            Err(E::LeaseLost)
        ));
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM audit_events WHERE operation_id=$1 AND action='REVIEW_CHECK_EXHAUSTED'").bind(final_lease.operation_id()).fetch_one(&pool).await.unwrap(),1);
    }
}
#[sqlx::test(migrations = "../../migrations")]
async fn publication_audit_failure_can_be_reconciled_without_another_write_marker(pool: PgPool) {
    let (f, _, h, store) = super::check_admission::prepared(&pool, true).await;
    store
        .evaluate(f.owner, f.proposal, h, Uuid::new_v4())
        .await
        .unwrap();
    let lease = store.claim(Uuid::new_v4(), 120).await.unwrap().unwrap();
    store.start_write(&lease, &[], true).await.unwrap();
    let pending = run(&lease, 81, CheckState::Pending);
    store.adopt(&lease, &pending).await.unwrap();
    store
        .start_write(&lease, std::slice::from_ref(&pending), true)
        .await
        .unwrap();
    sqlx::raw_sql("CREATE FUNCTION reject_published_check() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='REVIEW_CHECK_PUBLISHED' THEN RAISE EXCEPTION 'synthetic failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_published_check BEFORE INSERT ON audit_events FOR EACH ROW EXECUTE FUNCTION reject_published_check();").execute(&pool).await.unwrap();
    let success = run(&lease, 81, CheckState::Success);
    assert!(matches!(
        store
            .complete(&lease, std::slice::from_ref(&success), true)
            .await,
        Err(E::Unavailable)
    ));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM review_check_results")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM jobs WHERE id=$1")
            .bind(lease.job_id())
            .fetch_one(&pool)
            .await
            .unwrap(),
        "RUNNING"
    );
    sqlx::query("DROP TRIGGER reject_published_check ON audit_events")
        .execute(&pool)
        .await
        .unwrap();
    store
        .complete(&lease, std::slice::from_ref(&success), true)
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM review_check_writes")
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );
}
