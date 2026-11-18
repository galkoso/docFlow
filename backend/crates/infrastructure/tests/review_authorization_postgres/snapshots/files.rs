use super::*;
use specspace_application::{
    comparison::{Change, Kind},
    comparison_file::{
        ComparisonFileAuthorization, ComparisonFileReader, FileReadContext, FileReadError,
        image::ImageSide,
    },
    comparison_reader::Selection,
};
use specspace_infrastructure::{
    comparisons::PgComparisons, review_files::PgReviewFileAuthorization,
};
struct Published {
    fixture: Fixture,
    source: Arc<Source>,
    authorization: Arc<PgReviewFileAuthorization>,
    reader: Arc<ComparisonFileReader>,
    snapshot: Uuid,
    files: Vec<Change>,
}
async fn published(pool: &PgPool, media: bool) -> Published {
    let (fixture, source, preparer) = setup_media(pool, media).await;
    let (lease, verified) = prepared(pool, &fixture, &preparer, 2).await;
    let files = verified.candidate().plan().files().to_vec();
    let result = PgReviewSnapshots::new(pool.clone())
        .publish(&lease, &verified, &preparer)
        .await
        .unwrap();
    let authorization = Arc::new(PgReviewFileAuthorization::new(pool.clone()));
    let factory = Arc::new(Factory {
        source: source.clone(),
        repository: fixture.repository,
    });
    let reader = Arc::new(
        ComparisonFileReader::new(authorization.clone(), factory, Duration::from_secs(60)).unwrap(),
    );
    Published {
        fixture,
        source,
        authorization,
        reader,
        snapshot: result.snapshot_id,
        files,
    }
}
fn document(p: &Published) -> &str {
    &p.files
        .iter()
        .find(|file| file.kind == Kind::Document)
        .unwrap()
        .id
}
fn asset(p: &Published) -> &str {
    &p.files
        .iter()
        .find(|file| file.kind == Kind::Asset)
        .unwrap()
        .id
}

#[sqlx::test(migrations = "../../migrations")]
async fn readers_get_exact_shared_source_and_sealed_image_evidence_without_private_job_access(
    pool: PgPool,
) {
    let p = published(&pool, true).await;
    let f = &p.fixture;
    let reader = user(&pool, f.org, "READER", vec![]).await;
    let start = p.source.reads.load(Ordering::SeqCst);
    let context = p
        .authorization
        .resolve_file(reader, f.proposal, p.snapshot, document(&p))
        .await
        .unwrap();
    assert_eq!(context.scope.actor, reader);
    assert_eq!(context.scope.version, 3);
    assert_eq!(context.head, sha(1));
    assert_eq!(context.merge_base, sha(0));
    assert_eq!(context.change.id, document(&p));
    assert!(context.published_after_image.is_none());
    assert_eq!(p.source.reads.load(Ordering::SeqCst), start);
    let text = p
        .reader
        .load(reader, f.proposal, p.snapshot, document(&p))
        .await
        .unwrap();
    assert!(text.before().unwrap().contains("Before"));
    assert!(
        text.after()
            .unwrap()
            .contains("Saved proposal with verified media")
    );
    assert_eq!(p.source.reads.load(Ordering::SeqCst), start + 2);
    let image = p
        .reader
        .load_image(reader, f.proposal, p.snapshot, asset(&p), ImageSide::After)
        .await
        .unwrap();
    let proof = image.context().published_after_image.as_ref().unwrap();
    assert_eq!(proof.source_sha256, image.source_sha256());
    assert_eq!(proof.display_sha256, image.display_sha256());
    assert_eq!(
        (proof.width, proof.height),
        (image.image().width, image.image().height)
    );
    assert_eq!(
        proof.validation_version,
        specspace_document_engine::media::VALIDATION_VERSION
    );
    p.reader.recheck_image(reader, &image).await.unwrap();
    let job: Uuid =
        sqlx::query_scalar("SELECT job_id FROM review_submissions WHERE proposal_id=$1")
            .bind(f.proposal)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(matches!(
        PgReviewSubmissions::new(pool.clone())
            .status(reader, job)
            .await,
        Err(SubmissionError::NotFound)
    ));
}

#[sqlx::test(migrations = "../../migrations")]
async fn historical_source_survives_head_target_policy_and_closure_changes_without_reselecting_refs(
    pool: PgPool,
) {
    let p = published(&pool, false).await;
    let f = &p.fixture;
    let actor = user(&pool, f.org, "READER", vec![]).await;
    let old = p
        .reader
        .load(actor, f.proposal, p.snapshot, document(&p))
        .await
        .unwrap();
    let reads = p.source.reads.load(Ordering::SeqCst);
    sqlx::query("UPDATE proposals SET head_sha=$2,target_sha=$2,version=version+1,state='CLOSED' WHERE id=$1").bind(f.proposal).bind(sha(3).as_str()).execute(&pool).await.unwrap();
    sqlx::query("UPDATE organizations SET policy_version=2 WHERE id=$1")
        .bind(f.org)
        .execute(&pool)
        .await
        .unwrap();
    p.source.target.store(3, Ordering::SeqCst);
    let loaded = p
        .reader
        .load(actor, f.proposal, p.snapshot, document(&p))
        .await
        .unwrap();
    assert_eq!(loaded.before(), old.before());
    assert_eq!(loaded.after(), old.after());
    assert_eq!(loaded.head(), &sha(1));
    assert_eq!(loaded.merge_base(), &sha(0));
    assert_eq!(loaded.content_id(), old.content_id());
    assert_eq!(p.source.reads.load(Ordering::SeqCst), reads);
    let context = p
        .authorization
        .resolve_file(actor, f.proposal, p.snapshot, document(&p))
        .await
        .unwrap();
    assert_eq!(context.scope.recorded_head, sha(3));
    assert_eq!(context.scope.state, "CLOSED");
}

#[sqlx::test(migrations = "../../migrations")]
async fn shared_body_authority_never_falls_back_to_private_snapshots_or_foreign_scope(
    pool: PgPool,
) {
    let p = published(&pool, false).await;
    let f = &p.fixture;
    let private = PgComparisons::new(pool.clone())
        .reserve(
            f.owner,
            f.proposal,
            &IdempotencyKey::parse("private-read-scope").unwrap(),
            Selection::default(),
        )
        .await
        .unwrap();
    let foreign = fixture(&pool).await;
    let foreign_admin = user(&pool, foreign.org, "ADMIN", vec![]).await;
    let reader = user(&pool, f.org, "READER", vec![]).await;
    let reads = p.source.reads.load(Ordering::SeqCst);
    for (actor, proposal, snapshot, file) in [
        (reader, f.proposal, private.snapshot_id, document(&p)),
        (foreign_admin, f.proposal, p.snapshot, document(&p)),
        (reader, foreign.proposal, p.snapshot, document(&p)),
        (reader, f.proposal, p.snapshot, "../docs/a.md"),
        (reader, f.proposal, p.snapshot, &"0".repeat(64)),
    ] {
        assert!(
            p.reader
                .load(actor, proposal, snapshot, file)
                .await
                .is_err()
        );
    }
    assert_eq!(p.source.reads.load(Ordering::SeqCst), reads);
}

#[sqlx::test(migrations = "../../migrations")]
async fn cached_shared_text_rechecks_membership_and_never_reuses_another_actors_payload(
    pool: PgPool,
) {
    let p = published(&pool, false).await;
    let f = &p.fixture;
    let a = user(&pool, f.org, "READER", vec![]).await;
    let b = user(&pool, f.org, "READER", vec![]).await;
    p.reader
        .load(a, f.proposal, p.snapshot, document(&p))
        .await
        .unwrap();
    let reads = p.source.reads.load(Ordering::SeqCst);
    p.reader
        .load(b, f.proposal, p.snapshot, document(&p))
        .await
        .unwrap();
    assert_eq!(p.source.reads.load(Ordering::SeqCst), reads + 2);
    sqlx::query("DELETE FROM memberships WHERE organization_id=$1 AND user_id=$2")
        .bind(f.org)
        .bind(a)
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        p.reader.load(a, f.proposal, p.snapshot, document(&p)).await,
        Err(FileReadError::Access(ComparisonReadError::NotFound))
    ));
    assert_eq!(p.source.reads.load(Ordering::SeqCst), reads + 2);
    p.reader
        .load(b, f.proposal, p.snapshot, document(&p))
        .await
        .unwrap();
    assert_eq!(p.source.reads.load(Ordering::SeqCst), reads + 2);
}

#[sqlx::test(migrations = "../../migrations")]
async fn provider_binding_changes_cannot_redirect_cached_or_uncached_snapshot_reads(pool: PgPool) {
    let p = published(&pool, false).await;
    let f = &p.fixture;
    p.reader
        .load(f.owner, f.proposal, p.snapshot, document(&p))
        .await
        .unwrap();
    let reads = p.source.reads.load(Ordering::SeqCst);
    for column in ["remote_id", "installation_id"] {
        sqlx::query(&format!(
            "UPDATE repositories SET {column}='99' WHERE id=$1"
        ))
        .bind(f.repository)
        .execute(&pool)
        .await
        .unwrap();
        assert!(matches!(
            p.reader
                .load(f.owner, f.proposal, p.snapshot, document(&p))
                .await,
            Err(FileReadError::Access(ComparisonReadError::ContextChanged))
        ));
        sqlx::query("UPDATE repositories SET remote_id='42',installation_id='7' WHERE id=$1")
            .bind(f.repository)
            .execute(&pool)
            .await
            .unwrap();
    }
    sqlx::query("UPDATE proposals SET branch='change/redirected' WHERE id=$1")
        .bind(f.proposal)
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        p.reader
            .load(f.owner, f.proposal, p.snapshot, document(&p))
            .await,
        Err(FileReadError::Access(ComparisonReadError::ContextChanged))
    ));
    sqlx::query("UPDATE proposals SET branch='change/review-scope' WHERE id=$1")
        .bind(f.proposal)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(p.source.reads.load(Ordering::SeqCst), reads);
    p.reader
        .load(f.owner, f.proposal, p.snapshot, document(&p))
        .await
        .unwrap();
    assert_eq!(p.source.reads.load(Ordering::SeqCst), reads);
    assert!(
        PgReviewSnapshots::new(pool.clone())
            .get(f.owner, f.proposal, p.snapshot)
            .await
            .is_ok()
    );
}

struct Revoke {
    inner: Arc<PgReviewFileAuthorization>,
    pool: PgPool,
    calls: AtomicUsize,
    at: usize,
}
#[async_trait]
impl ComparisonFileAuthorization for Revoke {
    async fn resolve_file(
        &self,
        actor: Uuid,
        proposal: Uuid,
        snapshot: Uuid,
        file: &str,
    ) -> Result<FileReadContext, FileReadError> {
        if self.calls.fetch_add(1, Ordering::SeqCst) + 1 == self.at {
            sqlx::query("UPDATE users SET status='DISABLED' WHERE id=$1")
                .bind(actor)
                .execute(&self.pool)
                .await
                .unwrap();
        }
        self.inner
            .resolve_file(actor, proposal, snapshot, file)
            .await
    }
}
#[sqlx::test(migrations = "../../migrations")]
async fn revocation_after_io_or_cpu_discards_shared_text_and_image_results(pool: PgPool) {
    let p = published(&pool, true).await;
    let f = &p.fixture;
    for (image, boundary) in [(false, 3), (false, 5), (true, 3), (true, 4)] {
        let actor = user(&pool, f.org, "READER", vec![]).await;
        let auth = Arc::new(Revoke {
            inner: p.authorization.clone(),
            pool: pool.clone(),
            calls: AtomicUsize::new(0),
            at: boundary,
        });
        let reader = p.reader.with_authorization(auth);
        let result = if image {
            reader
                .load_image(actor, f.proposal, p.snapshot, asset(&p), ImageSide::After)
                .await
                .map(|_| ())
        } else {
            reader
                .load(actor, f.proposal, p.snapshot, document(&p))
                .await
                .map(|_| ())
        };
        assert!(
            matches!(
                result,
                Err(FileReadError::Access(ComparisonReadError::NotFound))
            ),
            "image={image} boundary={boundary}"
        );
    }
}
