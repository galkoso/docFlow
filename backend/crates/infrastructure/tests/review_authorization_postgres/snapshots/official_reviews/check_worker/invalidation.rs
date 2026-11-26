use super::*;
use specspace_application::review_check_publication::{CheckInvalidation as R, CheckPurpose};
use specspace_infrastructure::review_checks::CheckStoreError as E;

async fn published(pool: &PgPool) -> (Arc<PgReviewChecks>, Arc<Provider>, ReviewCheckWorker, Uuid) {
    let (store, provider, worker, job, proposal) = setup_worker(pool, true, 0).await;
    assert_eq!(worker.run_once().await.unwrap(), CheckWork::Completed(job));
    (store, provider, worker, proposal)
}
async fn role(pool: &PgPool, actor: Uuid, value: &str) {
    sqlx::query("UPDATE memberships SET role=$2 WHERE user_id=$1")
        .bind(actor)
        .bind(value)
        .execute(pool)
        .await
        .unwrap();
}
async fn reservations(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM review_check_intents WHERE purpose='INVALIDATION'")
        .fetch_one(pool)
        .await
        .unwrap()
}
#[sqlx::test(migrations = "../../migrations")]
async fn invalidation_resolves_the_original_run_and_fails_it_after_closure_without_git_ref_reads(
    pool: PgPool,
) {
    let (store, provider, worker, proposal) = published(&pool).await;
    role(&pool, provider.actor, "MAINTAINER").await;
    sqlx::query("UPDATE proposals SET state='CLOSED',head_sha=$2,target_sha=$2,version=version+1 WHERE id=$1").bind(proposal).bind(sha(3).as_str()).execute(&pool).await.unwrap();
    provider.git.branch_head.store(3, Ordering::SeqCst);
    provider.git.target.store(3, Ordering::SeqCst);
    provider
        .git
        .failure
        .lock()
        .unwrap()
        .replace(GitError::ProviderUnavailable);
    let reads = provider.git.reads.load(Ordering::SeqCst);
    let op = Uuid::new_v4();
    let ticket = store
        .invalidate(
            provider.actor,
            proposal,
            provider.operation,
            R::HeadChanged,
            op,
        )
        .await
        .unwrap();
    assert!(!ticket.replayed);
    assert_eq!(ticket.intent().result(), CheckResult::Failure);
    assert_eq!(ticket.intent().check_operation(), provider.operation);
    assert!(
        matches!(ticket.intent().purpose(),CheckPurpose::Invalidation{reason:R::HeadChanged,run,..} if run.get()==81)
    );
    assert_eq!(ticket.intent().source().head.as_str(), sha(1).as_str());
    assert_eq!(
        worker.run_once().await.unwrap(),
        CheckWork::Completed(ticket.job_id)
    );
    assert_eq!(provider.creates.load(Ordering::SeqCst), 1);
    assert_eq!(provider.updates.load(Ordering::SeqCst), 2);
    assert_eq!(provider.rows.lock().unwrap()[0].state, CheckState::Failure);
    assert_eq!(provider.git.reads.load(Ordering::SeqCst), reads);
    assert_eq!(count(&pool, "review_check_runs").await, 1);
    assert_eq!(count(&pool, "review_check_results").await, 2);
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM proposals WHERE id=$1")
            .bind(proposal)
            .fetch_one(&pool)
            .await
            .unwrap(),
        "CLOSED"
    );
    let replay = store
        .invalidate(
            provider.actor,
            proposal,
            provider.operation,
            R::HeadChanged,
            op,
        )
        .await
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.job_id, ticket.job_id);
    assert_eq!(
        replay.intent().request_hash(),
        ticket.intent().request_hash()
    );
    assert_eq!(reservations(&pool).await, 1);
}
#[sqlx::test(migrations = "../../migrations")]
async fn invalidation_exact_retry_is_scoped_and_requires_current_privilege_before_observation(
    pool: PgPool,
) {
    let (store, provider, _, proposal) = published(&pool).await;
    let admin = user(
        &pool,
        sqlx::query_scalar("SELECT organization_id FROM proposals WHERE id=$1")
            .bind(proposal)
            .fetch_one(&pool)
            .await
            .unwrap(),
        "ADMIN",
        vec![],
    )
    .await;
    role(&pool, provider.actor, "MAINTAINER").await;
    let op = Uuid::new_v4();
    let first = store
        .invalidate(
            provider.actor,
            proposal,
            provider.operation,
            R::PolicyChanged,
            op,
        )
        .await
        .unwrap();
    for reason in [R::HeadChanged, R::Superseded] {
        assert!(matches!(
            store
                .invalidate(provider.actor, proposal, provider.operation, reason, op)
                .await,
            Err(E::IdempotencyMismatch)
        ));
    }
    assert!(matches!(
        store
            .invalidate(admin, proposal, provider.operation, R::PolicyChanged, op)
            .await,
        Err(E::IdempotencyMismatch)
    ));
    assert!(matches!(
        store
            .invalidate(
                provider.actor,
                proposal,
                Uuid::new_v4(),
                R::PolicyChanged,
                op
            )
            .await,
        Err(E::IdempotencyMismatch)
    ));
    assert!(matches!(
        store
            .evaluate(
                provider.actor,
                proposal,
                first.intent().source().snapshot,
                op
            )
            .await,
        Err(E::IdempotencyMismatch)
    ));
    role(&pool, provider.actor, "CONTRIBUTOR").await;
    assert!(matches!(
        store
            .invalidate(
                provider.actor,
                proposal,
                provider.operation,
                R::PolicyChanged,
                op
            )
            .await,
        Err(E::Forbidden)
    ));
    role(&pool, provider.actor, "ADMIN").await;
    sqlx::query("UPDATE repositories SET check_app_id=NULL WHERE id=$1")
        .bind(first.intent().source().repository.repository)
        .execute(&pool)
        .await
        .unwrap();
    let replay = store
        .invalidate(
            provider.actor,
            proposal,
            provider.operation,
            R::PolicyChanged,
            op,
        )
        .await
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(
        replay.intent().request_hash(),
        first.intent().request_hash()
    );
    assert_eq!(reservations(&pool).await, 1);
}
#[sqlx::test(migrations = "../../migrations")]
async fn invalidation_rejects_unprivileged_foreign_missing_unadopted_and_wrong_purpose_targets(
    pool: PgPool,
) {
    let (store, provider, worker, job, proposal) = setup_worker(&pool, true, 0).await;
    for r in ["READER", "CONTRIBUTOR", "REVIEWER"] {
        role(&pool, provider.actor, r).await;
        assert!(matches!(
            store
                .invalidate(
                    provider.actor,
                    proposal,
                    provider.operation,
                    R::Superseded,
                    Uuid::new_v4()
                )
                .await,
            Err(E::Forbidden)
        ));
    }
    role(&pool, provider.actor, "MAINTAINER").await;
    assert!(matches!(
        store
            .invalidate(
                provider.actor,
                proposal,
                provider.operation,
                R::Superseded,
                Uuid::new_v4()
            )
            .await,
        Err(E::InvalidReceipt)
    ));
    assert!(matches!(
        store
            .invalidate(
                provider.actor,
                proposal,
                Uuid::new_v4(),
                R::Superseded,
                Uuid::new_v4()
            )
            .await,
        Err(E::NotFound)
    ));
    for (actor, p, target, op) in [
        (Uuid::nil(), proposal, provider.operation, Uuid::new_v4()),
        (
            provider.actor,
            proposal,
            provider.operation,
            provider.operation,
        ),
    ] {
        assert!(matches!(
            store.invalidate(actor, p, target, R::Superseded, op).await,
            Err(E::Invalid)
        ));
    }
    // Role changes invalidate evaluation proof; restore it for publication.
    role(&pool, provider.actor, "CONTRIBUTOR").await;
    assert_eq!(worker.run_once().await.unwrap(), CheckWork::Completed(job));
    role(&pool, provider.actor, "MAINTAINER").await;
    let second = store
        .invalidate(
            provider.actor,
            proposal,
            provider.operation,
            R::Superseded,
            Uuid::new_v4(),
        )
        .await
        .unwrap();
    assert!(matches!(
        store
            .invalidate(
                provider.actor,
                proposal,
                second.operation_id(),
                R::Superseded,
                Uuid::new_v4()
            )
            .await,
        Err(E::NotFound)
    ));
    let (_, foreign, _, _, foreign_proposal) = setup_worker(&pool, true, 0).await;
    assert!(matches!(
        store
            .invalidate(
                provider.actor,
                foreign_proposal,
                foreign.operation,
                R::Superseded,
                Uuid::new_v4()
            )
            .await,
        Err(E::NotFound)
    ));
    assert!(matches!(
        store
            .invalidate(
                provider.actor,
                proposal,
                foreign.operation,
                R::Superseded,
                Uuid::new_v4()
            )
            .await,
        Err(E::NotFound)
    ));
    assert_eq!(reservations(&pool).await, 1);
}
#[sqlx::test(migrations = "../../migrations")]
async fn invalidation_serializes_identical_reservations_and_cannot_bypass_active_work(
    pool: PgPool,
) {
    let (store, provider, _, proposal) = published(&pool).await;
    role(&pool, provider.actor, "MAINTAINER").await;
    let op = Uuid::new_v4();
    let (a, b) = tokio::join!(
        store.invalidate(
            provider.actor,
            proposal,
            provider.operation,
            R::BlockersChanged,
            op
        ),
        store.invalidate(
            provider.actor,
            proposal,
            provider.operation,
            R::BlockersChanged,
            op
        )
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(a.job_id, b.job_id);
    assert_ne!(a.replayed, b.replayed);
    assert!(matches!(
        store
            .invalidate(
                provider.actor,
                proposal,
                provider.operation,
                R::BlockersChanged,
                Uuid::new_v4()
            )
            .await,
        Err(E::Busy)
    ));
    assert_eq!(reservations(&pool).await, 1);
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM outbox WHERE event_type='REVIEW_CHECK_QUEUED' AND payload->>'purpose'='INVALIDATION'").fetch_one(&pool).await.unwrap(),1);
}
#[sqlx::test(migrations = "../../migrations")]
async fn invalidation_admission_rolls_back_partial_work_and_same_operation_can_retry(pool: PgPool) {
    let (store, provider, _, proposal) = published(&pool).await;
    role(&pool, provider.actor, "MAINTAINER").await;
    sqlx::raw_sql("CREATE FUNCTION reject_invalidation() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='REVIEW_CHECK_RESERVED' THEN RAISE EXCEPTION 'synthetic failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_invalidation BEFORE INSERT ON audit_events FOR EACH ROW EXECUTE FUNCTION reject_invalidation();").execute(&pool).await.unwrap();
    let op = Uuid::new_v4();
    assert!(matches!(
        store
            .invalidate(
                provider.actor,
                proposal,
                provider.operation,
                R::Superseded,
                op
            )
            .await,
        Err(E::Unavailable)
    ));
    assert_eq!(reservations(&pool).await, 0);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM git_operations WHERE id=$1")
            .bind(op)
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    sqlx::query("DROP TRIGGER reject_invalidation ON audit_events")
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        !store
            .invalidate(
                provider.actor,
                proposal,
                provider.operation,
                R::Superseded,
                op
            )
            .await
            .unwrap()
            .replayed
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn invalidation_lost_update_response_reconciles_failure_without_repeating_the_patch(
    pool: PgPool,
) {
    let (store, provider, worker, proposal) = published(&pool).await;
    role(&pool, provider.actor, "MAINTAINER").await;
    let ticket = store
        .invalidate(
            provider.actor,
            proposal,
            provider.operation,
            R::AuthorityChanged,
            Uuid::new_v4(),
        )
        .await
        .unwrap();
    provider.mode.store(2, Ordering::SeqCst);
    assert_eq!(
        worker.run_once().await.unwrap(),
        CheckWork::Deferred(ticket.job_id)
    );
    retry(&pool, ticket.job_id).await;
    assert_eq!(
        worker.run_once().await.unwrap(),
        CheckWork::Completed(ticket.job_id)
    );
    assert_eq!(provider.creates.load(Ordering::SeqCst), 1);
    assert_eq!(provider.updates.load(Ordering::SeqCst), 2);
}
#[sqlx::test(migrations = "../../migrations")]
async fn invalidation_connection_changes_and_revoked_authority_stop_new_provider_writes(
    pool: PgPool,
) {
    let (store, provider, worker, proposal) = published(&pool).await;
    role(&pool, provider.actor, "MAINTAINER").await;
    let repo = sqlx::query_scalar::<_, Uuid>("SELECT repository_id FROM proposals WHERE id=$1")
        .bind(proposal)
        .fetch_one(&pool)
        .await
        .unwrap();
    for setting in [None, Some(8)] {
        sqlx::query("UPDATE repositories SET check_app_id=$2 WHERE id=$1")
            .bind(repo)
            .bind(setting)
            .execute(&pool)
            .await
            .unwrap();
        let result = store
            .invalidate(
                provider.actor,
                proposal,
                provider.operation,
                R::Superseded,
                Uuid::new_v4(),
            )
            .await;
        assert!(matches!(
            (setting, result),
            (None, Err(E::NotConfigured)) | (Some(_), Err(E::Stale))
        ));
    }
    sqlx::query("UPDATE repositories SET check_app_id=7 WHERE id=$1")
        .bind(repo)
        .execute(&pool)
        .await
        .unwrap();
    let ticket = store
        .invalidate(
            provider.actor,
            proposal,
            provider.operation,
            R::Superseded,
            Uuid::new_v4(),
        )
        .await
        .unwrap();
    role(&pool, provider.actor, "READER").await;
    assert_eq!(
        worker.run_once().await.unwrap(),
        CheckWork::Stopped(ticket.job_id)
    );
    assert_eq!(provider.creates.load(Ordering::SeqCst), 1);
    assert_eq!(provider.updates.load(Ordering::SeqCst), 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn invalidation_shares_actor_quota_with_evaluation_and_exact_replay_is_free(pool: PgPool) {
    let (store, provider, _, proposal) = published(&pool).await;
    role(&pool, provider.actor, "MAINTAINER").await;
    let mut first = None;
    // The original evaluation has already consumed one of the sixty admissions.
    for _ in 0..59 {
        let op = Uuid::new_v4();
        let ticket = store
            .invalidate(
                provider.actor,
                proposal,
                provider.operation,
                R::Superseded,
                op,
            )
            .await
            .unwrap();
        if first.is_none() {
            first = Some((op, ticket.job_id, ticket.intent().request_hash().to_owned()));
        }
        let lease = store.claim(Uuid::new_v4(), 120).await.unwrap().unwrap();
        assert_eq!(lease.job_id(), ticket.job_id);
        assert!(
            !store
                .defer(&lease, "OPERATOR_STOPPED", None, None)
                .await
                .unwrap()
        );
    }
    assert!(matches!(
        store
            .invalidate(
                provider.actor,
                proposal,
                provider.operation,
                R::Superseded,
                Uuid::new_v4()
            )
            .await,
        Err(E::Quota)
    ));
    let (op, job, hash) = first.unwrap();
    let replay = store
        .invalidate(
            provider.actor,
            proposal,
            provider.operation,
            R::Superseded,
            op,
        )
        .await
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.job_id, job);
    assert_eq!(replay.intent().request_hash(), hash);
    assert_eq!(reservations(&pool).await, 59);
    assert_eq!(provider.updates.load(Ordering::SeqCst), 1);
}
#[sqlx::test(migrations = "../../migrations")]
async fn invalidation_respects_shared_cooldown_without_blocking_exact_observation(pool: PgPool) {
    let (store, provider, _, proposal) = published(&pool).await;
    role(&pool, provider.actor, "ADMIN").await;
    let op = Uuid::new_v4();
    let ticket = store
        .invalidate(
            provider.actor,
            proposal,
            provider.operation,
            R::Superseded,
            op,
        )
        .await
        .unwrap();
    let lease = store.claim(Uuid::new_v4(), 120).await.unwrap().unwrap();
    assert!(
        !store
            .defer(&lease, "PROVIDER_RATE_LIMITED", None, Some(17))
            .await
            .unwrap()
    );
    assert!(
        store
            .invalidate(
                provider.actor,
                proposal,
                provider.operation,
                R::Superseded,
                op
            )
            .await
            .unwrap()
            .replayed
    );
    assert!(
        matches!(store.invalidate(provider.actor,proposal,provider.operation,R::Superseded,Uuid::new_v4()).await,Err(E::Cooldown(n)) if (1..=17).contains(&n))
    );
    assert_eq!(reservations(&pool).await, 1);
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM jobs WHERE id=$1")
            .bind(ticket.job_id)
            .fetch_one(&pool)
            .await
            .unwrap(),
        "FAILED"
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn authority_is_rechecked_after_waiting_for_the_proposal_lock(pool: PgPool) {
    let (store, provider, _, proposal) = published(&pool).await;
    role(&pool, provider.actor, "MAINTAINER").await;
    let mut held = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM proposals WHERE id=$1 FOR UPDATE")
        .bind(proposal)
        .execute(&mut *held)
        .await
        .unwrap();
    let actor = provider.actor;
    let target = provider.operation;
    let waiter = tokio::spawn(async move {
        store
            .invalidate(actor, proposal, target, R::Superseded, Uuid::new_v4())
            .await
    });
    tokio::time::timeout(Duration::from_secs(3),async {
        loop {
            if sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND application_name='specspace-check-invalidation' AND wait_event_type='Lock')").fetch_one(&pool).await.unwrap() {break;}
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    role(&pool, provider.actor, "READER").await;
    held.commit().await.unwrap();
    assert!(matches!(waiter.await.unwrap(), Err(E::Forbidden)));
    assert_eq!(reservations(&pool).await, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_current_admin_can_invalidate_after_the_original_publisher_is_disabled(pool: PgPool) {
    let (store, provider, worker, proposal) = published(&pool).await;
    let org = sqlx::query_scalar("SELECT organization_id FROM proposals WHERE id=$1")
        .bind(proposal)
        .fetch_one(&pool)
        .await
        .unwrap();
    let admin = user(&pool, org, "ADMIN", vec![]).await;
    sqlx::query("UPDATE users SET status='DISABLED' WHERE id=$1")
        .bind(provider.actor)
        .execute(&pool)
        .await
        .unwrap();
    let ticket = store
        .invalidate(
            admin,
            proposal,
            provider.operation,
            R::AuthorityChanged,
            Uuid::new_v4(),
        )
        .await
        .unwrap();
    assert_eq!(ticket.intent().source().actor, admin);
    assert_eq!(
        worker.run_once().await.unwrap(),
        CheckWork::Completed(ticket.job_id)
    );
    assert_eq!(provider.updates.load(Ordering::SeqCst), 2);
    assert_eq!(provider.rows.lock().unwrap()[0].state, CheckState::Failure);
    assert_eq!(sqlx::query_scalar::<_,Uuid>("SELECT actor_id FROM audit_events WHERE operation_id=$1 AND action='REVIEW_CHECK_PUBLISHED'").bind(ticket.operation_id()).fetch_one(&pool).await.unwrap(),admin);
}
#[sqlx::test(migrations = "../../migrations")]
async fn invalidation_cannot_discard_an_adopted_unknown_evaluation_to_bypass_the_lane(
    pool: PgPool,
) {
    let (store, provider, _, _, proposal) = setup_worker(&pool, true, 0).await;
    let lease = store.claim(Uuid::new_v4(), 120).await.unwrap().unwrap();
    store.start_write(&lease, &[], true).await.unwrap();
    let receipt = CheckRun {
        id: CheckRunId::new(81).unwrap(),
        scope: lease.intent().scope(),
        operation: provider.operation,
        state: CheckState::Pending,
    };
    store.adopt(&lease, &receipt).await.unwrap();
    assert!(
        !store
            .defer(&lease, "RECONCILIATION_REQUIRED", None, None)
            .await
            .unwrap()
    );
    role(&pool, provider.actor, "ADMIN").await;
    assert!(matches!(
        store
            .invalidate(
                provider.actor,
                proposal,
                provider.operation,
                R::Superseded,
                Uuid::new_v4()
            )
            .await,
        Err(E::Busy)
    ));
    assert_eq!(reservations(&pool).await, 0);
    assert_eq!(count(&pool, "review_check_writes").await, 1);
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM git_operations WHERE id=$1")
            .bind(provider.operation)
            .fetch_one(&pool)
            .await
            .unwrap(),
        "UNKNOWN"
    );
}
