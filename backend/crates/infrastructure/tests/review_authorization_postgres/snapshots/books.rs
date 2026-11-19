use super::*;
use serde_json::{Value, json};
use specspace_application::comparison_file::{
    ComparisonFileReader, FileReadError, book::ReviewBookAuthorization, discussion::TextSelection,
};
use specspace_infrastructure::review_files::{
    PgReviewBookAuthorization, PgReviewFileAuthorization,
};

#[sqlx::test(migrations = "../../migrations")]
async fn complete_document_pages_keep_catalog_order_immutable_context_and_current_access(
    pool: PgPool,
) {
    let (f, source, preparer) = setup_book_options(&pool, false, None, false, None, true).await;
    let (lease, verified) = prepared(&pool, &f, &preparer, 2).await;
    let store = PgReviewSnapshots::new(pool.clone());
    let saved = store.publish(&lease, &verified, &preparer).await.unwrap();
    let viewer = user(&pool, f.org, "READER", vec![]).await;
    let reads = source.reads.load(Ordering::SeqCst);
    let first = store
        .documents(viewer, f.proposal, saved.snapshot_id, 0, 1)
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
        (0, 2, Some(1))
    );
    assert_eq!(
        first.documents[0].source.document_id.as_deref(),
        Some("DOC-B")
    );
    sqlx::query("UPDATE proposals SET head_sha=$2,version=version+1 WHERE id=$1")
        .bind(f.proposal)
        .bind(sha(3).as_str())
        .execute(&pool)
        .await
        .unwrap();
    let second = store
        .documents(viewer, f.proposal, saved.snapshot_id, 1, 1)
        .await
        .unwrap();
    assert_eq!(second.head_sha, first.head_sha);
    assert_eq!(second.fingerprint, first.fingerprint);
    assert_eq!(
        second.documents[0].source.document_id.as_deref(),
        Some("DOC-A")
    );
    assert_eq!(second.next_offset, None);
    let end = store
        .documents(viewer, f.proposal, saved.snapshot_id, 1000, 100)
        .await
        .unwrap();
    assert!(end.documents.is_empty());
    assert_eq!(end.next_offset, None);
    for (offset, limit) in [(-1, 1), (1001, 1), (0, 0), (0, 101)] {
        assert!(
            store
                .documents(viewer, f.proposal, saved.snapshot_id, offset, limit)
                .await
                .is_err()
        );
    }
    assert!(
        store
            .documents(viewer, Uuid::new_v4(), saved.snapshot_id, 0, 1)
            .await
            .is_err()
    );
    assert!(
        store
            .documents(viewer, f.proposal, Uuid::new_v4(), 0, 1)
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
            .documents(viewer, f.proposal, saved.snapshot_id, 0, 1)
            .await
            .is_err()
    );
    assert_eq!(source.reads.load(Ordering::SeqCst), reads);
}

#[sqlx::test(migrations = "../../migrations")]
async fn unchanged_document_source_proofs_are_scoped_to_the_sealed_book_not_the_diff(pool: PgPool) {
    let (f, source, preparer) = setup_book_options(&pool, false, None, false, None, true).await;
    let (lease, verified) = prepared(&pool, &f, &preparer, 2).await;
    let store = PgReviewSnapshots::new(pool.clone());
    let publication = store.publish(&lease, &verified, &preparer).await.unwrap();
    let viewer = user(&pool, f.org, "READER", vec![]).await;
    let documents = store
        .book_documents(viewer, f.proposal, publication.snapshot_id)
        .await
        .unwrap()
        .unwrap();
    let entry = &documents[0];
    assert_eq!(entry.source.document_id.as_deref(), Some("DOC-B"));
    let authorization = Arc::new(PgReviewBookAuthorization::new(pool.clone()));
    let base = ComparisonFileReader::new(
        Arc::new(PgReviewFileAuthorization::new(pool.clone())),
        Arc::new(Factory {
            source: source.clone(),
            repository: f.repository,
        }),
        Duration::from_secs(30),
    )
    .unwrap();
    let reader = base.with_book_authorization(authorization.clone());
    let reads = source.reads.load(Ordering::SeqCst);
    let context = authorization
        .resolve_document(viewer, f.proposal, publication.snapshot_id, &entry.id)
        .await
        .unwrap();
    assert_eq!(context.head, sha(1));
    assert_eq!(context.fingerprint, publication.fingerprint);
    assert_eq!(context.scope.version, 3);
    assert_eq!(context.document, *entry);
    assert_eq!(source.reads.load(Ordering::SeqCst), reads);
    let proof = reader
        .load_anchor(
            viewer,
            f.proposal,
            publication.snapshot_id,
            &entry.id,
            TextSelection::Section {
                section_id: "SEC-B".into(),
            },
        )
        .await
        .unwrap();
    assert!(proof.text().quote().contains("Unchanged original body"));
    assert_eq!(proof.commit(), &sha(1));
    assert_eq!(proof.source().path, "docs/b.md");
    assert_eq!(
        proof.text().source_sha256(),
        entry.source.sha256.as_deref().unwrap()
    );
    assert_eq!(source.reads.load(Ordering::SeqCst), reads + 1);
    // Book entry IDs do not become read authority in the diff namespace.
    assert!(
        base.load(viewer, f.proposal, publication.snapshot_id, &entry.id)
            .await
            .is_err()
    );
    for (proposal, snapshot, entry_id) in [
        (Uuid::new_v4(), publication.snapshot_id, entry.id.as_str()),
        (f.proposal, Uuid::new_v4(), entry.id.as_str()),
        (
            f.proposal,
            publication.snapshot_id,
            "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
        ),
    ] {
        assert!(
            authorization
                .resolve_document(viewer, proposal, snapshot, entry_id)
                .await
                .is_err()
        );
    }
    sqlx::query("UPDATE proposals SET head_sha=$2,version=version+1 WHERE id=$1")
        .bind(f.proposal)
        .bind(sha(3).as_str())
        .execute(&pool)
        .await
        .unwrap();
    assert!(reader.recheck(viewer, &proof).await.is_err());
    let historical = reader
        .load_anchor(
            viewer,
            f.proposal,
            publication.snapshot_id,
            &entry.id,
            TextSelection::Document {},
        )
        .await
        .unwrap();
    assert_eq!(historical.commit(), &sha(1));
    assert_eq!(historical.context().scope.recorded_head, sha(3));
    sqlx::query("DELETE FROM memberships WHERE user_id=$1")
        .bind(viewer)
        .execute(&pool)
        .await
        .unwrap();
    let reads = source.reads.load(Ordering::SeqCst);
    assert!(reader.recheck(viewer, &historical).await.is_err());
    assert!(
        reader
            .load_anchor(
                viewer,
                f.proposal,
                publication.snapshot_id,
                &entry.id,
                TextSelection::Document {}
            )
            .await
            .is_err()
    );
    assert_eq!(source.reads.load(Ordering::SeqCst), reads);
}

#[sqlx::test(migrations = "../../migrations")]
async fn changed_repository_bindings_cannot_authorize_original_book_source(pool: PgPool) {
    let (f, source, preparer) = setup_media(&pool, false).await;
    let (lease, verified) = prepared(&pool, &f, &preparer, 2).await;
    let store = PgReviewSnapshots::new(pool.clone());
    let published = store.publish(&lease, &verified, &preparer).await.unwrap();
    let entry = store
        .book_documents(f.owner, f.proposal, published.snapshot_id)
        .await
        .unwrap()
        .unwrap()
        .remove(0);
    let authorization = PgReviewBookAuthorization::new(pool.clone());
    sqlx::query("UPDATE repositories SET remote_id='43' WHERE id=$1")
        .bind(f.repository)
        .execute(&pool)
        .await
        .unwrap();
    let reads = source.reads.load(Ordering::SeqCst);
    assert!(matches!(
        authorization
            .resolve_document(f.owner, f.proposal, published.snapshot_id, &entry.id)
            .await,
        Err(FileReadError::Access(ComparisonReadError::ContextChanged))
    ));
    assert_eq!(source.reads.load(Ordering::SeqCst), reads);
}

#[sqlx::test(migrations = "../../migrations")]
async fn migration_preserves_legacy_snapshots_without_fabricating_complete_book_evidence(
    pool: PgPool,
) {
    let (f, _, preparer) = setup_media(&pool, false).await;
    let (lease, verified) = prepared(&pool, &f, &preparer, 2).await;
    let store = PgReviewSnapshots::new(pool.clone());
    let original = store.publish(&lease, &verified, &preparer).await.unwrap();
    let before: Value = sqlx::query_scalar(
        "SELECT to_jsonb(h)-'book_manifest_version' FROM review_snapshots h WHERE id=$1",
    )
    .bind(original.snapshot_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    // Remove only this migration's schema in this isolated test database to
    // recreate an already-published pre-023 snapshot. Older guards stay active.
    sqlx::raw_sql("DROP TRIGGER review_book_snapshot_guard ON review_snapshots; DROP TABLE review_snapshot_books; DROP FUNCTION review_book_manifest_guard(); DROP FUNCTION review_book_snapshot_guard(); ALTER TABLE review_snapshots DROP COLUMN book_manifest_version; DELETE FROM _sqlx_migrations WHERE version=202611190023;")
        .execute(&pool).await.unwrap();
    specspace_infrastructure::MIGRATOR.run(&pool).await.unwrap();
    let (after, version): (Value, i32) = sqlx::query_as("SELECT to_jsonb(h)-'book_manifest_version',book_manifest_version FROM review_snapshots h WHERE id=$1")
        .bind(original.snapshot_id).fetch_one(&pool).await.unwrap();
    assert_eq!(before, after);
    assert_eq!(version, 0);
    let unavailable = store
        .documents(f.owner, f.proposal, original.snapshot_id, 0, 20)
        .await
        .unwrap();
    assert!(!unavailable.available);
    assert_eq!(unavailable.total, 0);
    assert!(unavailable.documents.is_empty());
    assert_eq!(unavailable.next_offset, None);
    let authorization = PgReviewBookAuthorization::new(pool.clone());
    let entry = &verified.candidate().plan().book_documents()[0];
    assert!(matches!(
        authorization
            .resolve_document(f.owner, f.proposal, original.snapshot_id, &entry.id)
            .await,
        Err(FileReadError::NotReady)
    ));
    let legacy_book_request = specspace_application::discussion::OpenThreadRequest {
        request_id: Uuid::new_v4(),
        origin: specspace_application::discussion::ThreadOrigin::Snapshot {
            snapshot_id: original.snapshot_id,
            expected_fingerprint: original.fingerprint.clone(),
            anchor: specspace_application::discussion::SnapshotAnchor::BookDocument {
                entry_id: entry.id.clone(),
                selection: TextSelection::Document {},
            },
        },
        blocking: false,
        body: "Legacy snapshot cannot supply complete-book proof".into(),
    };
    assert!(matches!(
        specspace_infrastructure::discussions::PgDiscussions::new(pool.clone())
            .admit_open(f.owner, f.proposal, legacy_book_request)
            .await,
        Err(specspace_infrastructure::discussions::DiscussionStoreError::NotReady)
    ));
    assert!(
        store
            .book_documents(f.owner, f.proposal, original.snapshot_id)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store
            .files(f.owner, f.proposal, original.snapshot_id, 0, 100)
            .await
            .unwrap()
            .total,
        1
    );
    assert!(
        sqlx::query("UPDATE review_snapshots SET book_manifest_version=1 WHERE id=$1")
            .bind(original.snapshot_id)
            .execute(&pool)
            .await
            .is_err()
    );
    let (lease, verified) = prepared(&pool, &f, &preparer, 3).await;
    let next = store.publish(&lease, &verified, &preparer).await.unwrap();
    assert!(next.activated);
    assert_ne!(next.snapshot_id, original.snapshot_id);
    assert_eq!(
        store
            .book_documents(f.owner, f.proposal, next.snapshot_id)
            .await
            .unwrap()
            .unwrap()
            .len(),
        1
    );
    assert!(
        store
            .book_documents(f.owner, f.proposal, original.snapshot_id)
            .await
            .unwrap()
            .is_none()
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn publication_seals_unchanged_book_documents_without_expanding_the_diff(pool: PgPool) {
    let (f, source, preparer) = setup_book_options(&pool, false, None, false, None, true).await;
    let (lease, verified) = prepared(&pool, &f, &preparer, 2).await;
    let store = PgReviewSnapshots::new(pool.clone());
    let publication = store.publish(&lease, &verified, &preparer).await.unwrap();
    let viewer = user(&pool, f.org, "READER", vec![]).await;
    let reads = source.reads.load(Ordering::SeqCst);
    let documents = store
        .book_documents(viewer, f.proposal, publication.snapshot_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(documents, verified.candidate().plan().book_documents());
    assert_eq!(documents.len(), 2);
    assert_eq!(documents[0].source.document_id.as_deref(), Some("DOC-B"));
    assert_eq!(documents[0].source.catalog_position, Some(0));
    assert_eq!(documents[1].source.document_id.as_deref(), Some("DOC-A"));
    let files = store
        .files(viewer, f.proposal, publication.snapshot_id, 0, 100)
        .await
        .unwrap();
    assert_eq!(files.total, 1);
    assert_eq!(files.files[0].after.as_ref().unwrap().path, "docs/a.md");
    assert!(
        store
            .book_documents(viewer, Uuid::new_v4(), publication.snapshot_id)
            .await
            .is_err()
    );
    let foreign_org = Uuid::new_v4();
    sqlx::query("INSERT INTO organizations(id,name) VALUES($1,'Other synthetic organization')")
        .bind(foreign_org)
        .execute(&pool)
        .await
        .unwrap();
    let outsider = user(&pool, foreign_org, "READER", vec![]).await;
    assert!(
        store
            .book_documents(outsider, f.proposal, publication.snapshot_id)
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
            .book_documents(viewer, f.proposal, publication.snapshot_id)
            .await
            .is_err()
    );
    assert_eq!(source.reads.load(Ordering::SeqCst), reads);
    for statement in [
        "UPDATE review_snapshot_books SET documents='[]'",
        "DELETE FROM review_snapshot_books",
        "TRUNCATE review_snapshot_books",
        "INSERT INTO review_snapshot_books SELECT organization_id,proposal_id,snapshot_id,documents FROM review_snapshot_books",
    ] {
        assert!(sqlx::query(statement).execute(&pool).await.is_err());
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn book_manifest_requires_complete_scoped_immutable_publication_evidence(pool: PgPool) {
    let (f, _, preparer) = setup_media(&pool, false).await;
    let (lease, verified) = prepared(&pool, &f, &preparer, 2).await;
    let store = PgReviewSnapshots::new(pool.clone());
    let published = store.publish(&lease, &verified, &preparer).await.unwrap();
    let original: Value =
        sqlx::query_scalar("SELECT documents FROM review_snapshot_books WHERE snapshot_id=$1")
            .bind(published.snapshot_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    let (next, _) = prepared(&pool, &f, &preparer, 3).await;
    let copy = "INSERT INTO review_snapshots SELECT (jsonb_populate_record(NULL::review_snapshots,to_jsonb(h)||jsonb_build_object('id',$2::uuid,'created_operation',$3::uuid,'state','PUBLISHING','prepared_version',3,'metadata',jsonb_set(h.metadata,'{summary,expected_proposal_version}','3'::jsonb)))).* FROM review_snapshots h WHERE id=$1";
    for fault in 0..8 {
        let mut tx = pool.begin().await.unwrap();
        let id = Uuid::new_v4();
        sqlx::query(copy)
            .bind(published.snapshot_id)
            .bind(id)
            .bind(next.operation_id())
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query("INSERT INTO review_snapshot_files SELECT organization_id,proposal_id,$2,ordinal,file_id,change FROM review_snapshot_files WHERE snapshot_id=$1")
            .bind(published.snapshot_id).bind(id).execute(&mut *tx).await.unwrap();
        let mut documents = original.clone();
        match fault {
            0 => {} // Missing complete manifest.
            1 => {
                let duplicate = documents[0].clone();
                documents.as_array_mut().unwrap().push(duplicate);
            }
            2 => documents[0]["source"]["path"] = json!("docs/../private.md"),
            3 => documents[0]["source"]["sha256"] = json!("0".repeat(64)),
            4 => documents[0]["source"]["catalog_position"] = json!(1),
            5 => documents[0]["source"]["body"] = json!("must not be stored"),
            6 => documents = json!([]),
            _ => documents[0]["source"]["bytes"] = Value::Null,
        }
        if fault != 0 {
            let insert = sqlx::query("INSERT INTO review_snapshot_books(organization_id,proposal_id,snapshot_id,documents) VALUES($1,$2,$3,$4)")
                .bind(f.org).bind(f.proposal).bind(id).bind(documents).execute(&mut *tx).await;
            if ![3, 6].contains(&fault) {
                assert!(
                    insert.is_err(),
                    "fault {fault} must reject malformed book evidence"
                );
                tx.rollback().await.unwrap();
                continue;
            }
            insert.unwrap();
        }
        assert!(
            sqlx::query("UPDATE review_snapshots SET state='READY' WHERE id=$1")
                .bind(id)
                .execute(&mut *tx)
                .await
                .is_err(),
            "fault {fault} must not seal"
        );
        tx.rollback().await.unwrap();
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM review_snapshot_books")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
}
