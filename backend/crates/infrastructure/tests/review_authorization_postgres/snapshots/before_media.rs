use super::*;
use specspace_application::comparison_file::{
    ComparisonFileAuthorization, ComparisonFileReader, FileReadContext, FileReadError,
};
use specspace_infrastructure::review_files::PgReviewFileAuthorization;
const ASSET: &str = "assets/5e3d382db4dd83d59aa5742793ad6b7903409e865c83bcbc54835049f043bc15.png";
struct Published {
    fixture: Fixture,
    source: Arc<Source>,
    reader: ComparisonFileReader,
    snapshot: Uuid,
    file: String,
}
async fn published(pool: &PgPool, body: &str) -> Published {
    let (f, source, preparer) = setup_body_options(pool, true, None, true, Some(body)).await;
    let (lease, verified) = prepared(pool, &f, &preparer, 2).await;
    let file = verified
        .candidate()
        .plan()
        .files()
        .iter()
        .find(|file| file.kind == specspace_application::comparison::Kind::Document)
        .unwrap()
        .id
        .clone();
    let result = PgReviewSnapshots::new(pool.clone())
        .publish(&lease, &verified, &preparer)
        .await
        .unwrap();
    let reader = ComparisonFileReader::new(
        Arc::new(PgReviewFileAuthorization::new(pool.clone())),
        Arc::new(Factory {
            source: source.clone(),
            repository: f.repository,
        }),
        Duration::from_secs(60),
    )
    .unwrap();
    Published {
        fixture: f,
        source,
        reader,
        snapshot: result.snapshot_id,
        file,
    }
}
#[sqlx::test(migrations = "../../migrations")]
async fn before_image_is_anchored_to_exact_document_reference_and_historical_base_after_closure(
    pool: PgPool,
) {
    let p = published(&pool, &format!("Before\n\n![Before image]({ASSET})")).await;
    let f = &p.fixture;
    let actor = user(&pool, f.org, "READER", vec![]).await;
    p.reader
        .load(actor, f.proposal, p.snapshot, &p.file)
        .await
        .unwrap();
    sqlx::query("UPDATE proposals SET state='CLOSED',head_sha=$2,target_sha=$2,version=version+1 WHERE id=$1").bind(f.proposal).bind(sha(3).as_str()).execute(&pool).await.unwrap();
    p.source.target.store(3, Ordering::SeqCst);
    let start = p.source.reads.load(Ordering::SeqCst);
    let loaded = p
        .reader
        .load_before_image(actor, f.proposal, p.snapshot, &p.file, ASSET)
        .await
        .unwrap();
    assert_eq!(loaded.source().path, ASSET);
    assert_eq!(loaded.source().sha256.as_deref(), Some(&ASSET[7..71]));
    assert_eq!(loaded.image().context().change.id, p.file);
    assert_eq!(
        loaded
            .image()
            .context()
            .change
            .before
            .as_ref()
            .unwrap()
            .path,
        "docs/a.md"
    );
    assert_eq!(loaded.image().context().merge_base, sha(0));
    assert_eq!(loaded.image().context().scope.recorded_head, sha(3));
    assert_eq!(
        loaded.image().side(),
        specspace_application::comparison_file::image::ImageSide::Before
    );
    assert!(loaded.image().image().png.starts_with(b"\x89PNG\r\n\x1a\n"));
    p.reader.recheck_image(actor, loaded.image()).await.unwrap();
    assert_eq!(
        p.source.reads.load(Ordering::SeqCst),
        start + 2,
        "Only the frozen base tree and referenced bytes may be fetched"
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn only_canonical_before_image_events_authorize_reads_not_after_refs_code_html_or_plain_links(
    pool: PgPool,
) {
    for (body, allowed) in [
        (
            format!("Before\n\n![Before image][picture]\n\n[picture]: {ASSET}"),
            true,
        ),
        ("Before without an image".into(), false),
        (format!("```md\n![Example]({ASSET})\n```"), false),
        (format!("<img src=\"{ASSET}\">"), false),
        (format!("[Ordinary link]({ASSET})"), false),
    ] {
        let p = published(&pool, &body).await;
        let f = &p.fixture;
        p.reader
            .load(f.owner, f.proposal, p.snapshot, &p.file)
            .await
            .unwrap();
        let start = p.source.reads.load(Ordering::SeqCst);
        let result = p
            .reader
            .load_before_image(f.owner, f.proposal, p.snapshot, &p.file, ASSET)
            .await;
        assert_eq!(result.is_ok(), allowed, "{body}");
        assert_eq!(
            p.source.reads.load(Ordering::SeqCst),
            start + if allowed { 2 } else { 0 }
        );
    }
}
#[sqlx::test(migrations = "../../migrations")]
async fn before_media_scope_rejects_foreign_private_path_and_reconfigured_repository_without_io(
    pool: PgPool,
) {
    let p = published(&pool, &format!("![Before]({ASSET})")).await;
    let f = &p.fixture;
    let foreign = fixture(&pool).await;
    let actor = user(&pool, foreign.org, "ADMIN", vec![]).await;
    let private = specspace_infrastructure::comparisons::PgComparisons::new(pool.clone())
        .reserve(
            f.owner,
            f.proposal,
            &IdempotencyKey::parse("private-before-media").unwrap(),
            specspace_application::comparison_reader::Selection::default(),
        )
        .await
        .unwrap();
    let start = p.source.reads.load(Ordering::SeqCst);
    for (actor, snapshot, path) in [
        (actor, p.snapshot, ASSET),
        (f.owner, private.snapshot_id, ASSET),
        (f.owner, p.snapshot, "https://example.com/a.png"),
    ] {
        assert!(
            p.reader
                .load_before_image(actor, f.proposal, snapshot, &p.file, path)
                .await
                .is_err()
        );
    }
    sqlx::query("UPDATE repositories SET installation_id='99' WHERE id=$1")
        .bind(f.repository)
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        p.reader
            .load_before_image(f.owner, f.proposal, p.snapshot, &p.file, ASSET)
            .await,
        Err(FileReadError::Access(ComparisonReadError::ContextChanged))
    ));
    assert_eq!(p.source.reads.load(Ordering::SeqCst), start);
}
struct Revoke {
    inner: PgReviewFileAuthorization,
    pool: PgPool,
    calls: AtomicUsize,
    at: AtomicUsize,
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
        if self.calls.fetch_add(1, Ordering::SeqCst) + 1 == self.at.load(Ordering::SeqCst) {
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
async fn revocation_at_every_before_media_async_boundary_discards_results_and_forged_pixels_never_return(
    pool: PgPool,
) {
    let p = published(&pool, &format!("![Before]({ASSET})")).await;
    let f = &p.fixture;
    for boundary in 3..=8 {
        let actor = user(&pool, f.org, "READER", vec![]).await;
        let authority = Arc::new(Revoke {
            inner: PgReviewFileAuthorization::new(pool.clone()),
            pool: pool.clone(),
            calls: AtomicUsize::new(0),
            at: AtomicUsize::new(0),
        });
        let reader = p.reader.with_authorization(authority.clone());
        reader
            .load(actor, f.proposal, p.snapshot, &p.file)
            .await
            .unwrap();
        authority.calls.store(0, Ordering::SeqCst);
        authority.at.store(boundary, Ordering::SeqCst);
        assert!(
            matches!(
                reader
                    .load_before_image(actor, f.proposal, p.snapshot, &p.file, ASSET)
                    .await,
                Err(FileReadError::Access(ComparisonReadError::NotFound))
            ),
            "boundary {boundary}"
        );
    }
    p.source.invalid_media.store(true, Ordering::SeqCst);
    assert!(matches!(
        p.reader
            .load_before_image(f.owner, f.proposal, p.snapshot, &p.file, ASSET)
            .await,
        Err(FileReadError::InvalidEvidence)
    ));
}
