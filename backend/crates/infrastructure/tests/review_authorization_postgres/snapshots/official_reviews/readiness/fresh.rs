use super::*;
use specspace_infrastructure::review_readiness::FreshReadinessError as E;

#[sqlx::test(migrations = "../../migrations")]
async fn fresh_readiness_verifies_provider_refs_without_state_or_approval_writes(pool: PgPool) {
    let (f, source, h, fp, factory) = ready(&pool, true).await;
    let service = PgReviewReadiness::new(pool.clone());
    let pending = service
        .fresh(f.owner, f.proposal, h, &factory)
        .await
        .unwrap();
    assert!(pending.provider_refs_verified);
    assert_eq!(pending.evaluation.conclusion, C::Pending);
    assert!(!issue(&pending, G::Evidence));
    pair(&pool, &f, h, &fp, &factory).await;
    let audit: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_events")
        .fetch_one(&pool)
        .await
        .unwrap();
    let reads = source.reads.load(Ordering::SeqCst);
    let report = service
        .fresh(f.owner, f.proposal, h, &factory)
        .await
        .unwrap();
    assert!(report.provider_refs_verified && report.current_for_metadata);
    assert_eq!(report.evaluation.conclusion, C::Success);
    assert!(report.evaluation.issues.is_empty());
    assert!(source.reads.load(Ordering::SeqCst) >= reads + 2);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM audit_events")
            .fetch_one(&pool)
            .await
            .unwrap(),
        audit
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM proposals WHERE id=$1")
            .bind(f.proposal)
            .fetch_one(&pool)
            .await
            .unwrap(),
        "IN_REVIEW"
    );
    // The ordinary endpoint still provides only stored evidence after success.
    let stored = service.stored(f.owner, f.proposal, h).await.unwrap();
    assert!(!stored.provider_refs_verified);
    assert_eq!(stored.evaluation.conclusion, C::Pending);
}

#[sqlx::test(migrations = "../../migrations")]
async fn fresh_readiness_rejects_external_ref_or_merge_base_changes_and_provider_errors(
    pool: PgPool,
) {
    let (f, source, h, fp, factory) = ready(&pool, true).await;
    pair(&pool, &f, h, &fp, &factory).await;
    let service = PgReviewReadiness::new(pool.clone());
    source.branch_head.store(3, Ordering::SeqCst);
    assert!(matches!(
        service.fresh(f.owner, f.proposal, h, &factory).await,
        Err(E::HeadChanged)
    ));
    source.branch_head.store(1, Ordering::SeqCst);
    source.target.store(3, Ordering::SeqCst);
    assert!(matches!(
        service.fresh(f.owner, f.proposal, h, &factory).await,
        Err(E::TargetChanged)
    ));
    source.target.store(2, Ordering::SeqCst);
    source.base_updated.store(false, Ordering::SeqCst);
    assert!(matches!(
        service.fresh(f.owner, f.proposal, h, &factory).await,
        Err(E::MergeBaseChanged)
    ));
    source.base_updated.store(true, Ordering::SeqCst);
    source.move_head_after_base.store(true, Ordering::SeqCst);
    assert!(matches!(
        service.fresh(f.owner, f.proposal, h, &factory).await,
        Err(E::HeadChanged)
    ));
    source.branch_head.store(1, Ordering::SeqCst);
    for error in [
        GitError::BranchMissing,
        GitError::Unauthorized,
        GitError::RateLimited {
            retry_after_seconds: Some(71),
        },
        GitError::ProviderUnavailable,
    ] {
        *source.failure.lock().unwrap() = Some(error.clone());
        assert!(
            matches!(service.fresh(f.owner, f.proposal, h, &factory).await, Err(E::Provider(e)) if e == error)
        );
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn fresh_readiness_reloads_reviewer_authority_after_unlocked_provider_io(pool: PgPool) {
    let (f, source, h, fp, factory) = ready(&pool, true).await;
    pair(&pool, &f, h, &fp, &factory).await;
    // Drain the notification retained by fixture preparation.
    let _ = tokio::time::timeout(
        std::time::Duration::from_millis(1),
        source.started.notified(),
    )
    .await;
    source.blocked.store(true, Ordering::SeqCst);
    let actor = f.owner;
    let proposal = f.proposal;
    let db = pool.clone();
    let task = tokio::spawn(async move {
        PgReviewReadiness::new(db)
            .fresh(actor, proposal, h, &factory)
            .await
    });
    source.started.notified().await;
    // This must complete during provider IO; no DB transaction/locks span it.
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        sqlx::query("DELETE FROM memberships WHERE organization_id=$1 AND user_id=$2")
            .bind(f.org)
            .bind(f.development)
            .execute(&pool),
    )
    .await
    .unwrap()
    .unwrap();
    source.blocked.store(false, Ordering::SeqCst);
    source.release.notify_waiters();
    let result = task.await.unwrap().unwrap();
    assert!(result.provider_refs_verified);
    assert_eq!(result.evaluation.conclusion, C::Pending);
    assert!(issue(&result, G::Approvals) && issue(&result, G::RequiredReviewer));
}

#[sqlx::test(migrations = "../../migrations")]
async fn fresh_readiness_suppresses_provider_diagnostics_after_actor_revocation(pool: PgPool) {
    let (f, source, h, _, factory) = ready(&pool, true).await;
    // Drain the notification retained by fixture preparation.
    let _ = tokio::time::timeout(
        std::time::Duration::from_millis(1),
        source.started.notified(),
    )
    .await;
    source.blocked.store(true, Ordering::SeqCst);
    *source.failure.lock().unwrap() = Some(GitError::ProviderUnavailable);
    let actor = f.owner;
    let proposal = f.proposal;
    let db = pool.clone();
    let task = tokio::spawn(async move {
        PgReviewReadiness::new(db)
            .fresh(actor, proposal, h, &factory)
            .await
    });
    source.started.notified().await;
    sqlx::query("UPDATE users SET status='DISABLED' WHERE id=$1")
        .bind(actor)
        .execute(&pool)
        .await
        .unwrap();
    source.blocked.store(false, Ordering::SeqCst);
    source.release.notify_waiters();
    assert!(matches!(
        task.await.unwrap(),
        Err(E::Stored(ReadinessError::NotFound))
    ));
}

struct RebindingFactory {
    pool: PgPool,
    inner: Factory,
}
#[async_trait]
impl BoundProviderFactory for RebindingFactory {
    async fn provider_bound(
        &self,
        binding: &RepositoryBinding,
    ) -> Result<Arc<dyn GitProvider>, ComparisonReadError> {
        sqlx::query("UPDATE repositories SET installation_id='8' WHERE id=$1")
            .bind(binding.repository)
            .execute(&self.pool)
            .await
            .unwrap();
        self.inner.provider_bound(binding).await
    }
}
#[sqlx::test(migrations = "../../migrations")]
async fn fresh_readiness_rejects_connection_rebinding_before_any_provider_ref_read(pool: PgPool) {
    let (f, source, h, _, factory) = ready(&pool, true).await;
    let reads = source.reads.load(Ordering::SeqCst);
    let source_factory = RebindingFactory {
        pool: pool.clone(),
        inner: factory,
    };
    assert!(matches!(
        PgReviewReadiness::new(pool)
            .fresh(f.owner, f.proposal, h, &source_factory)
            .await,
        Err(E::ContextChanged)
    ));
    assert_eq!(source.reads.load(Ordering::SeqCst), reads);
}
