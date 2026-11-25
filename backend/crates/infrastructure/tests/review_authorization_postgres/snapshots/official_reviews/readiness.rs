use super::*;
use specspace_application::review_readiness::{
    ReadinessConclusion as C, ReadinessGate as G, ReviewReadiness,
};
use specspace_infrastructure::review_readiness::{PgReviewReadiness, ReadinessError};

fn issue(report: &ReviewReadiness, gate: G) -> bool {
    report.evaluation.issues.iter().any(|i| i.gate == gate)
}

#[sqlx::test(migrations = "../../migrations")]
async fn readiness_counts_outdated_blocking_threads_until_explicit_resolution(pool: PgPool) {
    use specspace_application::discussion::{
        OpenThreadRequest, ThreadMutation, ThreadMutationRequest, ThreadOrigin,
    };
    use specspace_infrastructure::discussions::{OpenAdmissionResult, PgDiscussions};
    let (f, _, h, _, _) = ready(&pool, true).await;
    let version: i64 = sqlx::query_scalar("SELECT version FROM proposals WHERE id=$1")
        .bind(f.proposal)
        .fetch_one(&pool)
        .await
        .unwrap();
    let discussion = PgDiscussions::new(pool.clone());
    let OpenAdmissionResult::Ready(admission) = discussion
        .admit_open(
            f.product,
            f.proposal,
            OpenThreadRequest {
                request_id: Uuid::new_v4(),
                origin: ThreadOrigin::Proposal {
                    expected_proposal_version: version,
                },
                blocking: true,
                body: "Independent blocking requirement".into(),
            },
        )
        .await
        .unwrap()
    else {
        panic!("new admission");
    };
    let opened = discussion.complete_open(&admission, None).await.unwrap();
    let service = PgReviewReadiness::new(pool.clone());
    assert!(issue(
        &service.stored(f.owner, f.proposal, h).await.unwrap(),
        G::BlockingThreads
    ));
    discussion
        .mutate(
            f.product,
            f.proposal,
            opened.thread_id,
            ThreadMutationRequest {
                request_id: Uuid::new_v4(),
                expected_thread_version: 1,
                mutation: ThreadMutation::Tombstone {
                    comment_id: opened.comment_id.unwrap(),
                    expected_comment_version: 1,
                },
            },
        )
        .await
        .unwrap();
    sqlx::query("UPDATE proposals SET head_sha=$2,version=version+1 WHERE id=$1")
        .bind(f.proposal)
        .bind("3".repeat(40))
        .execute(&pool)
        .await
        .unwrap();
    let outdated = service.stored(f.owner, f.proposal, h).await.unwrap();
    assert!(issue(&outdated, G::Snapshot) && issue(&outdated, G::BlockingThreads));
    discussion
        .mutate(
            f.product,
            f.proposal,
            opened.thread_id,
            ThreadMutationRequest {
                request_id: Uuid::new_v4(),
                expected_thread_version: 2,
                mutation: ThreadMutation::Resolve {},
            },
        )
        .await
        .unwrap();
    let resolved = service.stored(f.owner, f.proposal, h).await.unwrap();
    assert!(!issue(&resolved, G::BlockingThreads));
    assert_eq!(resolved.evaluation.conclusion, C::Failure);
}
async fn pair(pool: &PgPool, f: &Fixture, h: Uuid, fp: &str, factory: &Factory) {
    let service = PgOfficialReviews::new(pool.clone());
    for (actor, area) in [
        (f.product, ReviewArea::Product),
        (f.development, ReviewArea::Development),
    ] {
        let mut r = request(fp, ReviewDecision::ApproveAll);
        r.domain = area;
        service
            .submit(actor, f.proposal, h, r, factory)
            .await
            .unwrap();
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn stored_readiness_assembles_independent_approvals_without_treating_missing_integrations_as_passed(
    pool: PgPool,
) {
    let (f, source, h, fp, factory) = ready(&pool, true).await;
    let service = PgReviewReadiness::new(pool.clone());
    let initial = service.stored(f.owner, f.proposal, h).await.unwrap();
    assert!(initial.current_for_metadata && !initial.provider_refs_verified);
    assert_eq!(initial.evaluation.conclusion, C::Pending);
    assert!(
        issue(&initial, G::Approvals)
            && issue(&initial, G::RequiredReviewer)
            && issue(&initial, G::SectionCoverage)
    );
    assert!(
        !issue(&initial, G::ContentValidation)
            && !issue(&initial, G::MediaValidation)
            && !issue(&initial, G::BlockingThreads)
    );
    pair(&pool, &f, h, &fp, &factory).await;
    let reads = source.reads.load(Ordering::SeqCst);
    let report = service.stored(f.owner, f.proposal, h).await.unwrap();
    assert_eq!(report.evaluation.conclusion, C::Pending);
    assert_eq!(report.evaluation.issues.len(), 2);
    assert!(issue(&report, G::BlockingQuestions));
    assert!(
        report
            .evaluation
            .issues
            .iter()
            .any(|i| i.gate == G::Evidence && i.code == "PROVIDER_REFS_UNVERIFIED")
    );
    let units: i64 = sqlx::query_scalar("SELECT count(*) FROM review_unit_approvals")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(units, 0);
    assert_eq!(source.reads.load(Ordering::SeqCst), reads);
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM proposals WHERE id=$1")
            .bind(f.proposal)
            .fetch_one(&pool)
            .await
            .unwrap(),
        "IN_REVIEW"
    );
    sqlx::query("DELETE FROM memberships WHERE organization_id=$1 AND user_id=$2")
        .bind(f.org)
        .bind(f.development)
        .execute(&pool)
        .await
        .unwrap();
    let revoked = service.stored(f.owner, f.proposal, h).await.unwrap();
    assert!(issue(&revoked, G::Approvals) && issue(&revoked, G::RequiredReviewer));
}

#[sqlx::test(migrations = "../../migrations")]
async fn readiness_keeps_effective_change_requests_after_comments_and_rejects_stale_context(
    pool: PgPool,
) {
    let (f, source, h, fp, factory) = ready(&pool, true).await;
    pair(&pool, &f, h, &fp, &factory).await;
    let decisions = PgOfficialReviews::new(pool.clone());
    for action in [ReviewDecision::RequestChanges, ReviewDecision::Comment] {
        decisions
            .submit(f.product, f.proposal, h, request(&fp, action), &factory)
            .await
            .unwrap();
    }
    let service = PgReviewReadiness::new(pool.clone());
    let reads = source.reads.load(Ordering::SeqCst);
    let report = service.stored(f.owner, f.proposal, h).await.unwrap();
    assert_eq!(report.evaluation.conclusion, C::Failure);
    assert!(
        report
            .evaluation
            .issues
            .iter()
            .any(|i| i.code == "CHANGES_REQUESTED")
    );
    sqlx::query("UPDATE proposals SET head_sha=$2,version=version+1 WHERE id=$1")
        .bind(f.proposal)
        .bind("3".repeat(40))
        .execute(&pool)
        .await
        .unwrap();
    let stale = service.stored(f.owner, f.proposal, h).await.unwrap();
    assert!(!stale.current_for_metadata);
    assert_eq!(stale.evaluation.conclusion, C::Failure);
    assert!(issue(&stale, G::Snapshot));
    assert_eq!(source.reads.load(Ordering::SeqCst), reads);
    let other = fixture(&pool).await;
    assert_eq!(
        service.stored(other.owner, f.proposal, h).await.err(),
        Some(ReadinessError::NotFound)
    );
    assert_eq!(
        service
            .stored(f.owner, f.proposal, Uuid::new_v4())
            .await
            .err(),
        Some(ReadinessError::NotFound)
    );
    assert_eq!(
        service.stored(f.owner, f.proposal, Uuid::nil()).await.err(),
        Some(ReadinessError::Invalid)
    );
    sqlx::query("UPDATE users SET status='DISABLED' WHERE id=$1")
        .bind(f.owner)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        service.stored(f.owner, f.proposal, h).await.err(),
        Some(ReadinessError::NotFound)
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn incomplete_restored_file_manifest_never_passes_content_readiness(pool: PgPool) {
    let (f, _, h, fp, factory) = ready(&pool, true).await;
    pair(&pool, &f, h, &fp, &factory).await;
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("ALTER TABLE review_snapshot_files DISABLE TRIGGER ALL")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("DELETE FROM review_snapshot_files WHERE snapshot_id=$1 AND ordinal=0")
        .bind(h)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("ALTER TABLE review_snapshot_files ENABLE TRIGGER ALL")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let report = PgReviewReadiness::new(pool)
        .stored(f.owner, f.proposal, h)
        .await
        .unwrap();
    assert_eq!(report.evaluation.conclusion, C::Pending);
    assert!(
        report
            .evaluation
            .issues
            .iter()
            .any(|i| i.gate == G::ContentValidation && i.code == "POLICY_EVIDENCE_UNAVAILABLE")
    );
}
