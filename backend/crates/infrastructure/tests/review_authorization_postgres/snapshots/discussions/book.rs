use super::*;
use specspace_application::{
    comparison_file::book::VerifiedBookAnchor, discussion::read::DiscussionAnchor,
};
use specspace_infrastructure::review_files::PgReviewBookAuthorization;

struct BookReview {
    f: Fixture,
    source: Arc<Source>,
    reader: ComparisonFileReader,
    snapshot: Uuid,
    entry: String,
    request: OpenThreadRequest,
    preparer: ReviewPreparer,
}
async fn book_review(pool: &PgPool) -> BookReview {
    let (f, source, preparer) = setup_book_options(pool, false, None, false, None, true).await;
    let (lease, verified) = prepared(pool, &f, &preparer, 2).await;
    let result = PgReviewSnapshots::new(pool.clone())
        .publish(&lease, &verified, &preparer)
        .await
        .unwrap();
    let entry = verified.candidate().plan().book_documents()[0].id.clone();
    let reader = ComparisonFileReader::new(
        Arc::new(PgReviewFileAuthorization::new(pool.clone())),
        Arc::new(Factory {
            source: source.clone(),
            repository: f.repository,
        }),
        Duration::from_secs(30),
    )
    .unwrap();
    let request = OpenThreadRequest {
        request_id: Uuid::new_v4(),
        origin: ThreadOrigin::Snapshot {
            snapshot_id: result.snapshot_id,
            expected_fingerprint: result.fingerprint,
            anchor: SnapshotAnchor::BookDocument {
                entry_id: entry.clone(),
                selection: TextSelection::Section {
                    section_id: "SEC-B".into(),
                },
            },
        },
        blocking: true,
        body: "Original unchanged document discussion".into(),
    };
    BookReview {
        f,
        source,
        reader,
        snapshot: result.snapshot_id,
        entry,
        request,
        preparer,
    }
}
async fn proof(
    pool: &PgPool,
    r: &BookReview,
    actor: Uuid,
    entry: &str,
    selection: TextSelection,
) -> VerifiedBookAnchor {
    r.reader
        .with_book_authorization(Arc::new(PgReviewBookAuthorization::new(pool.clone())))
        .load_anchor(actor, r.f.proposal, r.snapshot, entry, selection)
        .await
        .unwrap()
}
#[sqlx::test(migrations = "../../migrations")]
async fn unchanged_book_discussions_are_atomic_replayable_and_keep_original_outdated_context(
    pool: PgPool,
) {
    let r = book_review(&pool).await;
    let store = PgDiscussions::new(pool.clone());
    let before = r.source.reads.load(Ordering::SeqCst);
    let created = store
        .open_thread(r.f.product, r.f.proposal, r.request.clone(), &r.reader)
        .await
        .unwrap();
    assert_eq!(r.source.reads.load(Ordering::SeqCst), before + 1);
    *r.source.failure.lock().unwrap() = Some(GitError::ProviderUnavailable);
    let replay = store
        .open_thread(r.f.product, r.f.proposal, r.request.clone(), &r.reader)
        .await
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(created.id, replay.id);
    assert_eq!(r.source.reads.load(Ordering::SeqCst), before + 1);
    sqlx::query("UPDATE proposals SET head_sha=$2,version=version+1 WHERE id=$1")
        .bind(r.f.proposal)
        .bind(sha(3).as_str())
        .execute(&pool)
        .await
        .unwrap();
    let page = store
        .comments(r.f.owner, r.f.proposal, created.thread_id, None, None)
        .await
        .unwrap();
    assert!(page.thread.outdated_context);
    assert!(page.thread.blocking);
    assert!(!page.thread.resolved);
    match page.thread.origin.anchor {
        DiscussionAnchor::BookDocument {
            entry_id,
            source,
            text,
        } => {
            assert_eq!(entry_id, r.entry);
            assert_eq!(source.path, "docs/b.md");
            assert!(text.quote.contains("Unchanged original body"));
        }
        _ => panic!("original book anchor missing"),
    }
    assert_eq!(
        store
            .threads(r.f.owner, r.f.proposal, None)
            .await
            .unwrap()
            .unresolved_blockers,
        1
    );
    let counts: (i64,i64,i64) = sqlx::query_as("SELECT (SELECT count(*) FROM discussion_threads),(SELECT count(*) FROM audit_events WHERE action='DISCUSSION_OPEN'),(SELECT count(*) FROM outbox WHERE event_type='DISCUSSION_CHANGED')").fetch_one(&pool).await.unwrap();
    assert_eq!(counts, (1, 1, 1));
}
#[sqlx::test(migrations = "../../migrations")]
async fn book_completion_requires_exact_actor_entry_selection_and_current_admission(pool: PgPool) {
    let r = book_review(&pool).await;
    let store = PgDiscussions::new(pool.clone());
    let admitted = admission(&store, r.f.product, r.f.proposal, r.request.clone()).await;
    assert_eq!(
        store.complete_open(&admitted, None).await.err(),
        Some(Error::Invalid)
    );
    let wrong_actor = proof(
        &pool,
        &r,
        r.f.owner,
        &r.entry,
        TextSelection::Section {
            section_id: "SEC-B".into(),
        },
    )
    .await;
    assert_eq!(
        store
            .complete_open(
                &admitted,
                Some(DiscussionSourceProof::BookDocument(&wrong_actor))
            )
            .await
            .err(),
        Some(Error::Stale)
    );
    let documents = PgReviewSnapshots::new(pool.clone())
        .book_documents(r.f.product, r.f.proposal, r.snapshot)
        .await
        .unwrap()
        .unwrap();
    let wrong_entry = proof(
        &pool,
        &r,
        r.f.product,
        &documents[1].id,
        TextSelection::Section {
            section_id: "SEC-A".into(),
        },
    )
    .await;
    assert_eq!(
        store
            .complete_open(
                &admitted,
                Some(DiscussionSourceProof::BookDocument(&wrong_entry))
            )
            .await
            .err(),
        Some(Error::Stale)
    );
    let wrong_selection = proof(&pool, &r, r.f.product, &r.entry, TextSelection::Document {}).await;
    assert_eq!(
        store
            .complete_open(
                &admitted,
                Some(DiscussionSourceProof::BookDocument(&wrong_selection))
            )
            .await
            .err(),
        Some(Error::Invalid)
    );
    let valid = proof(
        &pool,
        &r,
        r.f.product,
        &r.entry,
        TextSelection::Section {
            section_id: "SEC-B".into(),
        },
    )
    .await;
    sqlx::query("UPDATE proposals SET title='Changed metadata',version=version+1 WHERE id=$1")
        .bind(r.f.proposal)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        store
            .complete_open(&admitted, Some(DiscussionSourceProof::BookDocument(&valid)))
            .await
            .err(),
        Some(Error::Stale)
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM discussion_threads")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    let fresh = admission(&store, r.f.product, r.f.proposal, r.request.clone()).await;
    let valid = proof(
        &pool,
        &r,
        r.f.product,
        &r.entry,
        TextSelection::Section {
            section_id: "SEC-B".into(),
        },
    )
    .await;
    sqlx::query("DELETE FROM memberships WHERE user_id=$1")
        .bind(r.f.product)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        store
            .complete_open(&fresh, Some(DiscussionSourceProof::BookDocument(&valid)))
            .await
            .is_err()
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn matching_book_entry_ids_do_not_allow_proofs_from_another_snapshot(pool: PgPool) {
    let r = book_review(&pool).await;
    let store = PgDiscussions::new(pool.clone());
    sqlx::query(
        "UPDATE proposals SET description='New review context',version=version+1 WHERE id=$1",
    )
    .bind(r.f.proposal)
    .execute(&pool)
    .await
    .unwrap();
    let (lease, verified) = prepared(&pool, &r.f, &r.preparer, 4).await;
    let next = PgReviewSnapshots::new(pool.clone())
        .publish(&lease, &verified, &r.preparer)
        .await
        .unwrap();
    assert_ne!(next.snapshot_id, r.snapshot);
    assert_eq!(verified.candidate().plan().book_documents()[0].id, r.entry);
    let admitted = admission(&store, r.f.product, r.f.proposal, r.request.clone()).await;
    let proof = r
        .reader
        .with_book_authorization(Arc::new(PgReviewBookAuthorization::new(pool.clone())))
        .load_anchor(
            r.f.product,
            r.f.proposal,
            next.snapshot_id,
            &r.entry,
            TextSelection::Section {
                section_id: "SEC-B".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .complete_open(&admitted, Some(DiscussionSourceProof::BookDocument(&proof)))
            .await
            .err(),
        Some(Error::Stale)
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn book_origin_database_guard_rolls_back_forged_source_and_preserves_exact_retry(
    pool: PgPool,
) {
    let r = book_review(&pool).await;
    let store = PgDiscussions::new(pool.clone());
    let admitted = admission(&store, r.f.product, r.f.proposal, r.request.clone()).await;
    let valid = proof(
        &pool,
        &r,
        r.f.product,
        &r.entry,
        TextSelection::Section {
            section_id: "SEC-B".into(),
        },
    )
    .await;
    sqlx::raw_sql("CREATE FUNCTION tamper_book_origin() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN NEW.anchor=jsonb_set(NEW.anchor,'{source,path}','\"docs/forged.md\"'::jsonb); RETURN NEW; END $$; CREATE TRIGGER a_tamper_book_origin BEFORE INSERT ON discussion_threads FOR EACH ROW EXECUTE FUNCTION tamper_book_origin();").execute(&pool).await.unwrap();
    assert!(
        store
            .complete_open(&admitted, Some(DiscussionSourceProof::BookDocument(&valid)))
            .await
            .is_err()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM discussion_revisions")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    sqlx::query("DROP TRIGGER a_tamper_book_origin ON discussion_threads")
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        !store
            .complete_open(&admitted, Some(DiscussionSourceProof::BookDocument(&valid)))
            .await
            .unwrap()
            .replayed
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn book_source_limits_share_durable_cooldown_and_merge_admission_barrier(pool: PgPool) {
    let r = book_review(&pool).await;
    let store = PgDiscussions::new(pool.clone());
    *r.source.failure.lock().unwrap() = Some(GitError::RateLimited {
        retry_after_seconds: Some(12),
    });
    assert_eq!(
        store
            .open_thread(r.f.product, r.f.proposal, r.request.clone(), &r.reader)
            .await
            .err(),
        Some(Error::ProviderRateLimited(12))
    );
    let reads = r.source.reads.load(Ordering::SeqCst);
    let mut next = r.request.clone();
    next.request_id = Uuid::new_v4();
    assert!(matches!(
        store
            .open_thread(r.f.owner, r.f.proposal, next.clone(), &r.reader)
            .await,
        Err(Error::ProviderRateLimited(_))
    ));
    assert_eq!(r.source.reads.load(Ordering::SeqCst), reads);
    sqlx::query("UPDATE proposals SET state='MERGING',version=version+1 WHERE id=$1")
        .bind(r.f.proposal)
        .execute(&pool)
        .await
        .unwrap();
    next.request_id = Uuid::new_v4();
    assert_eq!(
        store
            .open_thread(r.f.product, r.f.proposal, next, &r.reader)
            .await
            .err(),
        Some(Error::NotReady)
    );
    assert_eq!(r.source.reads.load(Ordering::SeqCst), reads);
    let results: Vec<String> = sqlx::query_scalar(
        "SELECT result FROM audit_events WHERE action='DISCUSSION_SOURCE_STOPPED' ORDER BY result",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(results, ["PROVIDER_BACKOFF", "PROVIDER_RATE_LIMIT"]);
}
