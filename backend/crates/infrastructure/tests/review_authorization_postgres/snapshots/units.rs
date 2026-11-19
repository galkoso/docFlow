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
    let (f, _, preparer) = setup_media(&pool, false).await;
    let (lease, verified) = prepared(&pool, &f, &preparer, 2).await;
    let store = PgReviewSnapshots::new(pool.clone());
    let old = store.publish(&lease, &verified, &preparer).await.unwrap();
    let before:Value=sqlx::query_scalar("SELECT to_jsonb(h)-ARRAY['review_units_version','unit_count','unit_ids'] FROM review_snapshots h WHERE id=$1").bind(old.snapshot_id).fetch_one(&pool).await.unwrap();
    sqlx::raw_sql("DROP TRIGGER review_unit_snapshot_guard ON review_snapshots; DROP TABLE review_snapshot_units; DROP FUNCTION review_unit_evidence_guard(); DROP FUNCTION review_unit_snapshot_guard(); ALTER TABLE review_snapshots DROP COLUMN review_units_version,DROP COLUMN unit_count,DROP COLUMN unit_ids; DELETE FROM _sqlx_migrations WHERE version=202611190025;").execute(&pool).await.unwrap();
    specspace_infrastructure::MIGRATOR.run(&pool).await.unwrap();
    let (after,version,count,ids):(Value,i32,i32,Value)=sqlx::query_as("SELECT to_jsonb(h)-ARRAY['review_units_version','unit_count','unit_ids'],review_units_version,unit_count,unit_ids FROM review_snapshots h WHERE id=$1").bind(old.snapshot_id).fetch_one(&pool).await.unwrap();
    assert_eq!(before, after);
    assert_eq!((version, count, ids), (0, 0, json!([])));
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
