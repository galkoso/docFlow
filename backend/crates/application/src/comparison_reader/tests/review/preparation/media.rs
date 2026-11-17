use super::*;
use sha2::{Digest, Sha256};
use specspace_document_engine::media::MAX_ASSET_BYTES;
fn image() -> Vec<u8> {
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"),"/../../../fixtures/repository/assets/5e3d382db4dd83d59aa5742793ad6b7903409e865c83bcbc54835049f043bc15.png")).to_vec()
}
fn path(data: &[u8], extension: &str) -> String {
    format!("assets/{:x}.{extension}", Sha256::digest(data))
}
fn image_fixture() -> Fixture {
    let bytes = image();
    let path = path(&bytes, "png");
    fixture_asset(&format!("![Descriptive alt]({path})"), Some((path, bytes)))
}
async fn candidate(
    f: &Fixture,
    preparer: &ReviewPreparer,
    expected: &ReviewExpectation,
) -> crate::review_preparation::ReviewCandidate {
    let before = f.provider.binary_reads.lock().unwrap().len();
    let loaded = preparer
        .prepare(f.actor, f.proposal, expected.clone())
        .await
        .unwrap();
    assert_eq!(f.provider.binary_reads.lock().unwrap().len(), before);
    f.provider.binary_allowed.store(true, Ordering::SeqCst);
    loaded
}

#[tokio::test]
async fn complete_scan_deduplicates_references_verifies_unreferenced_additions_and_returns_only_metadata()
 {
    let first = image();
    let mut second = first.clone();
    second.extend_from_slice(b"synthetic-source-metadata");
    let first_path = path(&first, "png");
    let second_path = path(&second, "png");
    let body = format!("![First]({first_path})\n\n![Reuse]({first_path})");
    let f = fixture_assets(
        &body,
        vec![
            (first_path.clone(), first.clone()),
            (second_path.clone(), second.clone()),
        ],
    );
    let (_, preparer, expected) = prepared_fixture(&f);
    let loaded = candidate(&f, &preparer, &expected).await;
    assert_eq!(loaded.plan().required_media().len(), 2);
    let verified = preparer.verify_media(loaded).await.unwrap();
    assert_eq!(verified.media().len(), 2);
    assert_eq!(verified.media_validation_version(), 1);
    assert_eq!(verified.fingerprint().len(), 64);
    assert_eq!(verified.evidence_digest().len(), 64);
    assert_ne!(verified.fingerprint(), verified.evidence_digest());
    let receipt = verified
        .media()
        .iter()
        .find(|receipt| receipt.path() == first_path)
        .unwrap();
    assert_eq!(receipt.blob_oid(), blob::oid(&first, 40).unwrap().as_str());
    assert_eq!(receipt.bytes(), first.len() as u64);
    assert_eq!(
        receipt.source_sha256(),
        format!("{:x}", Sha256::digest(&first))
    );
    assert_eq!(receipt.display_dimensions(), (1, 1));
    assert_eq!(
        verified.media()[0].display_sha256(),
        verified.media()[1].display_sha256()
    );
    let json = serde_json::to_string(verified.media()).unwrap();
    assert!(!json.contains("synthetic-source-metadata"));
    assert!(!json.contains("\"png\":") && !json.contains("\"data\":"));
    let reads = f.provider.binary_reads.lock().unwrap();
    assert_eq!(reads.len(), 2);
    assert!(
        reads
            .iter()
            .all(|(commit, _, limit)| commit == HEAD && *limit == MAX_ASSET_BYTES)
    );
    assert!(reads.iter().any(|(_, path, _)| *path == second_path));
}

#[tokio::test]
async fn a_text_only_candidate_gets_empty_media_evidence_without_reading_opaque_files() {
    let f = fixture();
    let (_, preparer, expected) = prepared_fixture(&f);
    let verified = preparer
        .verify_media(candidate(&f, &preparer, &expected).await)
        .await
        .unwrap();
    assert!(verified.media().is_empty());
    assert_eq!(verified.fingerprint().len(), 64);
    assert!(f.provider.binary_reads.lock().unwrap().is_empty());
}

#[tokio::test]
async fn decoded_images_keep_missing_alt_findings_for_later_review_and_merge_gates() {
    let bytes = image();
    let path = path(&bytes, "png");
    let f = fixture_asset(&format!("![]({path})"), Some((path, bytes)));
    let (_, preparer, expected) = prepared_fixture(&f);
    let verified = preparer
        .verify_media(candidate(&f, &preparer, &expected).await)
        .await
        .unwrap();
    assert_eq!(verified.media().len(), 1);
    assert!(
        verified
            .candidate()
            .plan()
            .validation()
            .findings
            .iter()
            .any(|finding| finding.code == "IMAGE_ALT_REQUIRED")
    );
    assert!(verified.candidate().plan().summary().content_findings_total > 0);
}

#[tokio::test]
async fn forged_bytes_truncation_extra_bytes_and_excessive_provider_responses_never_get_receipts() {
    for mode in 0..4 {
        let f = image_fixture();
        let (_, preparer, expected) = prepared_fixture(&f);
        let loaded = candidate(&f, &preparer, &expected).await;
        let mut forged = image();
        match mode {
            0 => {
                forged[0] ^= 1;
            }
            1 => {
                forged.pop();
            }
            2 => forged.push(0),
            _ => forged = vec![0; MAX_ASSET_BYTES + 1],
        }
        *f.provider.binary_override.lock().unwrap() = Some(Ok(forged));
        assert!(matches!(
            preparer.verify_media(loaded).await,
            Err(ReviewPreparationError::InvalidMedia {
                code: "REVIEW_MEDIA_BLOB_MISMATCH",
                ..
            })
        ));
        assert_eq!(f.provider.binary_reads.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn object_identity_alone_cannot_bypass_content_addressing_mime_or_decoder_checks() {
    for mode in 0..3 {
        let (bytes, path, error) = match mode {
            0 => (
                image(),
                format!("assets/{}.png", "a".repeat(64)),
                "ASSET_HASH_MISMATCH",
            ),
            1 => {
                let bytes = image();
                let path = path(&bytes, "jpg");
                (bytes, path, "UNSUPPORTED_MEDIA_TYPE")
            }
            _ => {
                let bytes = b"\x89PNG\r\n\x1a\ninvalid-image".to_vec();
                let path = path(&bytes, "png");
                (bytes, path, "INVALID_IMAGE")
            }
        };
        let f = fixture_asset("Changed book", Some((path, bytes)));
        let (_, preparer, expected) = prepared_fixture(&f);
        let loaded = candidate(&f, &preparer, &expected).await;
        let result = preparer.verify_media(loaded).await;
        assert!(
            matches!(result,Err(ReviewPreparationError::InvalidMedia {code:actual,path:bad_path}) if actual==error && bad_path==f.provider.binary_reads.lock().unwrap()[0].1),
            "mode {mode}"
        );
    }
}

#[tokio::test]
async fn revoked_authority_at_every_async_boundary_discards_the_scan() {
    // Five scope reads prepare the candidate. The scan checks at start, after
    // connection/refs, after bytes/CPU, and after final connection/refs.
    for boundary in 6..=12 {
        let f = image_fixture();
        let (auth, preparer, expected) = prepared_fixture(&f);
        let loaded = candidate(&f, &preparer, &expected).await;
        let scope = auth.scope.clone();
        *auth.hook.lock().unwrap() = Some((
            boundary,
            Box::new(move || {
                scope.lock().unwrap().review.reviewer_memberships[0].active = false;
            }),
        ));
        assert!(
            matches!(
                preparer.verify_media(loaded).await,
                Err(ReviewPreparationError::ContextChanged)
            ),
            "boundary {boundary}"
        );
        assert!(f.provider.binary_reads.lock().unwrap().len() <= 1);
    }
}

#[tokio::test]
async fn evidence_and_identity_are_shared_across_actors_and_state_only_changes_but_bind_policy() {
    let f = image_fixture();
    let (auth, preparer, expected) = prepared_fixture(&f);
    let first = preparer
        .verify_media(candidate(&f, &preparer, &expected).await)
        .await
        .unwrap();
    let actor = Uuid::new_v4();
    {
        let mut read = f.auth.context.lock().unwrap();
        read.actor = actor;
        read.version = 4;
        read.state = "IN_REVIEW".into();
        let mut scope = auth.scope.lock().unwrap();
        scope.comparison = read.clone();
        scope.review.actor.user_id = actor;
        scope.review.expected_version = 4;
    }
    let mut next = expected;
    next.proposal_version = 4;
    let loaded = preparer
        .prepare(actor, f.proposal, next.clone())
        .await
        .unwrap();
    let again = preparer.verify_media(loaded).await.unwrap();
    assert_eq!(first.fingerprint(), again.fingerprint());
    assert_eq!(first.evidence_digest(), again.evidence_digest());
    next.policy_version = 2;
    auth.scope.lock().unwrap().review.policy_version = 2;
    let loaded = preparer.prepare(actor, f.proposal, next).await.unwrap();
    let updated = preparer.verify_media(loaded).await.unwrap();
    assert_ne!(first.fingerprint(), updated.fingerprint());
    assert_ne!(first.evidence_digest(), updated.evidence_digest());
}

#[tokio::test]
async fn git_ref_changes_during_the_scan_prevent_complete_evidence_publication() {
    for target in [false, true] {
        let f = image_fixture();
        let (_, preparer, expected) = prepared_fixture(&f);
        let loaded = candidate(&f, &preparer, &expected).await;
        let provider = f.provider.clone();
        *f.provider.after_bytes.lock().unwrap() = Some(Box::new(move || {
            if target {
                *provider.target.lock().unwrap() = NEXT.into();
            } else {
                *provider.branch.lock().unwrap() = Some(LATER.into());
            }
        }));
        let error = preparer.verify_media(loaded).await.err().unwrap();
        assert!(if target {
            matches!(error, ReviewPreparationError::TargetChanged)
        } else {
            matches!(error, ReviewPreparationError::HeadChanged)
        });
    }
}

#[tokio::test]
async fn provider_failures_keep_backoff_without_retry_or_partial_evidence() {
    for error in [
        GitError::RateLimited {
            retry_after_seconds: Some(41),
        },
        GitError::FileMissing,
    ] {
        let f = image_fixture();
        let (_, preparer, expected) = prepared_fixture(&f);
        let loaded = candidate(&f, &preparer, &expected).await;
        *f.provider.binary_override.lock().unwrap() = Some(Err(error.clone()));
        assert!(
            matches!(preparer.verify_media(loaded).await,Err(ReviewPreparationError::Provider(actual)) if actual==error)
        );
        assert_eq!(f.provider.binary_reads.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn revoked_access_suppresses_decoder_errors_after_cpu_processing() {
    let bytes = b"\x89PNG\r\n\x1a\ninvalid-image".to_vec();
    let path = path(&bytes, "png");
    let f = fixture_asset("Changed book", Some((path, bytes)));
    let (auth, preparer, expected) = prepared_fixture(&f);
    let loaded = candidate(&f, &preparer, &expected).await;
    let scope = auth.scope.clone();
    *auth.hook.lock().unwrap() = Some((
        10,
        Box::new(move || {
            scope.lock().unwrap().review.assigned_editor = false;
        }),
    ));
    assert!(matches!(
        preparer.verify_media(loaded).await,
        Err(ReviewPreparationError::ContextChanged)
    ));
}

#[tokio::test]
async fn a_timed_out_binary_read_does_not_retry_and_releases_capacity_for_fresh_work() {
    let f = image_fixture();
    let (auth, _, expected) = prepared_fixture(&f);
    let preparer = Arc::new(
        ReviewPreparer::new(
            auth,
            f.reader.clone(),
            f.factory.clone(),
            Duration::from_secs(30),
        )
        .unwrap(),
    );
    let loaded = candidate(&f, &preparer, &expected).await;
    f.provider.binary_blocked.store(true, Ordering::SeqCst);
    tokio::time::pause();
    let worker = preparer.clone();
    let task = tokio::spawn(async move { worker.verify_media(loaded).await });
    f.provider.binary_started.notified().await;
    tokio::time::advance(Duration::from_secs(31)).await;
    assert!(matches!(
        task.await.unwrap(),
        Err(ReviewPreparationError::TimedOut)
    ));
    assert_eq!(f.provider.binary_reads.lock().unwrap().len(), 1);
    tokio::time::resume();
    f.provider.binary_blocked.store(false, Ordering::SeqCst);
    let loaded = candidate(&f, &preparer, &expected).await;
    assert!(preparer.verify_media(loaded).await.is_ok());
    assert_eq!(f.provider.binary_reads.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn declared_oversized_media_is_rejected_before_any_binary_read() {
    let bytes = vec![0; MAX_ASSET_BYTES + 1];
    let path = path(&bytes, "png");
    let f = fixture_asset("Changed book", Some((path, bytes)));
    let (_, preparer, expected) = prepared_fixture(&f);
    assert!(
        matches!(preparer.prepare(f.actor,f.proposal,expected).await,
        Err(ReviewPreparationError::Plan(error)) if error.code()=="REVIEW_MEDIA_UNSUPPORTED")
    );
    assert!(f.provider.binary_reads.lock().unwrap().is_empty());
}
