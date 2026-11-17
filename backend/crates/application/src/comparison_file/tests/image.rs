use super::*;
use crate::comparison_file::image::{ImageSide, LoadedImage};
use specspace_document_engine::media;

fn png() -> Vec<u8> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../fixtures/repository/assets");
    std::fs::read(
        std::fs::read_dir(root)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path(),
    )
    .unwrap()
}
fn image_side(bytes: &[u8]) -> Side {
    Side {
        path: format!("assets/{:x}.png", Sha256::digest(bytes)),
        blob_oid: blob::oid(bytes, 40).unwrap().as_str().into(),
        bytes: bytes.len() as u64,
        executable: false,
        sha256: None,
        document_id: None,
        catalog_position: None,
    }
}
fn fixture(added: bool, bytes: Vec<u8>) -> Fixture {
    fixture_path(added, bytes, false)
}
fn fixture_path(added: bool, bytes: Vec<u8>, wrong_path: bool) -> Fixture {
    let seed = Fixture::new("", "");
    let mut context = seed.authority.context.lock().unwrap().clone();
    context.change.kind = Kind::Asset;
    context.change.status = if added {
        Status::Added
    } else {
        Status::Deleted
    };
    context.change.matched_by = None;
    context.change.content_changed = true;
    context.change.before = (!added).then(|| image_side(&bytes));
    context.change.after = added.then(|| image_side(&bytes));
    if wrong_path {
        context.change.after.as_mut().unwrap().path = format!("assets/{}.png", "9".repeat(64));
    }
    let provider = Arc::new(Provider {
        context: context.clone(),
        sources: Mutex::new(("".into(), "".into())),
        binary: Mutex::new(Some((bytes.clone(), bytes))),
        reads: AtomicUsize::new(0),
        failure: Mutex::new(None),
        gate: tokio::sync::Notify::new(),
        block: std::sync::atomic::AtomicBool::new(false),
    });
    let authority = Arc::new(Authority {
        context: Mutex::new(context),
        calls: AtomicUsize::new(0),
        fail_at: AtomicUsize::new(0),
        change_at: AtomicUsize::new(0),
    });
    let factory = Arc::new(Factory {
        provider,
        connects: AtomicUsize::new(0),
    });
    let reader = Arc::new(
        ComparisonFileReader::new(authority.clone(), factory.clone(), Duration::from_secs(60))
            .unwrap(),
    );
    Fixture {
        authority,
        factory,
        reader,
    }
}
async fn load(f: &Fixture, side: ImageSide) -> Result<LoadedImage, FileReadError> {
    let c = f.authority.context.lock().unwrap().clone();
    f.reader
        .load_image(
            c.scope.actor,
            c.scope.proposal,
            c.snapshot,
            &c.change.id,
            side,
        )
        .await
}
#[tokio::test]
async fn exact_snapshot_side_verifies_source_and_returns_only_bounded_reencoded_pixels() {
    for (added, side) in [(false, ImageSide::Before), (true, ImageSide::After)] {
        let mut source = png();
        source.extend_from_slice(b"PRIVATE SOURCE TRAILER");
        let f = fixture(added, source.clone());
        let loaded = load(&f, side).await.unwrap();
        assert_eq!(loaded.side(), side);
        assert_eq!((loaded.image().width, loaded.image().height), (1, 1));
        assert_eq!(
            loaded.source_sha256(),
            format!("{:x}", Sha256::digest(&source))
        );
        assert_eq!(
            loaded.display_sha256(),
            format!("{:x}", Sha256::digest(&loaded.image().png))
        );
        assert!(
            !loaded
                .image()
                .png
                .windows(22)
                .any(|part| part == b"PRIVATE SOURCE TRAILER")
        );
        assert_eq!(f.factory.provider.reads.load(Ordering::SeqCst), 1);
        let absent = if added {
            ImageSide::Before
        } else {
            ImageSide::After
        };
        assert!(matches!(
            load(&f, absent).await,
            Err(FileReadError::NotFound)
        ));
        assert_eq!(f.factory.provider.reads.load(Ordering::SeqCst), 1);
    }
}
#[tokio::test]
async fn mismatched_lengths_git_oids_source_hashes_and_content_paths_never_return_pixels() {
    for case in 0..3 {
        let f = fixture(true, png());
        {
            let mut c = f.authority.context.lock().unwrap();
            let side = c.change.after.as_mut().unwrap();
            match case {
                0 => side.bytes += 1,
                1 => side.blob_oid = "9".repeat(40),
                2 => side.sha256 = Some("9".repeat(64)),
                _ => side.path = format!("assets/{}.png", "9".repeat(64)),
            }
        }
        assert!(matches!(
            load(&f, ImageSide::After).await,
            Err(FileReadError::InvalidEvidence)
        ));
    }
    let f = fixture_path(true, png(), true);
    assert!(matches!(
        load(&f, ImageSide::After).await,
        Err(FileReadError::InvalidImage("ASSET_HASH_MISMATCH"))
    ));
    let bytes = b"<svg onload='execute()'/>".to_vec();
    let f = fixture(true, bytes);
    assert!(matches!(
        load(&f, ImageSide::After).await,
        Err(FileReadError::InvalidImage("UNSUPPORTED_MEDIA_TYPE"))
    ));
}
#[tokio::test]
async fn unsupported_paths_executables_and_oversized_metadata_are_rejected_before_provider_access()
{
    for case in 0..4 {
        let f = fixture(true, png());
        {
            let mut c = f.authority.context.lock().unwrap();
            let side = c.change.after.as_mut().unwrap();
            match case {
                0 => side.path = "assets/a.svg".into(),
                1 => side.executable = true,
                2 => side.bytes = media::MAX_ASSET_BYTES as u64 + 1,
                _ => c.change.kind = Kind::Document,
            }
        }
        assert!(load(&f, ImageSide::After).await.is_err());
        assert_eq!(f.factory.connects.load(Ordering::SeqCst), 0);
    }
}
#[tokio::test]
async fn revocation_before_fetch_after_fetch_after_decode_and_before_http_publication_discards_pixels()
 {
    for call in 2..=4 {
        let f = fixture(true, png());
        f.authority.fail_at.store(call, Ordering::SeqCst);
        assert!(matches!(
            load(&f, ImageSide::After).await,
            Err(FileReadError::NotFound)
        ));
    }
    let f = fixture(true, png());
    let image = load(&f, ImageSide::After).await.unwrap();
    let actor = f.authority.context.lock().unwrap().scope.actor;
    f.authority.fail_at.store(5, Ordering::SeqCst);
    assert!(matches!(
        f.reader.recheck_image(actor, &image).await,
        Err(FileReadError::NotFound)
    ));
}
#[tokio::test]
async fn provider_backoff_and_context_changes_are_preserved_without_reselecting_refs() {
    let f = fixture(true, png());
    *f.factory.provider.failure.lock().unwrap() = Some(GitError::RateLimited {
        retry_after_seconds: Some(13),
    });
    assert!(matches!(
        load(&f, ImageSide::After).await,
        Err(FileReadError::Provider(GitError::RateLimited {
            retry_after_seconds: Some(13)
        }))
    ));
    let f = fixture(true, png());
    f.authority.change_at.store(2, Ordering::SeqCst);
    assert!(matches!(
        load(&f, ImageSide::After).await,
        Err(FileReadError::Access(ComparisonReadError::ContextChanged))
    ));
    assert_eq!(f.factory.provider.reads.load(Ordering::SeqCst), 0);
}
