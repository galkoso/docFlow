use super::*;
use serde_json::{Value, json};
use specspace_application::review_units::RequiredReviewUnit;

#[sqlx::test(migrations = "../../migrations")]
async fn publication_seals_exact_required_units_without_approvals_or_source_bodies(pool: PgPool) {
    let (f, _, preparer) = setup_book_options(&pool, false, None, false, None, true).await;
    let (lease, verified) = prepared(&pool, &f, &preparer, 2).await;
    let saved = PgReviewSnapshots::new(pool.clone())
        .publish(&lease, &verified, &preparer)
        .await
        .unwrap();
    let (version, count, ids): (i32, i32, Value) = sqlx::query_as(
        "SELECT review_units_version,unit_count,unit_ids FROM review_snapshots WHERE id=$1",
    )
    .bind(saved.snapshot_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    let rows: Vec<Value> = sqlx::query_scalar(
        "SELECT unit FROM review_snapshot_units WHERE snapshot_id=$1 ORDER BY ordinal",
    )
    .bind(saved.snapshot_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    let decoded: Vec<RequiredReviewUnit> = rows
        .iter()
        .cloned()
        .map(serde_json::from_value)
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(version, 1);
    assert_eq!(count as usize, decoded.len());
    assert_eq!(decoded, verified.candidate().plan().required_units());
    assert_eq!(
        ids,
        json!(decoded.iter().map(|u| &u.id).collect::<Vec<_>>())
    );
    assert!(decoded.iter().all(|u| {
        u.after
            .as_ref()
            .is_some_and(|s| s.source.document_id.as_deref() == Some("DOC-A"))
    }));
    assert!(
        !serde_json::to_string(&rows)
            .unwrap()
            .contains("Saved text-only proposal")
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM official_reviews")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    for statement in [
        "UPDATE review_snapshot_units SET ordinal=ordinal+1",
        "DELETE FROM review_snapshot_units",
        "TRUNCATE review_snapshot_units",
        "INSERT INTO review_snapshot_units SELECT * FROM review_snapshot_units",
        "UPDATE review_snapshots SET unit_count=0,unit_ids='[]'::jsonb",
    ] {
        assert!(sqlx::query(statement).execute(&pool).await.is_err());
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn unit_guards_reject_missing_source_ranges_and_incomplete_requirement_sets(pool: PgPool) {
    let (f, _, preparer) = setup_media(&pool, false).await;
    let (lease, verified) = prepared(&pool, &f, &preparer, 2).await;
    let saved = PgReviewSnapshots::new(pool.clone())
        .publish(&lease, &verified, &preparer)
        .await
        .unwrap();
    let (next, _) = prepared(&pool, &f, &preparer, 3).await;
    let copy = "INSERT INTO review_snapshots SELECT (jsonb_populate_record(NULL::review_snapshots,to_jsonb(h)||jsonb_build_object('id',$2::uuid,'created_operation',$3::uuid,'state','PUBLISHING','prepared_version',3,'metadata',jsonb_set(h.metadata,'{summary,expected_proposal_version}','3'::jsonb)))).* FROM review_snapshots h WHERE id=$1";
    for fault in 0..6 {
        let id = Uuid::new_v4();
        let mut tx = pool.begin().await.unwrap();
        sqlx::query(copy)
            .bind(saved.snapshot_id)
            .bind(id)
            .bind(next.operation_id())
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query("INSERT INTO review_snapshot_files SELECT organization_id,proposal_id,$2,ordinal,file_id,change FROM review_snapshot_files WHERE snapshot_id=$1").bind(saved.snapshot_id).bind(id).execute(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO review_snapshot_books SELECT organization_id,proposal_id,$2,documents FROM review_snapshot_books WHERE snapshot_id=$1").bind(saved.snapshot_id).bind(id).execute(&mut *tx).await.unwrap();
        if fault == 0 {
            assert!(
                sqlx::query("UPDATE review_snapshots SET state='READY' WHERE id=$1")
                    .bind(id)
                    .execute(&mut *tx)
                    .await
                    .is_err()
            );
        } else {
            let (ordinal,unit_id,file_id,mut unit):(i32,String,String,Value)=sqlx::query_as("SELECT ordinal,unit_id,file_id,unit FROM review_snapshot_units WHERE snapshot_id=$1 LIMIT 1").bind(saved.snapshot_id).fetch_one(&mut *tx).await.unwrap();
            match fault {
                1 => unit["after"]["source"]["path"] = json!("docs/forged.md"),
                2 => unit["after"]["range"]["end_byte"] = json!(u32::MAX),
                3 => unit["after"]["range"] = Value::Null,
                4 => unit["requirement"]["document_id"] = json!("DOC-OTHER"),
                _ => {
                    unit["after"]["range"]["start_line"] = json!(u32::MAX);
                    unit["after"]["range"]["end_line"] = json!(u32::MAX);
                }
            }
            assert!(sqlx::query("INSERT INTO review_snapshot_units(organization_id,proposal_id,snapshot_id,ordinal,unit_id,file_id,unit) VALUES($1,$2,$3,$4,$5,$6,$7)")
                .bind(f.org).bind(f.proposal).bind(id).bind(ordinal).bind(unit_id).bind(file_id).bind(unit).execute(&mut *tx).await.is_err());
        }
        tx.rollback().await.unwrap();
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn unit_migration_preserves_legacy_evidence_and_requires_new_complete_publication(
    pool: PgPool,
) {
    let (f, source, preparer) = setup_media(&pool, false).await;
    let (lease, verified) = prepared(&pool, &f, &preparer, 2).await;
    let store = PgReviewSnapshots::new(pool.clone());
    let old = store.publish(&lease, &verified, &preparer).await.unwrap();
    let before:Value=sqlx::query_scalar("SELECT to_jsonb(h)-ARRAY['review_units_version','unit_count','unit_ids'] FROM review_snapshots h WHERE id=$1").bind(old.snapshot_id).fetch_one(&pool).await.unwrap();
    sqlx::raw_sql("DROP TRIGGER review_unit_approval_event_guard ON outbox; DROP TABLE review_unit_approvals; DROP FUNCTION review_unit_approval_guard(),review_unit_approval_complete(),review_unit_approval_event_guard(),review_unit_coverage(uuid,uuid,uuid,uuid,text,text,text); DELETE FROM _sqlx_migrations WHERE version=202611230028;").execute(&pool).await.unwrap();
    sqlx::raw_sql("DROP TRIGGER review_visit_event_guard ON outbox; DROP TABLE review_source_visits,review_source_deliveries,review_source_attempts; DROP FUNCTION review_visit_event_guard(),review_source_visit_complete(),review_source_visit_guard(),review_source_delivery_guard(),review_source_attempt_guard(),review_visit_scope_current(uuid,uuid,uuid,uuid,text,text); DELETE FROM _sqlx_migrations WHERE version IN (202611190026,202611230027);").execute(&pool).await.unwrap();
    sqlx::raw_sql("DROP TRIGGER review_unit_snapshot_guard ON review_snapshots; DROP TABLE review_snapshot_units; DROP FUNCTION review_unit_evidence_guard(); DROP FUNCTION review_unit_snapshot_guard(); ALTER TABLE review_snapshots DROP COLUMN review_units_version,DROP COLUMN unit_count,DROP COLUMN unit_ids; DELETE FROM _sqlx_migrations WHERE version=202611190025;").execute(&pool).await.unwrap();
    specspace_infrastructure::MIGRATOR.run(&pool).await.unwrap();
    let (after,version,count,ids):(Value,i32,i32,Value)=sqlx::query_as("SELECT to_jsonb(h)-ARRAY['review_units_version','unit_count','unit_ids'],review_units_version,unit_count,unit_ids FROM review_snapshots h WHERE id=$1").bind(old.snapshot_id).fetch_one(&pool).await.unwrap();
    assert_eq!(before, after);
    assert_eq!((version, count, ids), (0, 0, json!([])));
    let legacy = store
        .units(f.owner, f.proposal, old.snapshot_id, 0, 100)
        .await
        .unwrap();
    assert!(!legacy.available);
    assert!(legacy.units.is_empty());
    assert_eq!((legacy.total, legacy.next_offset), (0, None));
    assert_eq!(legacy.fingerprint, old.fingerprint);
    let visits = super::visits::store(
        &pool,
        Arc::new(Factory {
            source,
            repository: f.repository,
        }),
    );
    let coverage = visits
        .coverage(
            f.product,
            f.proposal,
            old.snapshot_id,
            specspace_application::review_visits::ReviewCoverageRequest {
                expected_fingerprint: old.fingerprint.clone(),
                domain: specspace_application::review_configuration::ReviewArea::Product,
                offset: 0,
                limit: 20,
            },
        )
        .await
        .unwrap();
    assert!(!coverage.available);
    assert_eq!(
        (
            coverage.total_units,
            coverage.viewed_units,
            coverage.unviewed_units
        ),
        (0, 0, 0)
    );
    assert!(coverage.units.is_empty());
    let warning = specspace_infrastructure::official_reviews::PgOfficialReviews::new(pool.clone())
        .history(f.product, f.proposal, old.snapshot_id, None)
        .await
        .unwrap();
    assert!(warning.unviewed_content_warning);
    assert_eq!(
        warning.unviewed_domains,
        vec![specspace_application::review_configuration::ReviewArea::Product]
    );
    let approvals = specspace_infrastructure::unit_approvals::PgUnitApprovals::new(pool.clone());
    let unknown = approvals
        .page(
            f.product,
            f.proposal,
            old.snapshot_id,
            specspace_application::unit_approval::UnitApprovalPageRequest {
                expected_fingerprint: old.fingerprint.clone(),
                domain: specspace_application::review_configuration::ReviewArea::Product,
                offset: 0,
                limit: 20,
            },
        )
        .await
        .unwrap();
    assert!(!unknown.available);
    assert_eq!(unknown.approved_units, 0);
    assert!(unknown.units.is_empty());
    assert!(matches!(
        approvals
            .approve(
                f.product,
                f.proposal,
                old.snapshot_id,
                specspace_application::unit_approval::UnitApprovalRequest {
                    request_id: Uuid::new_v4(),
                    expected_fingerprint: old.fingerprint.clone(),
                    domain: specspace_application::review_configuration::ReviewArea::Product,
                    unit_id: verified.candidate().plan().required_units()[0].id.clone()
                }
            )
            .await,
        Err(specspace_infrastructure::unit_approvals::UnitApprovalError::EvidenceUnavailable)
    ));

    {
        use specspace_application::comparison_file::{
            FileReadError, review_unit::ReviewUnitAuthorization,
        };
        use specspace_infrastructure::review_files::PgReviewUnitAuthorization;
        assert!(matches!(
            PgReviewUnitAuthorization::new(pool.clone())
                .resolve_unit(
                    f.owner,
                    f.proposal,
                    old.snapshot_id,
                    &verified.candidate().plan().required_units()[0].id
                )
                .await,
            Err(FileReadError::NotReady)
        ));
    }

    assert!(
        sqlx::query("UPDATE review_snapshots SET review_units_version=1 WHERE id=$1")
            .bind(old.snapshot_id)
            .execute(&pool)
            .await
            .is_err()
    );
    let (lease, verified) = prepared(&pool, &f, &preparer, 3).await;
    let new = store.publish(&lease, &verified, &preparer).await.unwrap();
    assert!(new.activated);
    assert_ne!(old.snapshot_id, new.snapshot_id);
    assert!(
        sqlx::query_scalar::<_, i32>("SELECT unit_count FROM review_snapshots WHERE id=$1")
            .bind(new.snapshot_id)
            .fetch_one(&pool)
            .await
            .unwrap()
            > 0
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn requirement_pages_preserve_exact_order_context_and_current_membership(pool: PgPool) {
    let (f, source, preparer) = setup_media(&pool, true).await;
    let (lease, verified) = prepared(&pool, &f, &preparer, 2).await;
    let store = PgReviewSnapshots::new(pool.clone());
    let saved = store.publish(&lease, &verified, &preparer).await.unwrap();
    let viewer = user(&pool, f.org, "READER", vec![]).await;
    let reads = source.reads.load(Ordering::SeqCst);
    let expected = verified.candidate().plan().required_units();
    assert!(expected.len() > 1);
    let first = store
        .units(viewer, f.proposal, saved.snapshot_id, 0, 1)
        .await
        .unwrap();
    assert!(first.available);
    assert_eq!(first.actor_id, viewer);
    assert_eq!(first.proposal_id, f.proposal);
    assert_eq!(first.snapshot_id, saved.snapshot_id);
    assert_eq!(first.fingerprint, saved.fingerprint);
    assert_eq!(first.head_sha, sha(1).as_str());
    assert_eq!(
        (first.offset, first.total, first.next_offset),
        (0, expected.len() as i32, Some(1))
    );
    assert_eq!(first.units, expected[..1]);
    sqlx::query("UPDATE proposals SET head_sha=$2,version=version+1 WHERE id=$1")
        .bind(f.proposal)
        .bind(sha(3).as_str())
        .execute(&pool)
        .await
        .unwrap();
    let rest = store
        .units(viewer, f.proposal, saved.snapshot_id, 1, 100)
        .await
        .unwrap();
    assert_eq!(rest.units, expected[1..]);
    assert_eq!(rest.head_sha, first.head_sha);
    assert_eq!(rest.fingerprint, first.fingerprint);
    assert_eq!(rest.next_offset, None);
    assert!(
        !serde_json::to_string(&rest)
            .unwrap()
            .contains("Saved text-only proposal")
    );
    let end = store
        .units(viewer, f.proposal, saved.snapshot_id, 40000, 100)
        .await
        .unwrap();
    assert!(end.units.is_empty());
    assert_eq!(end.next_offset, None);
    for (offset, limit) in [(-1, 1), (40001, 1), (0, 0), (0, 101)] {
        assert!(matches!(
            store
                .units(viewer, f.proposal, saved.snapshot_id, offset, limit)
                .await,
            Err(SnapshotError::Submission(SubmissionError::InvalidQuery))
        ));
    }
    for (proposal, snapshot) in [
        (Uuid::new_v4(), saved.snapshot_id),
        (f.proposal, Uuid::new_v4()),
    ] {
        assert!(matches!(
            store.units(viewer, proposal, snapshot, 0, 1).await,
            Err(SnapshotError::Submission(SubmissionError::NotFound))
        ));
    }
    let (foreign, _, _) = setup_media(&pool, false).await;
    let outsider = foreign.owner;
    assert!(
        store
            .units(outsider, f.proposal, saved.snapshot_id, 0, 1)
            .await
            .is_err()
    );
    sqlx::query("DELETE FROM memberships WHERE user_id=$1")
        .bind(viewer)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        store
            .units(viewer, f.proposal, saved.snapshot_id, 0, 1)
            .await
            .is_err()
    );
    assert_eq!(source.reads.load(Ordering::SeqCst), reads);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM official_reviews")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn required_unit_authority_reads_sealed_ranges_with_current_repository_binding(pool: PgPool) {
    use specspace_application::comparison_file::{
        ComparisonFileReader, FileReadError,
        discussion::AnchorSide,
        review_unit::{ReviewUnitAuthorization, ReviewUnitReader, UnitWindowRequest},
    };
    use specspace_infrastructure::review_files::{
        PgReviewFileAuthorization, PgReviewUnitAuthorization,
    };
    let (f, source, preparer) = setup_media(&pool, false).await;
    let (lease, verified) = prepared(&pool, &f, &preparer, 2).await;
    let saved = PgReviewSnapshots::new(pool.clone())
        .publish(&lease, &verified, &preparer)
        .await
        .unwrap();
    let viewer = user(&pool, f.org, "READER", vec![]).await;
    let auth = Arc::new(PgReviewUnitAuthorization::new(pool.clone()));
    let unit = &verified.candidate().plan().required_units()[0];
    let context = auth
        .resolve_unit(viewer, f.proposal, saved.snapshot_id, &unit.id)
        .await
        .unwrap();
    assert_eq!(context.unit, *unit);
    assert_eq!(context.fingerprint, saved.fingerprint);
    assert_eq!(context.scope.actor, viewer);
    assert_eq!(context.head, sha(1));
    assert_eq!(context.merge_base, sha(0));
    let files = Arc::new(
        ComparisonFileReader::new(
            Arc::new(PgReviewFileAuthorization::new(pool.clone())),
            Arc::new(Factory {
                source: source.clone(),
                repository: f.repository,
            }),
            Duration::from_secs(60),
        )
        .unwrap(),
    );
    let reader = ReviewUnitReader::new(files, auth.clone());
    let proof = reader
        .load(
            viewer,
            f.proposal,
            saved.snapshot_id,
            UnitWindowRequest {
                unit: &unit.id,
                side: AnchorSide::After,
                offset: 0,
            },
        )
        .await
        .unwrap();
    assert!(proof.text().contains("Saved text-only proposal"));
    assert_eq!(proof.source(), unit.after.as_ref().unwrap());
    sqlx::query("UPDATE proposals SET head_sha=$2,version=version+1 WHERE id=$1")
        .bind(f.proposal)
        .bind(sha(3).as_str())
        .execute(&pool)
        .await
        .unwrap();
    assert!(reader.recheck(viewer, &proof).await.is_err());
    let next = auth
        .resolve_unit(viewer, f.proposal, saved.snapshot_id, &unit.id)
        .await
        .unwrap();
    assert_eq!(next.head, context.head);
    assert_eq!(next.fingerprint, context.fingerprint);
    assert!(matches!(
        auth.resolve_unit(viewer, f.proposal, saved.snapshot_id, &"f".repeat(64))
            .await,
        Err(FileReadError::NotFound)
    ));
    assert!(
        auth.resolve_unit(viewer, Uuid::new_v4(), saved.snapshot_id, &unit.id)
            .await
            .is_err()
    );
    assert!(
        auth.resolve_unit(viewer, f.proposal, Uuid::new_v4(), &unit.id)
            .await
            .is_err()
    );
    let (foreign, _, _) = setup_media(&pool, false).await;
    assert!(
        auth.resolve_unit(foreign.owner, f.proposal, saved.snapshot_id, &unit.id)
            .await
            .is_err()
    );
    sqlx::query("UPDATE repositories SET remote_id='999999' WHERE id=$1")
        .bind(f.repository)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        auth.resolve_unit(viewer, f.proposal, saved.snapshot_id, &unit.id)
            .await
            .is_err()
    );
    sqlx::query("DELETE FROM memberships WHERE user_id=$1")
        .bind(viewer)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        auth.resolve_unit(viewer, f.proposal, saved.snapshot_id, &unit.id)
            .await
            .is_err()
    );
}
