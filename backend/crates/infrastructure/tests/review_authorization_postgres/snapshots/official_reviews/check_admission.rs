use super::*;
use specspace_git_adapter::checks::CheckResult;
use specspace_infrastructure::{
    review_checks::{CheckStoreError as E, PgReviewChecks},
    review_readiness::PgReviewReadiness,
};

pub(super) async fn prepared(
    pool: &PgPool,
    approved: bool,
) -> (Fixture, Arc<Source>, Uuid, Arc<PgReviewChecks>) {
    let (f, source, h, fp, factory) = ready(pool, true).await;
    if approved {
        super::readiness::pair(pool, &f, h, &fp, &factory).await;
    }
    sqlx::query("UPDATE repositories SET check_app_id=7 WHERE id=$1")
        .bind(f.repository)
        .execute(pool)
        .await
        .unwrap();
    let store = Arc::new(PgReviewChecks::new(pool.clone(), Arc::new(factory)));
    (f, source, h, store)
}
async fn no_rows(pool: &PgPool) {
    for query in [
        "SELECT count(*) FROM review_check_intents",
        "SELECT count(*) FROM jobs WHERE kind='PUBLISH_REVIEW_CHECK'",
        "SELECT count(*) FROM git_operations WHERE kind='REVIEW_CHECK'",
        "SELECT count(*) FROM audit_events WHERE action='REVIEW_CHECK_RESERVED'",
        "SELECT count(*) FROM outbox WHERE event_type='REVIEW_CHECK_QUEUED'",
    ] {
        assert_eq!(
            sqlx::query_scalar::<_, i64>(query)
                .fetch_one(pool)
                .await
                .unwrap(),
            0,
            "{query}"
        );
    }
}
#[sqlx::test(migrations = "../../migrations")]
async fn admission_freezes_real_server_pending_or_success_evidence_without_publishing_or_approving(
    pool: PgPool,
) {
    for approved in [false, true] {
        let (f, source, h, store) = prepared(&pool, approved).await;
        let op = Uuid::new_v4();
        let reads = source.reads.load(Ordering::SeqCst);
        let queued = store.evaluate(f.owner, f.proposal, h, op).await.unwrap();
        assert!(!queued.replayed);
        assert_eq!(queued.operation_id(), op);
        assert_eq!(
            queued.intent().result(),
            if approved {
                CheckResult::Success
            } else {
                CheckResult::Pending
            }
        );
        assert_eq!(queued.intent().source().snapshot, h);
        assert_eq!(queued.intent().source().app_id, 7);
        assert_eq!(queued.intent().source().authority_digest.len(), 64);
        assert!(source.reads.load(Ordering::SeqCst) > reads);
        let (hash,state):(String,String)=sqlx::query_as("SELECT i.request_hash,j.state FROM review_check_intents i JOIN jobs j ON j.id=i.job_id WHERE i.operation_id=$1").bind(op).fetch_one(&pool).await.unwrap();
        assert_eq!(hash, queued.intent().request_hash());
        assert_eq!(state, "QUEUED");
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM review_check_writes WHERE operation_id=$1"
            )
            .bind(op)
            .fetch_one(&pool)
            .await
            .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT state FROM proposals WHERE id=$1")
                .bind(f.proposal)
                .fetch_one(&pool)
                .await
                .unwrap(),
            "IN_REVIEW"
        );
        let reads = source.reads.load(Ordering::SeqCst);
        sqlx::query("UPDATE proposals SET state='CLOSED' WHERE id=$1")
            .bind(f.proposal)
            .execute(&pool)
            .await
            .unwrap();
        let replay = store.evaluate(f.owner, f.proposal, h, op).await.unwrap();
        assert!(replay.replayed);
        assert_eq!(replay.job_id, queued.job_id);
        assert_eq!(replay.intent().request_hash(), hash);
        assert_eq!(source.reads.load(Ordering::SeqCst), reads);
    }
}
#[sqlx::test(migrations = "../../migrations")]
async fn identical_concurrent_admission_has_one_job_and_new_intents_cannot_bypass_the_active_slot(
    pool: PgPool,
) {
    let (f, source, h, store) = prepared(&pool, true).await;
    let op = Uuid::new_v4();
    let (a, b) = tokio::join!(
        store.evaluate(f.owner, f.proposal, h, op),
        store.evaluate(f.owner, f.proposal, h, op)
    );
    let a = a.unwrap();
    let b = b.unwrap();
    assert_ne!(a.replayed, b.replayed);
    assert_eq!(a.job_id, b.job_id);
    assert_eq!(a.intent().request_hash(), b.intent().request_hash());
    let reads = source.reads.load(Ordering::SeqCst);
    assert!(matches!(
        store.evaluate(f.owner, f.proposal, h, Uuid::new_v4()).await,
        Err(E::Busy)
    ));
    assert_eq!(source.reads.load(Ordering::SeqCst), reads);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM review_check_intents")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    assert!(matches!(
        store.evaluate(f.product, f.proposal, h, op).await,
        Err(E::IdempotencyMismatch)
    ));
}
#[sqlx::test(migrations = "../../migrations")]
async fn admission_checks_identity_scope_role_app_and_provider_refs_before_reserving(pool: PgPool) {
    let (f, source, h, store) = prepared(&pool, false).await;
    let op = Uuid::new_v4();
    let reads = source.reads.load(Ordering::SeqCst);
    assert!(matches!(
        store.evaluate(Uuid::nil(), f.proposal, h, op).await,
        Err(E::Invalid)
    ));
    assert!(matches!(
        store.evaluate(f.owner, Uuid::new_v4(), h, op).await,
        Err(E::NotFound)
    ));
    assert!(matches!(
        store
            .evaluate(f.owner, f.proposal, Uuid::new_v4(), op)
            .await,
        Err(E::NotFound)
    ));
    sqlx::query("UPDATE memberships SET role='READER' WHERE organization_id=$1 AND user_id=$2")
        .bind(f.org)
        .bind(f.owner)
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        store.evaluate(f.owner, f.proposal, h, op).await,
        Err(E::Forbidden)
    ));
    sqlx::query(
        "UPDATE memberships SET role='CONTRIBUTOR' WHERE organization_id=$1 AND user_id=$2",
    )
    .bind(f.org)
    .bind(f.owner)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE repositories SET check_app_id=NULL WHERE id=$1")
        .bind(f.repository)
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        store.evaluate(f.owner, f.proposal, h, op).await,
        Err(E::NotConfigured)
    ));
    assert_eq!(source.reads.load(Ordering::SeqCst), reads);
    sqlx::query("UPDATE repositories SET check_app_id=7 WHERE id=$1")
        .bind(f.repository)
        .execute(&pool)
        .await
        .unwrap();
    source.target.store(3, Ordering::SeqCst);
    assert!(
        matches!(store.evaluate(f.owner,f.proposal,h,op).await,Err(E::Fresh(crate_error)) if crate_error==specspace_infrastructure::review_readiness::FreshReadinessError::TargetChanged)
    );
    no_rows(&pool).await;
    source.target.store(2, Ordering::SeqCst);
    let queued = store.evaluate(f.owner, f.proposal, h, op).await.unwrap();
    sqlx::query("UPDATE memberships SET role='READER' WHERE organization_id=$1 AND user_id=$2")
        .bind(f.org)
        .bind(f.owner)
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        store.evaluate(f.owner, f.proposal, h, op).await,
        Err(E::Forbidden)
    ));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM review_check_intents WHERE job_id=$1")
            .bind(queued.job_id)
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn audit_failure_rolls_back_admission_and_exact_retry_can_reserve_after_recovery(
    pool: PgPool,
) {
    let (f, _, h, store) = prepared(&pool, true).await;
    let op = Uuid::new_v4();
    sqlx::raw_sql("CREATE FUNCTION reject_check_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='REVIEW_CHECK_RESERVED' THEN RAISE EXCEPTION 'synthetic failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_check_audit BEFORE INSERT ON audit_events FOR EACH ROW EXECUTE FUNCTION reject_check_audit();").execute(&pool).await.unwrap();
    assert!(matches!(
        store.evaluate(f.owner, f.proposal, h, op).await,
        Err(E::Unavailable)
    ));
    no_rows(&pool).await;
    sqlx::query("DROP TRIGGER reject_check_audit ON audit_events")
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        !store
            .evaluate(f.owner, f.proposal, h, op)
            .await
            .unwrap()
            .replayed
    );
}
struct RoleChangingFactory {
    pool: PgPool,
    actor: Uuid,
    inner: Factory,
}
#[async_trait]
impl BoundProviderFactory for RoleChangingFactory {
    async fn provider_bound(
        &self,
        binding: &RepositoryBinding,
    ) -> Result<Arc<dyn GitProvider>, ComparisonReadError> {
        sqlx::query("UPDATE memberships SET role='READER' WHERE user_id=$1")
            .bind(self.actor)
            .execute(&self.pool)
            .await
            .unwrap();
        self.inner.provider_bound(binding).await
    }
}
#[sqlx::test(migrations = "../../migrations")]
async fn losing_write_authority_during_connection_prevents_any_queued_intent(pool: PgPool) {
    let (f, source, h, _) = prepared(&pool, false).await;
    let factory = RoleChangingFactory {
        pool: pool.clone(),
        actor: f.owner,
        inner: Factory {
            source,
            repository: f.repository,
        },
    };
    let store = PgReviewChecks::new(pool.clone(), Arc::new(factory));
    assert!(matches!(
        store.evaluate(f.owner, f.proposal, h, Uuid::new_v4()).await,
        Err(E::Forbidden)
    ));
    no_rows(&pool).await;
}
#[sqlx::test(migrations = "../../migrations")]
async fn reservation_rechecks_qualification_changes_after_provider_io_even_when_policy_result_is_unchanged(
    pool: PgPool,
) {
    let (f, _, h, store) = prepared(&pool, false).await;
    let op = Uuid::new_v4();
    let before = PgReviewReadiness::new(pool.clone())
        .stored(f.owner, f.proposal, h)
        .await
        .unwrap();
    let mut barrier = pool.begin().await.unwrap();
    sqlx::query(
        "SELECT user_id FROM memberships WHERE organization_id=$1 AND user_id=$2 FOR UPDATE",
    )
    .bind(f.org)
    .bind(f.development)
    .execute(&mut *barrier)
    .await
    .unwrap();
    let actor = f.owner;
    let proposal = f.proposal;
    let pending = tokio::spawn({
        let store = store.clone();
        async move { store.evaluate(actor, proposal, h, op).await }
    });
    tokio::time::timeout(Duration::from_secs(5),async {
        loop {
            let blocked:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND application_name='specspace-check-admission' AND wait_event_type='Lock')").fetch_one(&pool).await.unwrap();
            if blocked {break;} tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    sqlx::query(
        "UPDATE memberships SET review_domains='{}' WHERE organization_id=$1 AND user_id=$2",
    )
    .bind(f.org)
    .bind(f.development)
    .execute(&mut *barrier)
    .await
    .unwrap();
    barrier.commit().await.unwrap();
    assert!(matches!(pending.await.unwrap(), Err(E::Stale)));
    no_rows(&pool).await;
    let after = PgReviewReadiness::new(pool.clone())
        .stored(f.owner, f.proposal, h)
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(before.evaluation).unwrap(),
        serde_json::to_value(after.evaluation).unwrap()
    );
    assert_eq!(
        store
            .evaluate(f.owner, f.proposal, h, op)
            .await
            .unwrap()
            .intent()
            .result(),
        CheckResult::Pending
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn equal_pending_results_do_not_hide_new_question_revision_evidence(pool: PgPool) {
    use specspace_application::product_question::CreateProductQuestion;
    use specspace_infrastructure::product_questions::PgProductQuestions;
    let (f, _, h, store) = prepared(&pool, false).await;
    let first = store
        .evaluate(f.owner, f.proposal, h, Uuid::new_v4())
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("UPDATE jobs SET state='FAILED' WHERE id=$1")
        .bind(first.job_id)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("UPDATE git_operations SET state='FAILED' WHERE id=$1")
        .bind(first.operation_id())
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    PgProductQuestions::new(pool.clone())
        .create(
            f.owner,
            f.proposal,
            CreateProductQuestion {
                request_id: Uuid::new_v4(),
                expected_proposal_version: first.intent().source().proposal_version,
                text: "Private nonblocking question".into(),
                blocking: false,
                proposed_answer: None,
            },
        )
        .await
        .unwrap();
    let next = store
        .evaluate(f.owner, f.proposal, h, Uuid::new_v4())
        .await
        .unwrap();
    assert_eq!(first.intent().result(), next.intent().result());
    assert_eq!(
        first.intent().source().proposal_version,
        next.intent().source().proposal_version
    );
    assert_ne!(
        first.intent().source().authority_digest,
        next.intent().source().authority_digest
    );
    let payload: serde_json::Value = sqlx::query_scalar("SELECT payload FROM jobs WHERE id=$1")
        .bind(next.job_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        payload,
        serde_json::json!({"operation_id":next.operation_id()})
    );
}
