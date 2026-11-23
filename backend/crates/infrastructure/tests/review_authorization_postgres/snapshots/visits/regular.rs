use super::*;
use specspace_application::{
    official_review::{OfficialReviewRequest, ReviewDecision},
    unit_approval::UnitApprovalRequest,
};
use specspace_infrastructure::{
    official_reviews::PgOfficialReviews, unit_approvals::PgUnitApprovals,
};
async fn ready(pool: &PgPool, dual: bool) -> Ready {
    let (f,source,preparer)=setup_body(pool,false,Some("Changed\n\n<!-- ss:section:SEC-REG-FIRST -->\n## First\n\nFirst change\n\n<!-- ss:section:SEC-REG-SECOND -->\n## Second\n\nSecond change")).await;
    source.base_updated.store(true, Ordering::SeqCst);
    sqlx::query("UPDATE proposals SET base_sha=$2 WHERE id=$1")
        .bind(f.proposal)
        .bind(sha(2).as_str())
        .execute(pool)
        .await
        .unwrap();
    let (version, configuration) = if dual {
        use specspace_application::{
            proposal::UiImpact,
            review_configuration::{ReviewConfigurationRequest, ReviewSettings, ReviewerSelection},
        };
        sqlx::query(
            "UPDATE memberships SET review_domains=ARRAY['PRODUCT','DESIGN'] WHERE user_id=$1",
        )
        .bind(f.product)
        .execute(pool)
        .await
        .unwrap();
        specspace_infrastructure::review_configuration::PgReviewConfiguration::new(pool.clone())
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
        super::coverage::publish_context(pool, &f, &preparer, version, configuration, 1).await;
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
        store: service,
        request: ReviewSourceRequest {
            expected_fingerprint: fp,
            domain: ReviewArea::Product,
            unit_id: unit,
            side: AnchorSide::After,
            offset: 0,
        },
    }
}
fn intent(r: &Ready, domain: ReviewArea, decision: ReviewDecision) -> OfficialReviewRequest {
    OfficialReviewRequest {
        request_id: Uuid::new_v4(),
        expected_fingerprint: r.request.expected_fingerprint.clone(),
        domain,
        decision,
        explanation: "Reviewed every required individual change".into(),
    }
}
fn factory(r: &Ready) -> Factory {
    Factory {
        source: r.source.clone(),
        repository: r.f.repository,
    }
}
async fn units(
    r: &Ready,
    pool: &PgPool,
) -> Vec<specspace_application::review_units::RequiredReviewUnit> {
    PgReviewSnapshots::new(pool.clone())
        .units(r.f.product, r.f.proposal, r.snapshot, 0, 100)
        .await
        .unwrap()
        .units
}
async fn view(
    r: &Ready,
    domain: ReviewArea,
    u: &specspace_application::review_units::RequiredReviewUnit,
) {
    for side in [AnchorSide::Before, AnchorSide::After] {
        let source = match side {
            AnchorSide::Before => &u.before,
            AnchorSide::After => &u.after,
        };
        if source.is_none() {
            continue;
        }
        let mut request = r.request.clone();
        request.domain = domain;
        request.side = side;
        request.unit_id = u.id.clone();
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
async fn approve_unit(r: &Ready, pool: &PgPool, domain: ReviewArea, id: &str) {
    PgUnitApprovals::new(pool.clone())
        .approve(
            r.f.product,
            r.f.proposal,
            r.snapshot,
            UnitApprovalRequest {
                request_id: Uuid::new_v4(),
                expected_fingerprint: r.request.expected_fingerprint.clone(),
                domain,
                unit_id: id.into(),
            },
        )
        .await
        .unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn stored_validity_rechecks_regular_evidence_without_fabricating_all_approval_items(
    pool: PgPool,
) {
    let r = ready(&pool, false).await;
    let service = PgOfficialReviews::new(pool.clone());
    let page = units(&r, &pool).await;
    for unit in &page {
        view(&r, ReviewArea::Product, unit).await;
        approve_unit(&r, &pool, ReviewArea::Product, &unit.id).await;
    }
    let regular = service
        .submit(
            r.f.product,
            r.f.proposal,
            r.snapshot,
            intent(&r, ReviewArea::Product, ReviewDecision::Approve),
            &factory(&r),
        )
        .await
        .unwrap();
    let all = service
        .submit(
            r.f.development,
            r.f.proposal,
            r.snapshot,
            intent(&r, ReviewArea::Development, ReviewDecision::ApproveAll),
            &factory(&r),
        )
        .await
        .unwrap();
    let before = service
        .history(r.f.owner, r.f.proposal, r.snapshot, None)
        .await
        .unwrap();
    assert!(before.current_for_metadata);
    assert!(
        before
            .entries
            .iter()
            .all(|e| e.effective && e.valid_for_stored_context)
    );
    let reads = r.source.reads.load(Ordering::SeqCst);
    let mut tx = pool.begin().await.unwrap();
    // Restore damage must not silently promote a regular decision to All.
    sqlx::query("ALTER TABLE review_unit_approvals DISABLE TRIGGER ALL")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("DELETE FROM review_unit_approvals WHERE organization_id=$1 AND proposal_id=$2 AND snapshot_id=$3 AND reviewer_id=$4 AND domain='PRODUCT' AND unit_id=$5")
        .bind(r.f.org).bind(r.f.proposal).bind(r.snapshot).bind(r.f.product).bind(&page[0].id).execute(&mut *tx).await.unwrap();
    sqlx::query("ALTER TABLE review_unit_approvals ENABLE TRIGGER ALL")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let after = service
        .history(r.f.owner, r.f.proposal, r.snapshot, None)
        .await
        .unwrap();
    let regular = after.entries.iter().find(|e| e.id == regular.id).unwrap();
    assert!(regular.effective && regular.reviewer_eligible && !regular.valid_for_stored_context);
    let all = after.entries.iter().find(|e| e.id == all.id).unwrap();
    assert!(all.effective && all.reviewer_eligible && all.valid_for_stored_context);
    let fabricated: i64 =
        sqlx::query_scalar("SELECT count(*) FROM review_unit_approvals WHERE reviewer_id=$1")
            .bind(r.f.development)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(fabricated, 0);
    assert_eq!(r.source.reads.load(Ordering::SeqCst), reads);
}

#[sqlx::test(migrations = "../../migrations")]
async fn regular_approval_requires_actual_individual_actions_for_every_unit_and_preserves_explicit_all_override(
    pool: PgPool,
) {
    let r = ready(&pool, false).await;
    let service = PgOfficialReviews::new(pool.clone());
    let mut input = intent(&r, ReviewArea::Product, ReviewDecision::Approve);
    input.explanation.clear();
    let before = r.source.reads.load(Ordering::SeqCst);
    assert!(matches!(
        service
            .submit(
                r.f.product,
                r.f.proposal,
                r.snapshot,
                input.clone(),
                &factory(&r)
            )
            .await,
        Err(OfficialReviewError::SectionsIncomplete)
    ));
    assert_eq!(r.source.reads.load(Ordering::SeqCst), before);
    let page = units(&r, &pool).await;
    assert!(page.len() > 1);
    for unit in &page {
        view(&r, ReviewArea::Product, unit).await;
    }
    let read = service
        .history(r.f.product, r.f.proposal, r.snapshot, None)
        .await
        .unwrap();
    assert!(read.unviewed_domains.is_empty() && read.approve_domains.is_empty());
    approve_unit(&r, &pool, ReviewArea::Product, &page[0].id).await;
    assert!(matches!(
        service
            .submit(
                r.f.product,
                r.f.proposal,
                r.snapshot,
                input.clone(),
                &factory(&r)
            )
            .await,
        Err(OfficialReviewError::SectionsIncomplete)
    ));
    for unit in &page[1..] {
        approve_unit(&r, &pool, ReviewArea::Product, &unit.id).await;
    }
    approve_unit(&r, &pool, ReviewArea::Product, &page[0].id).await;
    assert_eq!(
        service
            .history(r.f.product, r.f.proposal, r.snapshot, None)
            .await
            .unwrap()
            .approve_domains,
        vec![ReviewArea::Product]
    );
    let saved = service
        .submit(
            r.f.product,
            r.f.proposal,
            r.snapshot,
            input.clone(),
            &factory(&r),
        )
        .await
        .unwrap();
    assert_eq!(saved.decision, ReviewDecision::Approve);
    let reads = r.source.reads.load(Ordering::SeqCst);
    let replay = service
        .submit(r.f.product, r.f.proposal, r.snapshot, input, &factory(&r))
        .await
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(saved.id, replay.id);
    assert_eq!(reads, r.source.reads.load(Ordering::SeqCst));
    service
        .submit(
            r.f.product,
            r.f.proposal,
            r.snapshot,
            intent(&r, ReviewArea::Product, ReviewDecision::Comment),
            &factory(&r),
        )
        .await
        .unwrap();
    let history = service
        .history(r.f.product, r.f.proposal, r.snapshot, None)
        .await
        .unwrap();
    assert!(!history.entries[0].effective);
    assert_eq!(history.entries[1].decision, ReviewDecision::Approve);
    assert!(history.entries[1].effective);
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM proposals WHERE id=$1")
            .bind(r.f.proposal)
            .fetch_one(&pool)
            .await
            .unwrap(),
        "IN_REVIEW"
    );
    // The explicit all-changes action remains available to the other qualified reviewer without fabricating unit records.
    let mut all = intent(&r, ReviewArea::Development, ReviewDecision::ApproveAll);
    all.explanation = "Explicitly reviewed the entire saved snapshot".into();
    service
        .submit(r.f.development, r.f.proposal, r.snapshot, all, &factory(&r))
        .await
        .unwrap();
    assert!(
        service
            .history(r.f.development, r.f.proposal, r.snapshot, None)
            .await
            .unwrap()
            .approve_domains
            .is_empty()
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn regular_approval_coverage_is_independent_for_two_domains_of_the_same_reviewer(
    pool: PgPool,
) {
    let r = ready(&pool, true).await;
    let service = PgOfficialReviews::new(pool.clone());
    let page = units(&r, &pool).await;
    for unit in &page {
        view(&r, ReviewArea::Product, unit).await;
        approve_unit(&r, &pool, ReviewArea::Product, &unit.id).await;
    }
    assert_eq!(
        service
            .history(r.f.product, r.f.proposal, r.snapshot, None)
            .await
            .unwrap()
            .approve_domains,
        vec![ReviewArea::Product]
    );
    assert!(matches!(
        service
            .submit(
                r.f.product,
                r.f.proposal,
                r.snapshot,
                intent(&r, ReviewArea::Design, ReviewDecision::Approve),
                &factory(&r)
            )
            .await,
        Err(OfficialReviewError::SectionsIncomplete)
    ));
    for unit in &page {
        view(&r, ReviewArea::Design, unit).await;
        approve_unit(&r, &pool, ReviewArea::Design, &unit.id).await;
    }
    assert_eq!(
        service
            .history(r.f.product, r.f.proposal, r.snapshot, None)
            .await
            .unwrap()
            .approve_domains,
        vec![ReviewArea::Design, ReviewArea::Product]
    );
    let approved = service
        .submit(
            r.f.product,
            r.f.proposal,
            r.snapshot,
            intent(&r, ReviewArea::Design, ReviewDecision::Approve),
            &factory(&r),
        )
        .await
        .unwrap();
    assert_eq!(approved.domain, ReviewArea::Design);
}
#[sqlx::test(migrations = "../../migrations")]
async fn regular_approval_rechecks_membership_after_provider_io_and_discards_a_revoked_reviewer(
    pool: PgPool,
) {
    let r = ready(&pool, false).await;
    for unit in units(&r, &pool).await {
        view(&r, ReviewArea::Product, &unit).await;
        approve_unit(&r, &pool, ReviewArea::Product, &unit.id).await;
    }
    let before = r.source.reads.load(Ordering::SeqCst);
    r.source.blocked.store(true, Ordering::SeqCst);
    let service = PgOfficialReviews::new(pool.clone());
    let input = intent(&r, ReviewArea::Product, ReviewDecision::Approve);
    let f = factory(&r);
    let (actor, proposal, snapshot) = (r.f.product, r.f.proposal, r.snapshot);
    let work =
        tokio::spawn(async move { service.submit(actor, proposal, snapshot, input, &f).await });
    while r.source.reads.load(Ordering::SeqCst) == before {
        r.source.started.notified().await;
    }
    sqlx::query("UPDATE memberships SET review_domains='{}' WHERE user_id=$1")
        .bind(r.f.product)
        .execute(&pool)
        .await
        .unwrap();
    r.source.blocked.store(false, Ordering::SeqCst);
    r.source.release.notify_one();
    assert!(matches!(
        work.await.unwrap(),
        Err(OfficialReviewError::Forbidden)
    ));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM official_reviews")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn database_rejects_regular_approval_without_complete_individual_evidence(pool: PgPool) {
    let r = ready(&pool, false).await;
    let error=sqlx::query("INSERT INTO official_reviews(id,organization_id,proposal_id,snapshot_id,reviewer_id,request_id,request_hash,domain,decision,explanation,fingerprint,head_sha,target_sha,policy_version,configuration_version,audit_id,event_id) SELECT $1,organization_id,proposal_id,id,$2,$3,$4,'PRODUCT','APPROVE','Forged coverage',fingerprint,head_sha,target_sha,policy_version,configuration_version,$5,$6 FROM review_snapshots WHERE id=$7")
        .bind(Uuid::new_v4()).bind(r.f.product).bind(Uuid::new_v4()).bind("0".repeat(64)).bind(Uuid::new_v4()).bind(Uuid::new_v4()).bind(r.snapshot).execute(&pool).await.unwrap_err();
    assert!(
        error
            .as_database_error()
            .unwrap()
            .message()
            .contains("current qualified assignment")
    );
}
