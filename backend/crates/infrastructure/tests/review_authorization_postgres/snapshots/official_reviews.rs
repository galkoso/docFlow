use super::*;
use specspace_application::{
    official_review::{OfficialReviewRequest, ReviewDecision},
    review_configuration::ReviewArea,
};
use specspace_infrastructure::official_reviews::{OfficialReviewError, PgOfficialReviews};

async fn ready(pool: &PgPool, base_updated: bool) -> (Fixture, Arc<Source>, Uuid, String, Factory) {
    let (f, source, preparer) = setup(pool).await;
    if base_updated {
        source.base_updated.store(true, Ordering::SeqCst);
        sqlx::query("UPDATE proposals SET base_sha=$2 WHERE id=$1")
            .bind(f.proposal)
            .bind(sha(2).as_str())
            .execute(pool)
            .await
            .unwrap();
    }
    let (lease, verified) = prepared(pool, &f, &preparer, 2).await;
    let published = PgReviewSnapshots::new(pool.clone())
        .publish(&lease, &verified, &preparer)
        .await
        .unwrap();
    let factory = Factory {
        source: source.clone(),
        repository: f.repository,
    };
    (
        f,
        source,
        published.snapshot_id,
        published.fingerprint,
        factory,
    )
}
fn request(fingerprint: &str, decision: ReviewDecision) -> OfficialReviewRequest {
    OfficialReviewRequest {
        request_id: Uuid::new_v4(),
        expected_fingerprint: fingerprint.into(),
        domain: ReviewArea::Product,
        decision,
        explanation: "Reviewed the complete saved change".into(),
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn failed_provider_reads_are_charged_but_recorded_replay_needs_no_io(pool: PgPool) {
    let (f, source, snapshot, fp, factory) = ready(&pool, true).await;
    let store = PgOfficialReviews::new(pool.clone());
    let first = request(&fp, ReviewDecision::Comment);
    store
        .submit(f.product, f.proposal, snapshot, first.clone(), &factory)
        .await
        .unwrap();
    *source.failure.lock().unwrap() = Some(GitError::ProviderUnavailable);
    for _ in 1..30 {
        assert_eq!(
            store
                .submit(
                    f.product,
                    f.proposal,
                    snapshot,
                    request(&fp, ReviewDecision::Comment),
                    &factory
                )
                .await
                .err(),
            Some(OfficialReviewError::ProviderUnavailable)
        );
    }
    let reads = source.reads.load(Ordering::SeqCst);
    assert_eq!(
        store
            .submit(
                f.product,
                f.proposal,
                snapshot,
                request(&fp, ReviewDecision::Comment),
                &factory
            )
            .await
            .err(),
        Some(OfficialReviewError::Quota)
    );
    assert!(
        store
            .submit(f.product, f.proposal, snapshot, first, &factory)
            .await
            .unwrap()
            .replayed
    );
    assert_eq!(source.reads.load(Ordering::SeqCst), reads);
    let charged: i64 = sqlx::query_scalar("SELECT count(*) FROM official_review_reads")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(charged, 30);
    let failures:i64=sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE action='OFFICIAL_REVIEW_SOURCE_FAILED' AND result='PROVIDER_UNAVAILABLE'").fetch_one(&pool).await.unwrap();
    assert_eq!(failures, 29);
}

#[sqlx::test(migrations = "../../migrations")]
async fn provider_backoff_applies_to_other_reviewers_and_failed_request_bodies_stay_bound(
    pool: PgPool,
) {
    let (f, source, snapshot, fp, factory) = ready(&pool, true).await;
    let store = PgOfficialReviews::new(pool.clone());
    *source.failure.lock().unwrap() = Some(GitError::RateLimited {
        retry_after_seconds: Some(17),
    });
    let req = request(&fp, ReviewDecision::Comment);
    assert_eq!(
        store
            .submit(f.product, f.proposal, snapshot, req.clone(), &factory)
            .await
            .err(),
        Some(OfficialReviewError::ProviderRateLimited(17))
    );
    let reads = source.reads.load(Ordering::SeqCst);
    let mut other = request(&fp, ReviewDecision::Comment);
    other.domain = ReviewArea::Development;
    assert!(matches!(
        store
            .submit(f.development, f.proposal, snapshot, other, &factory)
            .await,
        Err(OfficialReviewError::ProviderRateLimited(_))
    ));
    assert_eq!(reads, source.reads.load(Ordering::SeqCst));
    let mut changed = req;
    changed.explanation = "Another intent".into();
    assert_eq!(
        store
            .submit(f.product, f.proposal, snapshot, changed, &factory)
            .await
            .err(),
        Some(OfficialReviewError::IdempotencyMismatch)
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn history_is_shared_paged_and_separates_effective_eligibility_from_live_approval(
    pool: PgPool,
) {
    let (f, source, snapshot, fp, factory) = ready(&pool, true).await;
    let store = PgOfficialReviews::new(pool.clone());
    store
        .submit(
            f.product,
            f.proposal,
            snapshot,
            request(&fp, ReviewDecision::ApproveAll),
            &factory,
        )
        .await
        .unwrap();
    for _ in 0..22 {
        store
            .submit(
                f.product,
                f.proposal,
                snapshot,
                request(&fp, ReviewDecision::Comment),
                &factory,
            )
            .await
            .unwrap();
    }
    let reader = user(&pool, f.org, "READER", vec![]).await;
    let reads = source.reads.load(Ordering::SeqCst);
    let first = store
        .history(reader, f.proposal, snapshot, None)
        .await
        .unwrap();
    assert_eq!(first.entries.len(), 20);
    assert!(
        first.domains.is_empty() && !first.approve_all_available && first.unviewed_content_warning
    );
    let last = store
        .history(reader, f.proposal, snapshot, first.next_after)
        .await
        .unwrap();
    assert_eq!(last.entries.len(), 3);
    assert!(last.next_after.is_none());
    assert!(
        last.entries.last().unwrap().effective
            && last.entries.last().unwrap().valid_for_stored_context
    );
    let foreign = fixture(&pool).await;
    assert!(matches!(
        store
            .history(foreign.owner, f.proposal, snapshot, None)
            .await,
        Err(OfficialReviewError::NotFound)
    ));
    assert!(matches!(
        store
            .history(reader, f.proposal, snapshot, Some(Uuid::new_v4()))
            .await,
        Err(OfficialReviewError::Invalid)
    ));
    sqlx::query(
        "UPDATE memberships SET review_domains='{}' WHERE organization_id=$1 AND user_id=$2",
    )
    .bind(f.org)
    .bind(f.product)
    .execute(&pool)
    .await
    .unwrap();
    let revoked = store
        .history(reader, f.proposal, snapshot, first.next_after)
        .await
        .unwrap();
    let approval = revoked.entries.last().unwrap();
    assert!(
        approval.effective && !approval.reviewer_eligible && !approval.valid_for_stored_context
    );
    sqlx::query("UPDATE proposals SET state='CLOSED' WHERE id=$1")
        .bind(f.proposal)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        !store
            .history(reader, f.proposal, snapshot, None)
            .await
            .unwrap()
            .current_for_metadata
    );
    assert_eq!(source.reads.load(Ordering::SeqCst), reads);
}

#[sqlx::test(migrations = "../../migrations")]
async fn decision_quota_limits_new_actions_without_blocking_an_exact_replay(pool: PgPool) {
    let (f, _, snapshot, fp, factory) = ready(&pool, true).await;
    let store = PgOfficialReviews::new(pool.clone());
    let first = request(&fp, ReviewDecision::Comment);
    store
        .submit(f.product, f.proposal, snapshot, first.clone(), &factory)
        .await
        .unwrap();
    for _ in 1..30 {
        store
            .submit(
                f.product,
                f.proposal,
                snapshot,
                request(&fp, ReviewDecision::Comment),
                &factory,
            )
            .await
            .unwrap();
    }
    assert_eq!(
        store
            .submit(
                f.product,
                f.proposal,
                snapshot,
                request(&fp, ReviewDecision::Comment),
                &factory
            )
            .await
            .err(),
        Some(OfficialReviewError::Quota)
    );
    assert!(
        store
            .submit(f.product, f.proposal, snapshot, first, &factory)
            .await
            .unwrap()
            .replayed
    );
    assert_eq!(counts(&pool).await, (30, 0, 30, 30));
}
async fn counts(pool: &PgPool) -> (i64, i64, i64, i64) {
    sqlx::query_as("SELECT (SELECT count(*) FROM official_reviews),(SELECT count(*) FROM official_review_effective),(SELECT count(*) FROM audit_events WHERE action='OFFICIAL_REVIEW_RECORDED'),(SELECT count(*) FROM outbox WHERE event_type='REVIEW_DECISION_RECORDED')").fetch_one(pool).await.unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
async fn provider_capacity_is_bounded_without_holding_database_connections_during_reads(
    pool: PgPool,
) {
    let (f, source, snapshot, fp, factory) = ready(&pool, true).await;
    let store = Arc::new(PgOfficialReviews::new(pool.clone()));
    let factory = Arc::new(factory);
    source.blocked.store(true, Ordering::SeqCst);
    let before = source.reads.load(Ordering::SeqCst);
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let store = store.clone();
        let factory = factory.clone();
        let req = request(&fp, ReviewDecision::Comment);
        let proposal = f.proposal;
        let reviewer = f.product;
        tasks.push(tokio::spawn(async move {
            store
                .submit(reviewer, proposal, snapshot, req, factory.as_ref())
                .await
        }));
    }
    tokio::time::timeout(Duration::from_secs(10), async {
        while source.reads.load(Ordering::SeqCst) < before + 8 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        store
            .submit(
                f.product,
                f.proposal,
                snapshot,
                request(&fp, ReviewDecision::Comment),
                factory.as_ref()
            )
            .await
            .err(),
        Some(OfficialReviewError::Capacity)
    );
    source.blocked.store(false, Ordering::SeqCst);
    source.release.notify_waiters();
    for task in tasks {
        task.await.unwrap().unwrap();
    }
    assert_eq!(counts(&pool).await, (8, 0, 8, 8));
}
#[sqlx::test(migrations = "../../migrations")]
async fn official_approval_comments_and_change_requests_preserve_history_without_publishing(
    pool: PgPool,
) {
    let (f, _, snapshot, fp, factory) = ready(&pool, true).await;
    let store = PgOfficialReviews::new(pool.clone());
    let approve = store
        .submit(
            f.product,
            f.proposal,
            snapshot,
            request(&fp, ReviewDecision::ApproveAll),
            &factory,
        )
        .await
        .unwrap();
    let comment = store
        .submit(
            f.product,
            f.proposal,
            snapshot,
            request(&fp, ReviewDecision::Comment),
            &factory,
        )
        .await
        .unwrap();
    let effective: Uuid = sqlx::query_scalar("SELECT review_id FROM official_review_effective")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(effective, approve.id);
    assert_ne!(effective, comment.id);
    store
        .submit(
            f.product,
            f.proposal,
            snapshot,
            request(&fp, ReviewDecision::RequestChanges),
            &factory,
        )
        .await
        .unwrap();
    let state: String = sqlx::query_scalar("SELECT state FROM proposals WHERE id=$1")
        .bind(f.proposal)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state, "CHANGES_REQUESTED");
    store
        .submit(
            f.product,
            f.proposal,
            snapshot,
            request(&fp, ReviewDecision::ApproveAll),
            &factory,
        )
        .await
        .unwrap();
    let state: String = sqlx::query_scalar("SELECT state FROM proposals WHERE id=$1")
        .bind(f.proposal)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state, "IN_REVIEW");
    assert_eq!(counts(&pool).await, (4, 1, 4, 4));
}
#[sqlx::test(migrations = "../../migrations")]
async fn identical_replays_and_concurrent_requests_never_duplicate_decision_events(pool: PgPool) {
    let (f, source, snapshot, fp, factory) = ready(&pool, true).await;
    let store = PgOfficialReviews::new(pool.clone());
    let req = request(&fp, ReviewDecision::ApproveAll);
    let (a, b) = tokio::join!(
        store.submit(f.product, f.proposal, snapshot, req.clone(), &factory),
        store.submit(f.product, f.proposal, snapshot, req.clone(), &factory)
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(a.id, b.id);
    assert_ne!(a.replayed, b.replayed);
    assert_eq!(counts(&pool).await, (1, 1, 1, 1));
    let reads = source.reads.load(Ordering::SeqCst);
    sqlx::query("UPDATE proposals SET state='CLOSED' WHERE id=$1")
        .bind(f.proposal)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        store
            .submit(f.product, f.proposal, snapshot, req.clone(), &factory)
            .await
            .unwrap()
            .replayed
    );
    assert_eq!(reads, source.reads.load(Ordering::SeqCst));
    let mut changed = req;
    changed.explanation = "Different explanation".into();
    assert_eq!(
        store
            .submit(f.product, f.proposal, snapshot, changed, &factory)
            .await
            .err(),
        Some(OfficialReviewError::IdempotencyMismatch)
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn current_qualified_assignment_and_independence_are_mandatory(pool: PgPool) {
    let (f, _, snapshot, fp, factory) = ready(&pool, true).await;
    let store = PgOfficialReviews::new(pool.clone());
    for actor in [
        f.owner,
        user(&pool, f.org, "READER", vec![]).await,
        user(&pool, f.org, "REVIEWER", vec!["PRODUCT"]).await,
    ] {
        assert_eq!(
            store
                .submit(
                    actor,
                    f.proposal,
                    snapshot,
                    request(&fp, ReviewDecision::ApproveAll),
                    &factory
                )
                .await
                .err(),
            Some(OfficialReviewError::Forbidden)
        );
    }
    let mut wrong = request(&fp, ReviewDecision::ApproveAll);
    wrong.domain = ReviewArea::Development;
    assert_eq!(
        store
            .submit(f.product, f.proposal, snapshot, wrong, &factory)
            .await
            .err(),
        Some(OfficialReviewError::Forbidden)
    );
    sqlx::query(
        "UPDATE memberships SET review_domains='{}' WHERE organization_id=$1 AND user_id=$2",
    )
    .bind(f.org)
    .bind(f.product)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        store
            .submit(
                f.product,
                f.proposal,
                snapshot,
                request(&fp, ReviewDecision::ApproveAll),
                &factory
            )
            .await
            .err(),
        Some(OfficialReviewError::Forbidden)
    );
    assert_eq!(counts(&pool).await, (0, 0, 0, 0));
}
#[sqlx::test(migrations = "../../migrations")]
async fn stale_scope_provider_drift_and_outdated_base_do_not_record_approval(pool: PgPool) {
    let (f, source, snapshot, fp, factory) = ready(&pool, false).await;
    let store = PgOfficialReviews::new(pool.clone());
    assert_eq!(
        store
            .submit(
                f.product,
                f.proposal,
                snapshot,
                request(&fp, ReviewDecision::ApproveAll),
                &factory
            )
            .await
            .err(),
        Some(OfficialReviewError::BaseOutdated)
    );
    source.branch_head.store(9, Ordering::SeqCst);
    assert_eq!(
        store
            .submit(
                f.product,
                f.proposal,
                snapshot,
                request(&fp, ReviewDecision::RequestChanges),
                &factory
            )
            .await
            .err(),
        Some(OfficialReviewError::Stale)
    );
    source.branch_head.store(1, Ordering::SeqCst);
    source.target.store(3, Ordering::SeqCst);
    assert_eq!(
        store
            .submit(
                f.product,
                f.proposal,
                snapshot,
                request(&fp, ReviewDecision::Comment),
                &factory
            )
            .await
            .err(),
        Some(OfficialReviewError::Stale)
    );
    assert_eq!(counts(&pool).await, (0, 0, 0, 0));
}
#[sqlx::test(migrations = "../../migrations")]
async fn revoked_membership_during_provider_io_discards_the_decision(pool: PgPool) {
    let (f, source, snapshot, fp, factory) = ready(&pool, true).await;
    // Discard the preparation notification, then block only the next ref read.
    let _ = tokio::time::timeout(Duration::from_millis(5), source.started.notified()).await;
    source.blocked.store(true, Ordering::SeqCst);
    let proposal = f.proposal;
    let reviewer = f.product;
    let connection = pool.clone();
    let task = tokio::spawn(async move {
        PgOfficialReviews::new(connection)
            .submit(
                reviewer,
                proposal,
                snapshot,
                request(&fp, ReviewDecision::ApproveAll),
                &factory,
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), source.started.notified())
        .await
        .unwrap();
    sqlx::query("UPDATE users SET status='DISABLED' WHERE id=$1")
        .bind(f.product)
        .execute(&pool)
        .await
        .unwrap();
    source.blocked.store(false, Ordering::SeqCst);
    source.release.notify_one();
    assert_eq!(
        task.await.unwrap().err(),
        Some(OfficialReviewError::NotFound)
    );
    assert_eq!(counts(&pool).await, (0, 0, 0, 0));
}
#[sqlx::test(migrations = "../../migrations")]
async fn audit_failure_rolls_back_every_row_and_history_and_events_are_immutable(pool: PgPool) {
    let (f, _, snapshot, fp, factory) = ready(&pool, true).await;
    let store = PgOfficialReviews::new(pool.clone());
    sqlx::query("CREATE FUNCTION fail_official_review() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='OFFICIAL_REVIEW_RECORDED' THEN RAISE EXCEPTION 'synthetic'; END IF; RETURN NEW; END $$").execute(&pool).await.unwrap();
    sqlx::query("CREATE TRIGGER fail_official_review BEFORE INSERT ON audit_events FOR EACH ROW EXECUTE FUNCTION fail_official_review()").execute(&pool).await.unwrap();
    let req = request(&fp, ReviewDecision::ApproveAll);
    assert!(
        store
            .submit(f.product, f.proposal, snapshot, req.clone(), &factory)
            .await
            .is_err()
    );
    assert_eq!(counts(&pool).await, (0, 0, 0, 0));
    sqlx::query("DROP TRIGGER fail_official_review ON audit_events")
        .execute(&pool)
        .await
        .unwrap();
    let receipt = store
        .submit(f.product, f.proposal, snapshot, req, &factory)
        .await
        .unwrap();
    for sql in [
        "UPDATE official_reviews SET explanation='tampered' WHERE id=$1",
        "DELETE FROM official_reviews WHERE id=$1",
        "DELETE FROM official_review_effective WHERE review_id=$1",
        "UPDATE outbox SET payload='{}' WHERE id=(SELECT event_id FROM official_reviews WHERE id=$1)",
    ] {
        assert!(
            sqlx::query(sql)
                .bind(receipt.id)
                .execute(&pool)
                .await
                .is_err(),
            "{sql}"
        );
    }
}
