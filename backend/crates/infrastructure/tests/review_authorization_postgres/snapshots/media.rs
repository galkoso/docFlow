use super::*;
use specspace_application::{
    comparison::Kind,
    comparison_file::{
        ComparisonFileAuthorization, ComparisonFileReader, FileReadError, image::ImageSide,
    },
};
use specspace_infrastructure::review_files::{
    PgReviewFileAuthorization, PgReviewMediaAuthorization,
};

struct PublishedMedia {
    fixture: Fixture,
    source: Arc<Source>,
    reader: ComparisonFileReader,
    authorization: Arc<PgReviewMediaAuthorization>,
    snapshot: Uuid,
    path: String,
}
async fn published(pool: &PgPool, reused: bool) -> PublishedMedia {
    let (f, source, preparer) = setup_body_options(pool, true, None, reused).await;
    let (lease, verified) = prepared(pool, &f, &preparer, 2).await;
    if reused {
        assert!(
            verified
                .candidate()
                .plan()
                .files()
                .iter()
                .all(|file| file.kind != Kind::Asset)
        );
    }
    let path = verified.media()[0].path().to_owned();
    let receipt = PgReviewSnapshots::new(pool.clone())
        .publish(&lease, &verified, &preparer)
        .await
        .unwrap();
    let authorization =
        Arc::new(PgReviewMediaAuthorization::new(pool.clone(), path.clone()).unwrap());
    let private = ComparisonFileReader::new(
        Arc::new(PgReviewFileAuthorization::new(pool.clone())),
        Arc::new(Factory {
            source: source.clone(),
            repository: f.repository,
        }),
        Duration::from_secs(60),
    )
    .unwrap();
    let reader = private.with_authorization(authorization.clone());
    PublishedMedia {
        fixture: f,
        source,
        reader,
        authorization,
        snapshot: receipt.snapshot_id,
        path,
    }
}
#[sqlx::test(migrations = "../../migrations")]
async fn sealed_referenced_media_without_a_changed_asset_is_available_to_current_readers(
    pool: PgPool,
) {
    let p = published(&pool, true).await;
    let f = &p.fixture;
    let actor = user(&pool, f.org, "READER", vec![]).await;
    let start = p.source.reads.load(Ordering::SeqCst);
    let context = p
        .authorization
        .resolve_file(actor, f.proposal, p.snapshot, p.authorization.file_id())
        .await
        .unwrap();
    assert_eq!(p.source.reads.load(Ordering::SeqCst), start);
    assert_eq!(context.head, sha(1));
    assert_eq!(context.merge_base, sha(0));
    assert_eq!(context.change.after.as_ref().unwrap().path, p.path);
    let image = p
        .reader
        .load_image(
            actor,
            f.proposal,
            p.snapshot,
            p.authorization.file_id(),
            ImageSide::After,
        )
        .await
        .unwrap();
    assert_eq!(image.source_sha256(), &p.path[7..71]);
    assert_eq!(
        image
            .context()
            .published_after_image
            .as_ref()
            .unwrap()
            .display_sha256,
        image.display_sha256()
    );
    assert!(image.image().png.starts_with(b"\x89PNG\r\n\x1a\n"));
    p.reader.recheck_image(actor, &image).await.unwrap();
    assert_eq!(p.source.reads.load(Ordering::SeqCst), start + 1);
    assert!(matches!(
        p.reader
            .load_image(
                actor,
                f.proposal,
                p.snapshot,
                p.authorization.file_id(),
                ImageSide::Before
            )
            .await,
        Err(FileReadError::NotFound)
    ));
    assert_eq!(p.source.reads.load(Ordering::SeqCst), start + 1);
}
#[sqlx::test(migrations = "../../migrations")]
async fn referenced_media_uses_historical_head_after_closure_policy_and_target_changes(
    pool: PgPool,
) {
    let p = published(&pool, true).await;
    let f = &p.fixture;
    sqlx::query("UPDATE proposals SET state='CLOSED',head_sha=$2,target_sha=$2,version=version+1 WHERE id=$1").bind(f.proposal).bind(sha(3).as_str()).execute(&pool).await.unwrap();
    sqlx::query("UPDATE organizations SET policy_version=2 WHERE id=$1")
        .bind(f.org)
        .execute(&pool)
        .await
        .unwrap();
    p.source.target.store(3, Ordering::SeqCst);
    let image = p
        .reader
        .load_image(
            f.owner,
            f.proposal,
            p.snapshot,
            p.authorization.file_id(),
            ImageSide::After,
        )
        .await
        .unwrap();
    assert_eq!(image.context().head, sha(1));
    assert_eq!(image.context().scope.recorded_head, sha(3));
    assert_eq!(image.source_sha256(), &p.path[7..71]);
}
#[sqlx::test(migrations = "../../migrations")]
async fn media_paths_ids_private_snapshots_and_foreign_scope_cannot_substitute_for_sealed_receipts(
    pool: PgPool,
) {
    let p = published(&pool, false).await;
    let f = &p.fixture;
    let foreign = fixture(&pool).await;
    let foreign_actor = user(&pool, foreign.org, "ADMIN", vec![]).await;
    let private = specspace_infrastructure::comparisons::PgComparisons::new(pool.clone())
        .reserve(
            f.owner,
            f.proposal,
            &IdempotencyKey::parse("private-media-read").unwrap(),
            specspace_application::comparison_reader::Selection::default(),
        )
        .await
        .unwrap();
    let start = p.source.reads.load(Ordering::SeqCst);
    for (actor, proposal, snapshot, file) in [
        (
            f.owner,
            f.proposal,
            private.snapshot_id,
            p.authorization.file_id(),
        ),
        (
            foreign_actor,
            f.proposal,
            p.snapshot,
            p.authorization.file_id(),
        ),
        (
            f.owner,
            foreign.proposal,
            p.snapshot,
            p.authorization.file_id(),
        ),
        (
            f.owner,
            f.proposal,
            Uuid::new_v4(),
            p.authorization.file_id(),
        ),
        (f.owner, f.proposal, p.snapshot, "wrong identity"),
    ] {
        assert!(
            p.reader
                .load_image(actor, proposal, snapshot, file, ImageSide::After)
                .await
                .is_err()
        );
    }
    let missing = Arc::new(
        PgReviewMediaAuthorization::new(pool.clone(), format!("assets/{}.png", "0".repeat(64)))
            .unwrap(),
    );
    let other = p.reader.with_authorization(missing.clone());
    assert!(matches!(
        other
            .load_image(
                f.owner,
                f.proposal,
                p.snapshot,
                missing.file_id(),
                ImageSide::After
            )
            .await,
        Err(FileReadError::NotFound)
    ));
    for path in [
        "../assets/a.png",
        "https://example.com/a.png",
        "assets/a.png",
        &format!("assets/{}.svg", "a".repeat(64)),
        &format!("assets/{}.png?ref=main", "a".repeat(64)),
    ] {
        assert!(PgReviewMediaAuthorization::new(pool.clone(), path.to_owned()).is_err());
    }
    assert_eq!(p.source.reads.load(Ordering::SeqCst), start);
}
#[sqlx::test(migrations = "../../migrations")]
async fn binding_changes_and_membership_revocation_deny_media_before_provider_io(pool: PgPool) {
    let p = published(&pool, true).await;
    let f = &p.fixture;
    let actor = user(&pool, f.org, "READER", vec![]).await;
    let start = p.source.reads.load(Ordering::SeqCst);
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
                .load_image(
                    actor,
                    f.proposal,
                    p.snapshot,
                    p.authorization.file_id(),
                    ImageSide::After
                )
                .await,
            Err(FileReadError::Access(ComparisonReadError::ContextChanged))
        ));
        sqlx::query("UPDATE repositories SET remote_id='42',installation_id='7' WHERE id=$1")
            .bind(f.repository)
            .execute(&pool)
            .await
            .unwrap();
    }
    sqlx::query("DELETE FROM memberships WHERE organization_id=$1 AND user_id=$2")
        .bind(f.org)
        .bind(actor)
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        p.reader
            .load_image(
                actor,
                f.proposal,
                p.snapshot,
                p.authorization.file_id(),
                ImageSide::After
            )
            .await,
        Err(FileReadError::Access(ComparisonReadError::NotFound))
    ));
    assert_eq!(p.source.reads.load(Ordering::SeqCst), start);
}
