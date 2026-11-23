use super::*;
use specspace_application::review_visits::{
    ReviewCoverage, ReviewCoverageRequest, ReviewSideCoverage as State,
};
use specspace_application::{
    proposal::UiImpact,
    review_configuration::{ReviewConfigurationRequest, ReviewSettings, ReviewerSelection},
};
use specspace_infrastructure::official_reviews::PgOfficialReviews;
use specspace_infrastructure::review_configuration::PgReviewConfiguration;
async fn history(
    pool: &PgPool,
    r: &Ready,
    actor: Uuid,
) -> specspace_application::official_review::OfficialReviewHistory {
    PgOfficialReviews::new(pool.clone())
        .history(actor, r.f.proposal, r.snapshot, None)
        .await
        .unwrap()
}
async fn coverage(r: &Ready) -> ReviewCoverage {
    r.store
        .coverage(
            r.f.product,
            r.f.proposal,
            r.snapshot,
            ReviewCoverageRequest {
                expected_fingerprint: r.request.expected_fingerprint.clone(),
                domain: ReviewArea::Product,
                offset: 0,
                limit: 100,
            },
        )
        .await
        .unwrap()
}
async fn acknowledge(r: &Ready, side: AnchorSide, offset: usize) -> ReviewSourceWindow {
    let mut request = r.request.clone();
    request.side = side;
    request.offset = offset;
    let w = r
        .store
        .deliver(r.f.product, r.f.proposal, r.snapshot, request)
        .await
        .unwrap();
    r.store
        .acknowledge(r.f.product, r.f.proposal, r.snapshot, ack(&w))
        .await
        .unwrap();
    w
}
pub(super) async fn publish_context(
    pool: &PgPool,
    f: &Fixture,
    preparer: &ReviewPreparer,
    version: i64,
    configuration: i64,
    policy: i64,
) -> (Uuid, String, String) {
    let submissions = PgReviewSubmissions::new(pool.clone());
    let input = ValidatedReviewSubmission::new(
        f.owner,
        f.proposal,
        ReviewSubmissionRequest {
            expected_head_sha: sha(1).as_str().into(),
            expected_target_sha: sha(2).as_str().into(),
            expected_proposal_version: version,
            expected_configuration_version: configuration,
            expected_policy_version: policy,
        },
    )
    .unwrap();
    submissions
        .reserve(
            &input,
            &IdempotencyKey::parse(Uuid::new_v4().to_string()).unwrap(),
        )
        .await
        .unwrap();
    let lease = submissions
        .claim(Uuid::new_v4(), 300)
        .await
        .unwrap()
        .unwrap();
    let bound = preparer.with_authorization(Arc::new(submissions.authorization(&lease)));
    let candidate = bound
        .prepare(f.owner, f.proposal, lease.expectation().clone())
        .await
        .unwrap();
    let verified = bound.verify_media(candidate).await.unwrap();
    let unit = verified
        .candidate()
        .plan()
        .required_units()
        .iter()
        .find(|u| u.after.as_ref().is_some_and(|s| s.range.is_some()))
        .unwrap()
        .id
        .clone();
    let saved = PgReviewSnapshots::new(pool.clone())
        .publish(&lease, &verified, &bound)
        .await
        .unwrap();
    (saved.snapshot_id, saved.fingerprint, unit)
}
pub(super) async fn custom(
    pool: &PgPool,
    body: Option<&str>,
    media: bool,
    dual_domain: bool,
) -> Ready {
    let (f, source, preparer) = setup_body(pool, media, body).await;
    let (version, configuration) = if dual_domain {
        sqlx::query(
            "UPDATE memberships SET review_domains=ARRAY['PRODUCT','DESIGN'] WHERE user_id=$1",
        )
        .bind(f.product)
        .execute(pool)
        .await
        .unwrap();
        PgReviewConfiguration::new(pool.clone())
            .update(
                f.owner,
                f.proposal,
                ReviewConfigurationRequest {
                    request_id: Uuid::new_v4(),
                    expected_head_sha: sha(1).as_str().into(),
                    expected_proposal_version: 2,
                    expected_configuration_version: 2,
                    settings: ReviewSettings {
                        ui_impact: UiImpact::Yes,
                        ui_impact_reason: None,
                        assignments: vec![
                            ReviewerSelection {
                                reviewer_id: f.product,
                                domain: ReviewArea::Product,
                                required: true,
                            },
                            ReviewerSelection {
                                reviewer_id: f.development,
                                domain: ReviewArea::Development,
                                required: true,
                            },
                            ReviewerSelection {
                                reviewer_id: f.product,
                                domain: ReviewArea::Design,
                                required: true,
                            },
                        ],
                    },
                },
            )
            .await
            .unwrap();
        (3, 3)
    } else {
        (2, 2)
    };
    let (snapshot, fp, unit) =
        publish_context(pool, &f, &preparer, version, configuration, 1).await;
    let service = store(
        pool,
        Arc::new(Factory {
            source: source.clone(),
            repository: f.repository,
        }),
    );
    Ready {
        f,
        source,
        snapshot,
        request: ReviewSourceRequest {
            expected_fingerprint: fp,
            domain: ReviewArea::Product,
            unit_id: unit,
            side: AnchorSide::After,
            offset: 0,
        },
        store: service,
    }
}
#[sqlx::test(migrations = "../../migrations")]
async fn coverage_requires_explicit_acknowledgement_of_every_present_side_without_provider_io(
    pool: PgPool,
) {
    let r = ready(&pool).await;
    let before = coverage(&r).await;
    assert!(before.available && before.current_for_metadata);
    assert_eq!(
        (
            before.total_units,
            before.viewed_units,
            before.unviewed_units
        ),
        (1, 0, 1)
    );
    assert_eq!(before.units[0].before, State::Unviewed);
    assert_eq!(before.units[0].after, State::Unviewed);
    assert_eq!(
        history(&pool, &r, r.f.product).await.unviewed_domains,
        vec![ReviewArea::Product]
    );
    deliver(&r).await.unwrap();
    assert_eq!(coverage(&r).await.units[0].after, State::Unviewed);
    acknowledge(&r, AnchorSide::After, 0).await;
    let after = coverage(&r).await;
    assert_eq!(after.units[0].after, State::Viewed);
    assert!(!after.units[0].viewed);
    assert_eq!(after.viewed_units, 0);
    assert!(
        history(&pool, &r, r.f.product)
            .await
            .unviewed_content_warning
    );
    acknowledge(&r, AnchorSide::Before, 0).await;
    let reads = r.source.reads.load(Ordering::SeqCst);
    let complete = coverage(&r).await;
    assert!(complete.units[0].viewed);
    assert_eq!((complete.viewed_units, complete.unviewed_units), (1, 0));
    let warning = history(&pool, &r, r.f.product).await;
    assert!(!warning.unviewed_content_warning && warning.unviewed_domains.is_empty());
    assert_eq!(r.source.reads.load(Ordering::SeqCst), reads);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM official_reviews")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn overlapping_duplicate_and_empty_tail_visits_cannot_hide_unread_gaps(pool: PgPool) {
    let r = custom(&pool, Some(&"x".repeat(131100)), false, false).await;
    let first = deliver(&r).await.unwrap();
    let span = first.source.range.as_ref().unwrap();
    let length = span.end_byte - span.start_byte;
    acknowledge(&r, AnchorSide::After, length).await;
    assert_eq!(coverage(&r).await.units[0].after, State::Unviewed);
    acknowledge(&r, AnchorSide::After, 0).await;
    acknowledge(&r, AnchorSide::After, 0).await;
    acknowledge(&r, AnchorSide::After, 131072).await;
    assert_eq!(coverage(&r).await.units[0].after, State::Partial);
    assert!(
        history(&pool, &r, r.f.product)
            .await
            .unviewed_content_warning
    );
    acknowledge(&r, AnchorSide::After, 65536).await;
    let joined = coverage(&r).await;
    assert_eq!(joined.units[0].after, State::Viewed);
    assert!(!joined.units[0].viewed);
    acknowledge(&r, AnchorSide::Before, 0).await;
    assert_eq!(coverage(&r).await.viewed_units, 1);
}
#[sqlx::test(migrations = "../../migrations")]
async fn identical_actor_and_unit_ids_do_not_transfer_coverage_across_domains_or_policy_snapshots(
    pool: PgPool,
) {
    let r = custom(&pool, None, false, true).await;
    acknowledge(&r, AnchorSide::Before, 0).await;
    acknowledge(&r, AnchorSide::After, 0).await;
    assert_eq!(coverage(&r).await.viewed_units, 1);
    let design = r
        .store
        .coverage(
            r.f.product,
            r.f.proposal,
            r.snapshot,
            ReviewCoverageRequest {
                expected_fingerprint: r.request.expected_fingerprint.clone(),
                domain: ReviewArea::Design,
                offset: 0,
                limit: 20,
            },
        )
        .await
        .unwrap();
    assert_eq!(design.viewed_units, 0);
    sqlx::query("UPDATE organizations SET policy_version=2 WHERE id=$1")
        .bind(r.f.org)
        .execute(&pool)
        .await
        .unwrap();
    let comparison = Arc::new(
        ComparisonReader::new(
            Arc::new(PgComparisonAuthorization::new(pool.clone())),
            Arc::new(Factory {
                source: r.source.clone(),
                repository: r.f.repository,
            }),
            Duration::from_secs(900),
        )
        .unwrap(),
    );
    let preparer = ReviewPreparer::new(
        Arc::new(PgReviewAuthorization::new(pool.clone())),
        comparison,
        Arc::new(Factory {
            source: r.source.clone(),
            repository: r.f.repository,
        }),
        Duration::from_secs(900),
    )
    .unwrap();
    let version: i64 = sqlx::query_scalar("SELECT version FROM proposals WHERE id=$1")
        .bind(r.f.proposal)
        .fetch_one(&pool)
        .await
        .unwrap();
    let (snapshot, fp, unit) = publish_context(&pool, &r.f, &preparer, version, 3, 2).await;
    assert_ne!(snapshot, r.snapshot);
    assert_eq!(unit, r.request.unit_id);
    let fresh = r
        .store
        .coverage(
            r.f.product,
            r.f.proposal,
            snapshot,
            ReviewCoverageRequest {
                expected_fingerprint: fp,
                domain: ReviewArea::Product,
                offset: 0,
                limit: 20,
            },
        )
        .await
        .unwrap();
    assert_eq!(fresh.viewed_units, 0);
    let old = coverage(&r).await;
    assert!(!old.current_for_metadata);
    assert_eq!(old.viewed_units, 1);
    let historical = history(&pool, &r, r.f.product).await;
    assert!(
        !historical.current_for_metadata
            && historical.domains.is_empty()
            && historical.unviewed_content_warning
    );
    let fresh = PgOfficialReviews::new(pool.clone())
        .history(r.f.product, r.f.proposal, snapshot, None)
        .await
        .unwrap();
    assert_eq!(
        fresh.unviewed_domains,
        vec![ReviewArea::Design, ReviewArea::Product]
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn warning_is_scoped_to_each_eligible_actor_domain_and_never_creates_a_decision(
    pool: PgPool,
) {
    let r = custom(&pool, None, false, true).await;
    assert_eq!(
        history(&pool, &r, r.f.product).await.unviewed_domains,
        vec![ReviewArea::Design, ReviewArea::Product]
    );
    acknowledge(&r, AnchorSide::Before, 0).await;
    acknowledge(&r, AnchorSide::After, 0).await;
    let product_read = history(&pool, &r, r.f.product).await;
    assert!(product_read.unviewed_content_warning);
    assert_eq!(product_read.unviewed_domains, vec![ReviewArea::Design]);
    for side in [AnchorSide::Before, AnchorSide::After] {
        let mut request = r.request.clone();
        request.domain = ReviewArea::Design;
        request.side = side;
        let w = r
            .store
            .deliver(r.f.product, r.f.proposal, r.snapshot, request)
            .await
            .unwrap();
        r.store
            .acknowledge(r.f.product, r.f.proposal, r.snapshot, ack(&w))
            .await
            .unwrap();
    }
    assert!(
        !history(&pool, &r, r.f.product)
            .await
            .unviewed_content_warning
    );
    assert_eq!(
        history(&pool, &r, r.f.development).await.unviewed_domains,
        vec![ReviewArea::Development]
    );
    let author = history(&pool, &r, r.f.owner).await;
    assert!(
        author.domains.is_empty()
            && author.unviewed_domains.is_empty()
            && author.unviewed_content_warning
    );
    sqlx::query("UPDATE memberships SET review_domains='{}' WHERE user_id=$1")
        .bind(r.f.product)
        .execute(&pool)
        .await
        .unwrap();
    let revoked = history(&pool, &r, r.f.product).await;
    assert!(revoked.domains.is_empty() && revoked.unviewed_content_warning);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM official_reviews")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn reading_all_text_cannot_hide_off_page_unsupported_work_from_approve_all_warning(
    pool: PgPool,
) {
    let r = custom(&pool, None, true, false).await;
    let page = PgReviewSnapshots::new(pool.clone())
        .units(r.f.product, r.f.proposal, r.snapshot, 0, 100)
        .await
        .unwrap();
    for unit in page.units {
        for side in [AnchorSide::Before, AnchorSide::After] {
            let source = match side {
                AnchorSide::Before => &unit.before,
                AnchorSide::After => &unit.after,
            };
            if !source.as_ref().is_some_and(|s| s.range.is_some()) {
                continue;
            }
            let mut request = r.request.clone();
            request.unit_id = unit.id.clone();
            request.side = side;
            let w = r
                .store
                .deliver(r.f.product, r.f.proposal, r.snapshot, request)
                .await
                .unwrap();
            r.store
                .acknowledge(r.f.product, r.f.proposal, r.snapshot, ack(&w))
                .await
                .unwrap();
        }
    }
    let counts = coverage(&r).await;
    assert!(counts.viewed_units > 0 && counts.unsupported_units > 0);
    let hints = history(&pool, &r, r.f.product).await;
    assert!(hints.unviewed_content_warning);
    assert_eq!(hints.unviewed_domains, vec![ReviewArea::Product]);
}
#[sqlx::test(migrations = "../../migrations")]
async fn coverage_reports_unsupported_media_and_bounds_paging_scope_and_current_membership(
    pool: PgPool,
) {
    let r = custom(&pool, None, true, false).await;
    let page = coverage(&r).await;
    assert!(page.unsupported_units > 0);
    assert_eq!(page.viewed_units, 0);
    assert!(page.units.iter().any(|u| u.after == State::Unsupported));
    for (offset, limit) in [(-1, 20), (40001, 20), (0, 0), (0, 101)] {
        assert!(matches!(
            r.store
                .coverage(
                    r.f.product,
                    r.f.proposal,
                    r.snapshot,
                    ReviewCoverageRequest {
                        expected_fingerprint: r.request.expected_fingerprint.clone(),
                        domain: ReviewArea::Product,
                        offset,
                        limit
                    }
                )
                .await,
            Err(VisitError::Invalid)
        ));
    }
    let first = r
        .store
        .coverage(
            r.f.product,
            r.f.proposal,
            r.snapshot,
            ReviewCoverageRequest {
                expected_fingerprint: r.request.expected_fingerprint.clone(),
                domain: ReviewArea::Product,
                offset: 0,
                limit: 1,
            },
        )
        .await
        .unwrap();
    assert_eq!(first.next_offset, Some(1));
    let end = r
        .store
        .coverage(
            r.f.product,
            r.f.proposal,
            r.snapshot,
            ReviewCoverageRequest {
                expected_fingerprint: r.request.expected_fingerprint.clone(),
                domain: ReviewArea::Product,
                offset: 40000,
                limit: 20,
            },
        )
        .await
        .unwrap();
    assert!(end.units.is_empty());
    assert_eq!(end.next_offset, None);
    assert!(
        r.store
            .coverage(
                r.f.owner,
                r.f.proposal,
                r.snapshot,
                ReviewCoverageRequest {
                    expected_fingerprint: r.request.expected_fingerprint.clone(),
                    domain: ReviewArea::Product,
                    offset: 0,
                    limit: 20
                }
            )
            .await
            .is_err()
    );
    sqlx::query("DELETE FROM memberships WHERE user_id=$1")
        .bind(r.f.product)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        r.store
            .coverage(
                r.f.product,
                r.f.proposal,
                r.snapshot,
                ReviewCoverageRequest {
                    expected_fingerprint: r.request.expected_fingerprint.clone(),
                    domain: ReviewArea::Product,
                    offset: 0,
                    limit: 20
                }
            )
            .await
            .is_err()
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn added_section_coverage_requires_only_its_present_side(pool: PgPool) {
    let r = custom(
        &pool,
        Some("Changed\n\n<!-- ss:section:SEC-NEW -->\n## New\n\nAdded source"),
        false,
        false,
    )
    .await;
    let page = PgReviewSnapshots::new(pool.clone())
        .units(r.f.owner, r.f.proposal, r.snapshot, 0, 100)
        .await
        .unwrap();
    let unit=page.units.iter().find(|u|matches!(&u.requirement,specspace_application::review_units::Requirement::Section {section_id,..} if section_id=="SEC-NEW")).unwrap();
    let before = coverage(&r).await;
    let item = before.units.iter().find(|u| u.unit_id == unit.id).unwrap();
    assert_eq!(item.before, State::Absent);
    assert_eq!(item.after, State::Unviewed);
    assert!(!item.viewed);
    let mut request = r.request.clone();
    request.unit_id = unit.id.clone();
    let delivered = r
        .store
        .deliver(r.f.product, r.f.proposal, r.snapshot, request)
        .await
        .unwrap();
    r.store
        .acknowledge(r.f.product, r.f.proposal, r.snapshot, ack(&delivered))
        .await
        .unwrap();
    let after = coverage(&r).await;
    let item = after.units.iter().find(|u| u.unit_id == unit.id).unwrap();
    assert_eq!(item.before, State::Absent);
    assert_eq!(item.after, State::Viewed);
    assert!(item.viewed);
    assert_eq!(after.viewed_units, 1);
}
