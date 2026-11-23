use super::coverage::custom;
use super::*;
use serde_json::json;
use specspace_application::unit_approval::*;
use specspace_infrastructure::unit_approvals::{PgUnitApprovals, UnitApprovalError as Error};

fn request(r: &Ready) -> UnitApprovalRequest {
    UnitApprovalRequest {
        request_id: Uuid::new_v4(),
        expected_fingerprint: r.request.expected_fingerprint.clone(),
        domain: ReviewArea::Product,
        unit_id: r.request.unit_id.clone(),
    }
}
fn page_request(r: &Ready) -> UnitApprovalPageRequest {
    UnitApprovalPageRequest {
        expected_fingerprint: r.request.expected_fingerprint.clone(),
        domain: ReviewArea::Product,
        offset: 0,
        limit: 20,
    }
}
async fn complete(r: &Ready) {
    for side in [AnchorSide::Before, AnchorSide::After] {
        let mut input = r.request.clone();
        input.side = side;
        let window = r
            .store
            .deliver(r.f.product, r.f.proposal, r.snapshot, input)
            .await
            .unwrap();
        r.store
            .acknowledge(r.f.product, r.f.proposal, r.snapshot, ack(&window))
            .await
            .unwrap();
    }
}
async fn approval_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM review_unit_approvals")
        .fetch_one(pool)
        .await
        .unwrap()
}
#[sqlx::test(migrations = "../../migrations")]
async fn a_new_stable_section_can_be_approved_after_its_only_present_side_is_acknowledged(
    pool: PgPool,
) {
    let mut r = custom(
        &pool,
        Some("Changed\n\n<!-- ss:section:SEC-NEW -->\n## New\n\nAdded source"),
        false,
        false,
    )
    .await;
    let units = PgReviewSnapshots::new(pool.clone())
        .units(r.f.product, r.f.proposal, r.snapshot, 0, 100)
        .await
        .unwrap();
    let added = units.units.iter().find(|u|matches!(&u.requirement,specspace_application::review_units::Requirement::Section {section_id,..} if section_id=="SEC-NEW")).unwrap();
    assert!(added.before.is_none());
    r.request.unit_id = added.id.clone();
    let w = deliver(&r).await.unwrap();
    r.store
        .acknowledge(r.f.product, r.f.proposal, r.snapshot, ack(&w))
        .await
        .unwrap();
    let store = PgUnitApprovals::new(pool.clone());
    let saved = store
        .approve(r.f.product, r.f.proposal, r.snapshot, request(&r))
        .await
        .unwrap();
    let page = store
        .page(r.f.product, r.f.proposal, r.snapshot, page_request(&r))
        .await
        .unwrap();
    assert!(page.total_units > 1);
    assert_eq!(page.approved_units, 1);
    assert_eq!(page.unapproved_units, page.total_units - 1);
    assert_eq!(
        page.units
            .iter()
            .find(|u| u.unit_id == added.id)
            .unwrap()
            .approval_id,
        Some(saved.id)
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn a_gap_between_acknowledged_windows_prevents_individual_approval(pool: PgPool) {
    let r = custom(&pool, Some(&"x".repeat(131100)), false, false).await;
    let store = PgUnitApprovals::new(pool.clone());
    for (side, offset) in [
        (AnchorSide::After, 0),
        (AnchorSide::After, 131072),
        (AnchorSide::Before, 0),
    ] {
        let mut input = r.request.clone();
        input.side = side;
        input.offset = offset;
        let w = r
            .store
            .deliver(r.f.product, r.f.proposal, r.snapshot, input)
            .await
            .unwrap();
        r.store
            .acknowledge(r.f.product, r.f.proposal, r.snapshot, ack(&w))
            .await
            .unwrap();
    }
    assert!(matches!(
        store
            .approve(r.f.product, r.f.proposal, r.snapshot, request(&r))
            .await,
        Err(Error::IncompleteCoverage)
    ));
    let mut input = r.request.clone();
    input.offset = 65536;
    let w = r
        .store
        .deliver(r.f.product, r.f.proposal, r.snapshot, input)
        .await
        .unwrap();
    r.store
        .acknowledge(r.f.product, r.f.proposal, r.snapshot, ack(&w))
        .await
        .unwrap();
    assert!(
        store
            .approve(r.f.product, r.f.proposal, r.snapshot, request(&r))
            .await
            .is_ok()
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn successful_approval_quota_does_not_prevent_exact_replay_or_inflate_unit_counts(
    pool: PgPool,
) {
    let r = ready(&pool).await;
    complete(&r).await;
    let store = PgUnitApprovals::new(pool.clone());
    let first = request(&r);
    let saved = store
        .approve(r.f.product, r.f.proposal, r.snapshot, first.clone())
        .await
        .unwrap();
    for _ in 1..600 {
        store
            .approve(r.f.product, r.f.proposal, r.snapshot, request(&r))
            .await
            .unwrap();
    }
    assert!(matches!(
        store
            .approve(r.f.product, r.f.proposal, r.snapshot, request(&r))
            .await,
        Err(Error::Quota)
    ));
    let replay = store
        .approve(r.f.product, r.f.proposal, r.snapshot, first)
        .await
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(saved.id, replay.id);
    let page = store
        .page(r.f.product, r.f.proposal, r.snapshot, page_request(&r))
        .await
        .unwrap();
    assert_eq!(page.approved_units, 1);
    assert_eq!(approval_count(&pool).await, 600);
}
#[sqlx::test(migrations = "../../migrations")]
async fn individual_approval_requires_every_present_side_and_never_changes_official_decisions_or_refs(
    pool: PgPool,
) {
    let r = ready(&pool).await;
    let store = PgUnitApprovals::new(pool.clone());
    let intent = request(&r);
    let before: Value = sqlx::query_scalar("SELECT to_jsonb(p) FROM proposals p WHERE id=$1")
        .bind(r.f.proposal)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(matches!(
        store
            .approve(r.f.product, r.f.proposal, r.snapshot, intent.clone())
            .await,
        Err(Error::IncompleteCoverage)
    ));
    let w = deliver(&r).await.unwrap();
    r.store
        .acknowledge(r.f.product, r.f.proposal, r.snapshot, ack(&w))
        .await
        .unwrap();
    assert!(matches!(
        store
            .approve(r.f.product, r.f.proposal, r.snapshot, intent.clone())
            .await,
        Err(Error::IncompleteCoverage)
    ));
    let mut input = r.request.clone();
    input.side = AnchorSide::Before;
    let w = r
        .store
        .deliver(r.f.product, r.f.proposal, r.snapshot, input)
        .await
        .unwrap();
    r.store
        .acknowledge(r.f.product, r.f.proposal, r.snapshot, ack(&w))
        .await
        .unwrap();
    assert_eq!(approval_count(&pool).await, 0);
    let reads = r.source.reads.load(Ordering::SeqCst);
    let saved = store
        .approve(r.f.product, r.f.proposal, r.snapshot, intent.clone())
        .await
        .unwrap();
    assert!(!saved.replayed);
    assert_eq!(saved.unit_id, intent.unit_id);
    assert_eq!(saved.fingerprint, intent.expected_fingerprint);
    assert_eq!(saved.head_sha, sha(1).as_str());
    assert_eq!(saved.target_sha, sha(2).as_str());
    let page = store
        .page(r.f.product, r.f.proposal, r.snapshot, page_request(&r))
        .await
        .unwrap();
    assert!(page.current_for_metadata && page.available);
    assert_eq!(
        (page.total_units, page.approved_units, page.unapproved_units),
        (1, 1, 0)
    );
    assert_eq!(page.units[0].approval_id, Some(saved.id));
    assert!(page.units[0].approved_at.is_some());
    assert_eq!(r.source.reads.load(Ordering::SeqCst), reads);
    let after: Value = sqlx::query_scalar("SELECT to_jsonb(p) FROM proposals p WHERE id=$1")
        .bind(r.f.proposal)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(before, after);
    assert_eq!(counts(&pool).await.3, 0);
    let (audit,event):(i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM audit_events WHERE action='REVIEW_UNIT_APPROVED'),(SELECT count(*) FROM outbox WHERE event_type='REVIEW_UNIT_APPROVED')").fetch_one(&pool).await.unwrap();
    assert_eq!((audit, event), (1, 1));
}
#[sqlx::test(migrations = "../../migrations")]
async fn concurrent_exact_intents_replay_one_receipt_and_changed_keys_cannot_retarget_it(
    pool: PgPool,
) {
    let r = ready(&pool).await;
    complete(&r).await;
    let store = PgUnitApprovals::new(pool.clone());
    let intent = request(&r);
    let (a, b) = tokio::join!(
        store.approve(r.f.product, r.f.proposal, r.snapshot, intent.clone()),
        store.approve(r.f.product, r.f.proposal, r.snapshot, intent.clone())
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(a.id, b.id);
    assert_ne!(a.replayed, b.replayed);
    assert_eq!(approval_count(&pool).await, 1);
    let mut changed = intent.clone();
    changed.unit_id = "0".repeat(64);
    assert!(matches!(
        store
            .approve(r.f.product, r.f.proposal, r.snapshot, changed)
            .await,
        Err(Error::Conflict)
    ));
    let mut changed = intent.clone();
    changed.expected_fingerprint = "0".repeat(64);
    assert!(matches!(
        store
            .approve(r.f.product, r.f.proposal, r.snapshot, changed)
            .await,
        Err(Error::Conflict)
    ));
    // A separate explicit action is history, while summary coverage still counts one unit.
    let again = store
        .approve(r.f.product, r.f.proposal, r.snapshot, request(&r))
        .await
        .unwrap();
    assert_ne!(again.id, a.id);
    assert_eq!(
        store
            .page(r.f.product, r.f.proposal, r.snapshot, page_request(&r))
            .await
            .unwrap()
            .approved_units,
        1
    );
    assert_eq!(approval_count(&pool).await, 2);
}
#[sqlx::test(migrations = "../../migrations")]
async fn approvals_are_actor_domain_bound_and_revocation_denies_even_exact_replay(pool: PgPool) {
    let r = custom(&pool, None, false, true).await;
    complete(&r).await;
    let store = PgUnitApprovals::new(pool.clone());
    let intent = request(&r);
    store
        .approve(r.f.product, r.f.proposal, r.snapshot, intent.clone())
        .await
        .unwrap();
    let mut design = request(&r);
    design.domain = ReviewArea::Design;
    assert!(matches!(
        store
            .approve(r.f.product, r.f.proposal, r.snapshot, design)
            .await,
        Err(Error::IncompleteCoverage)
    ));
    let mut input = page_request(&r);
    input.domain = ReviewArea::Design;
    assert_eq!(
        store
            .page(r.f.product, r.f.proposal, r.snapshot, input)
            .await
            .unwrap()
            .approved_units,
        0
    );
    let mut input = page_request(&r);
    input.domain = ReviewArea::Development;
    assert_eq!(
        store
            .page(r.f.development, r.f.proposal, r.snapshot, input)
            .await
            .unwrap()
            .approved_units,
        0
    );
    assert!(
        store
            .approve(r.f.owner, r.f.proposal, r.snapshot, request(&r))
            .await
            .is_err()
    );
    assert!(
        store
            .page(r.f.owner, r.f.proposal, r.snapshot, page_request(&r))
            .await
            .is_err()
    );
    sqlx::query("UPDATE memberships SET review_domains='{}' WHERE user_id=$1")
        .bind(r.f.product)
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        store
            .approve(r.f.product, r.f.proposal, r.snapshot, intent)
            .await,
        Err(Error::Authority(OfficialReviewError::Forbidden))
    ));
    assert_eq!(approval_count(&pool).await, 1);
}
#[sqlx::test(migrations = "../../migrations")]
async fn historical_approval_stays_pinned_and_new_policy_snapshot_does_not_inherit_identical_unit_ids(
    pool: PgPool,
) {
    let r = ready(&pool).await;
    complete(&r).await;
    let store = PgUnitApprovals::new(pool.clone());
    let intent = request(&r);
    let saved = store
        .approve(r.f.product, r.f.proposal, r.snapshot, intent.clone())
        .await
        .unwrap();
    sqlx::query("UPDATE organizations SET policy_version=2 WHERE id=$1")
        .bind(r.f.org)
        .execute(&pool)
        .await
        .unwrap();
    let (version, configuration): (i64, i64) =
        sqlx::query_as("SELECT version,review_configuration_version FROM proposals WHERE id=$1")
            .bind(r.f.proposal)
            .fetch_one(&pool)
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
    let (new, fp, unit) =
        super::coverage::publish_context(&pool, &r.f, &preparer, version, configuration, 2).await;
    assert_eq!(unit, intent.unit_id);
    let old = store
        .page(r.f.product, r.f.proposal, r.snapshot, page_request(&r))
        .await
        .unwrap();
    assert!(!old.current_for_metadata);
    assert_eq!(old.approved_units, 1);
    let replay = store
        .approve(r.f.product, r.f.proposal, r.snapshot, intent)
        .await
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.id, saved.id);
    assert!(matches!(
        store
            .approve(r.f.product, r.f.proposal, r.snapshot, request(&r))
            .await,
        Err(Error::Authority(OfficialReviewError::Stale))
    ));
    let page = store
        .page(
            r.f.product,
            r.f.proposal,
            new,
            UnitApprovalPageRequest {
                expected_fingerprint: fp,
                domain: ReviewArea::Product,
                offset: 0,
                limit: 20,
            },
        )
        .await
        .unwrap();
    assert!(page.current_for_metadata);
    assert_eq!(page.approved_units, 0);
    assert_eq!(page.units[0].approval_id, None);
}
#[sqlx::test(migrations = "../../migrations")]
async fn unsupported_items_and_missing_or_malformed_intents_never_create_approvals(pool: PgPool) {
    let r = custom(&pool, None, true, false).await;
    let store = PgUnitApprovals::new(pool.clone());
    let units = PgReviewSnapshots::new(pool.clone())
        .units(r.f.product, r.f.proposal, r.snapshot, 0, 100)
        .await
        .unwrap();
    let media = units
        .units
        .iter()
        .find(|u| u.after.as_ref().is_some_and(|s| s.range.is_none()))
        .unwrap();
    let mut intent = request(&r);
    intent.unit_id = media.id.clone();
    assert!(matches!(
        store
            .approve(r.f.product, r.f.proposal, r.snapshot, intent)
            .await,
        Err(Error::IncompleteCoverage)
    ));
    let mut intent = request(&r);
    intent.unit_id = "f".repeat(64);
    assert!(matches!(
        store
            .approve(r.f.product, r.f.proposal, r.snapshot, intent)
            .await,
        Err(Error::NotFound)
    ));
    let mut intent = request(&r);
    intent.request_id = Uuid::nil();
    assert!(matches!(
        store
            .approve(r.f.product, r.f.proposal, r.snapshot, intent)
            .await,
        Err(Error::Invalid)
    ));
    for (offset, limit) in [(-1, 20), (40001, 20), (0, 0), (0, 101)] {
        let mut input = page_request(&r);
        input.offset = offset;
        input.limit = limit;
        assert!(matches!(
            store
                .page(r.f.product, r.f.proposal, r.snapshot, input)
                .await,
            Err(Error::Invalid)
        ));
    }
    let mut input = page_request(&r);
    input.limit = 1;
    let first = store
        .page(r.f.product, r.f.proposal, r.snapshot, input)
        .await
        .unwrap();
    assert!(first.total_units > 1);
    assert_eq!(first.next_offset, Some(1));
    let mut input = page_request(&r);
    input.offset = 40000;
    let last = store
        .page(r.f.product, r.f.proposal, r.snapshot, input)
        .await
        .unwrap();
    assert!(last.units.is_empty());
    assert!(last.next_offset.is_none());
    assert_eq!(last.total_units, first.total_units);
    assert_eq!(approval_count(&pool).await, 0);
}
async fn direct(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    r: &Ready,
    ids: Option<(Uuid, Uuid, Uuid)>,
) -> Result<(), sqlx::Error> {
    let (id, audit, event) =
        ids.unwrap_or_else(|| (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()));
    sqlx::query("INSERT INTO review_unit_approvals(id,organization_id,proposal_id,snapshot_id,reviewer_id,domain,fingerprint,unit_id,request_id,request_hash,audit_id,event_id) VALUES($1,$2,$3,$4,$5,'PRODUCT',$6,$7,$8,$9,$10,$11)")
        .bind(id).bind(r.f.org).bind(r.f.proposal).bind(r.snapshot).bind(r.f.product).bind(&r.request.expected_fingerprint).bind(&r.request.unit_id)
        .bind(Uuid::new_v4()).bind("0".repeat(64)).bind(audit).bind(event).execute(&mut **tx).await?;
    Ok(())
}
#[sqlx::test(migrations = "../../migrations")]
async fn database_guards_require_complete_source_and_atomic_audit_event_and_keep_receipts_immutable(
    pool: PgPool,
) {
    let r = ready(&pool).await;
    let mut tx = pool.begin().await.unwrap();
    let denied = direct(&mut tx, &r, None).await.unwrap_err();
    assert!(
        denied
            .as_database_error()
            .unwrap()
            .message()
            .contains("complete acknowledged source")
    );
    tx.rollback().await.unwrap();
    complete(&r).await;
    let mut tx = pool.begin().await.unwrap();
    direct(&mut tx, &r, None).await.unwrap();
    assert!(tx.commit().await.is_err());
    for fault in ["audit", "event"] {
        let (id, audit, event) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let mut tx = pool.begin().await.unwrap();
        sqlx::query("INSERT INTO audit_events(id,organization_id,actor_id,entity_id,action,after_ref,result) VALUES($1,$2,$3,$4,'REVIEW_UNIT_APPROVED',$5,'RECORDED')")
            .bind(audit).bind(r.f.org).bind(r.f.product).bind(r.f.proposal).bind(if fault=="audit" {Uuid::nil().to_string()} else {id.to_string()}).execute(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO outbox(id,organization_id,entity_id,event_type,payload) VALUES($1,$2,$3,'REVIEW_UNIT_APPROVED',$4)")
            .bind(event).bind(r.f.org).bind(r.f.proposal).bind(json!({"approval_id":id,"snapshot_id":r.snapshot,"unit_id":r.request.unit_id,"domain":if fault=="event" {"DESIGN"} else {"PRODUCT"}})).execute(&mut *tx).await.unwrap();
        direct(&mut tx, &r, Some((id, audit, event))).await.unwrap();
        let failure = tx.commit().await.unwrap_err();
        assert!(
            failure
                .as_database_error()
                .unwrap()
                .message()
                .contains("atomic audit and event")
        );
    }
    assert_eq!(approval_count(&pool).await, 0);
    let store = PgUnitApprovals::new(pool.clone());
    let saved = store
        .approve(r.f.product, r.f.proposal, r.snapshot, request(&r))
        .await
        .unwrap();
    for statement in [
        "UPDATE review_unit_approvals SET unit_id=unit_id WHERE id=$1",
        "DELETE FROM review_unit_approvals WHERE id=$1",
    ] {
        assert!(
            sqlx::query(statement)
                .bind(saved.id)
                .execute(&pool)
                .await
                .is_err()
        );
    }
    assert!(
        sqlx::query("TRUNCATE review_unit_approvals")
            .execute(&pool)
            .await
            .is_err()
    );
    for statement in [
        "UPDATE outbox SET payload='{}' WHERE id=(SELECT event_id FROM review_unit_approvals WHERE id=$1)",
        "DELETE FROM outbox WHERE id=(SELECT event_id FROM review_unit_approvals WHERE id=$1)",
    ] {
        assert!(
            sqlx::query(statement)
                .bind(saved.id)
                .execute(&pool)
                .await
                .is_err()
        );
    }
    sqlx::query("UPDATE outbox SET sent_at=clock_timestamp() WHERE id=(SELECT event_id FROM review_unit_approvals WHERE id=$1)").bind(saved.id).execute(&pool).await.unwrap();
}
