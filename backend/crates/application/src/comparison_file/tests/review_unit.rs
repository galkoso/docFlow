use super::*;
use crate::{
    comparison_file::{discussion::AnchorSide, review_unit::*},
    review_units::{FileRequirement, RequiredReviewUnit, Requirement, UnitSource},
};
use specspace_document_engine::review_sections;

const PREFIX: &str = "---\r\ndocument_id: DOC-ONE\r\ntitle: Source\r\nowner_team: core\r\nschema_version: 1\r\n---\r\n";
fn source(body: &str) -> String {
    format!(
        "{PREFIX}<!-- ss:section:SEC-ONE -->\r\n# שלום 🌍\r\n{body}\r\n<script>protected</script>\r\n![image](assets/a.png)\r\n"
    )
}
struct UnitAuthority {
    context: Mutex<ReviewUnitContext>,
    calls: AtomicUsize,
    fail_at: AtomicUsize,
    change_at: AtomicUsize,
}
#[async_trait]
impl ReviewUnitAuthorization for UnitAuthority {
    async fn resolve_unit(
        &self,
        actor: Uuid,
        proposal: Uuid,
        snapshot: Uuid,
        unit: &str,
    ) -> Result<ReviewUnitContext, FileReadError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let mut c = self.context.lock().unwrap();
        if call == self.fail_at.load(Ordering::SeqCst) {
            return Err(FileReadError::NotFound);
        }
        if call == self.change_at.load(Ordering::SeqCst) {
            c.fingerprint = "c".repeat(64);
        }
        if actor != c.scope.actor
            || proposal != c.scope.proposal
            || snapshot != c.snapshot
            || unit != c.unit.id
        {
            return Err(FileReadError::NotFound);
        }
        Ok(c.clone())
    }
}
fn reidentify(u: &mut RequiredReviewUnit) {
    u.id = format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&(
                "specspace-required-review-unit-v1",
                &u.file_id,
                &u.requirement,
                &u.before,
                &u.after
            ))
            .unwrap()
        )
    );
}
fn setup(f: &Fixture, reason: Option<FileRequirement>) -> (Arc<UnitAuthority>, ReviewUnitReader) {
    let file = f.authority.context.lock().unwrap().clone();
    let sources = f.factory.provider.sources.lock().unwrap();
    let whole = reason.is_some();
    let selection = |text: &str| {
        if whole {
            review_sections::whole(text)
        } else {
            review_sections::prepare(text, "DOC-ONE").unwrap().sections[0]
                .range
                .clone()
        }
    };
    let mut unit = RequiredReviewUnit {
        id: String::new(),
        file_id: file.change.id.clone(),
        requirement: reason.map(|reason| Requirement::File { reason }).unwrap_or(
            Requirement::Section {
                document_id: "DOC-ONE".into(),
                section_id: "SEC-ONE".into(),
            },
        ),
        before: Some(UnitSource {
            source: file.change.before.clone().unwrap(),
            range: Some(selection(&sources.0)),
        }),
        after: Some(UnitSource {
            source: file.change.after.clone().unwrap(),
            range: Some(selection(&sources.1)),
        }),
    };
    reidentify(&mut unit);
    let authorization = Arc::new(UnitAuthority {
        context: Mutex::new(ReviewUnitContext {
            scope: file.scope,
            snapshot: file.snapshot,
            fingerprint: "a".repeat(64),
            head: file.head,
            merge_base: file.merge_base,
            unit,
        }),
        calls: AtomicUsize::new(0),
        fail_at: AtomicUsize::new(0),
        change_at: AtomicUsize::new(0),
    });
    let reader = ReviewUnitReader::new(f.reader.clone(), authorization.clone());
    (authorization, reader)
}
async fn load(
    reader: &ReviewUnitReader,
    authority: &UnitAuthority,
    side: AnchorSide,
    offset: usize,
) -> Result<VerifiedReviewUnitWindow, UnitReadError> {
    let c = authority.context.lock().unwrap().clone();
    reader
        .load(
            c.scope.actor,
            c.scope.proposal,
            c.snapshot,
            UnitWindowRequest {
                unit: &c.unit.id,
                side,
                offset,
            },
        )
        .await
}
#[tokio::test]
async fn exact_windows_reconstruct_large_unicode_crlf_sections_including_protected_source() {
    let before = source("Before");
    let after = source(&"שלום 🌍\r\n".repeat(12000));
    let f = Fixture::new(&before, &after);
    let (a, r) = setup(&f, None);
    let mut offset = 0;
    let mut reconstructed = String::new();
    let mut windows = 0;
    loop {
        let proof = load(&r, &a, AnchorSide::After, offset).await.unwrap();
        assert!(proof.text().len() <= MAX_WINDOW_BYTES);
        assert_eq!(proof.start_byte(), proof.range().start_byte + offset);
        assert_eq!(proof.end_byte() - proof.start_byte(), proof.text().len());
        assert_eq!(
            proof.context().snapshot,
            f.authority.context.lock().unwrap().snapshot
        );
        assert_eq!(proof.commit(), &sha('2'));
        assert_eq!(
            proof.source().source,
            f.authority
                .context
                .lock()
                .unwrap()
                .change
                .after
                .clone()
                .unwrap()
        );
        reconstructed.push_str(proof.text());
        windows += 1;
        match proof.next_offset() {
            Some(next) => {
                assert!(next > offset);
                offset = next;
            }
            None => break,
        }
    }
    assert!(windows > 2);
    assert_eq!(reconstructed, after[PREFIX.len()..]);
    assert!(reconstructed.contains("<script>protected</script>\r\n"));
    let old = load(&r, &a, AnchorSide::Before, 0).await.unwrap();
    assert_eq!(old.text(), &before[PREFIX.len()..]);
    assert_eq!(old.commit(), &sha('1'));
    assert_eq!(f.factory.provider.reads.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn forged_complete_hashes_do_not_turn_truncated_or_retargeted_ranges_into_requirements() {
    for fault in 0..4 {
        let after = source("After");
        let f = Fixture::new(&source("Before"), &after);
        let (a, r) = setup(&f, None);
        {
            let mut c = a.context.lock().unwrap();
            match fault {
                0 => {
                    let span = c.unit.after.as_mut().unwrap().range.as_mut().unwrap();
                    span.end_byte -= 4;
                    span.sha256 = format!(
                        "{:x}",
                        Sha256::digest(&after.as_bytes()[span.start_byte..span.end_byte])
                    );
                }
                1 => {
                    c.unit.requirement = Requirement::Section {
                        document_id: "DOC-ONE".into(),
                        section_id: "SEC-OTHER".into(),
                    }
                }
                2 => c.unit.after.as_mut().unwrap().source.path = "docs/elsewhere.md".into(),
                _ => c.unit.before = None,
            }
            reidentify(&mut c.unit);
        }
        assert!(matches!(
            load(&r, &a, AnchorSide::After, 0).await,
            Err(UnitReadError::Source(FileReadError::InvalidEvidence))
        ));
    }
}
#[tokio::test]
async fn whole_file_fallback_retains_front_matter_and_rejects_utf8_or_out_of_range_windows() {
    for reason in [
        FileRequirement::ManifestLimit,
        FileRequirement::Unstructured,
    ] {
        let text = if reason == FileRequirement::Unstructured {
            "malformed שלום 🌍\r\n".into()
        } else {
            source("שלום")
        };
        let f = Fixture::new("before unstructured", &text);
        let (a, r) = setup(&f, Some(reason));
        let proof = load(&r, &a, AnchorSide::After, 0).await.unwrap();
        assert_eq!(proof.text(), text);
        assert_eq!(proof.start_byte(), 0);
        let bad = text.find('ש').unwrap() + 1;
        assert!(matches!(
            load(&r, &a, AnchorSide::After, bad).await,
            Err(UnitReadError::InvalidWindow)
        ));
        assert!(matches!(
            load(&r, &a, AnchorSide::After, text.len() + 1).await,
            Err(UnitReadError::InvalidWindow)
        ));
        let end = load(&r, &a, AnchorSide::After, text.len()).await.unwrap();
        assert!(end.text().is_empty());
        assert_eq!(end.next_offset(), None);
    }
}
#[tokio::test]
async fn denial_missing_sides_binary_ranges_and_invalid_requests_never_connect() {
    for fault in 0..4 {
        let f = Fixture::new(&source("Before"), &source("After"));
        let (a, r) = setup(&f, None);
        match fault {
            0 => {
                a.fail_at.store(1, Ordering::SeqCst);
            }
            1 => a.context.lock().unwrap().unit.after = None,
            2 => a.context.lock().unwrap().unit.after.as_mut().unwrap().range = None,
            _ => {}
        }
        let result = load(
            &r,
            &a,
            AnchorSide::After,
            if fault == 3 {
                diff::MAX_FILE_BYTES + 1
            } else {
                0
            },
        )
        .await;
        assert!(result.is_err());
        assert_eq!(f.factory.connects.load(Ordering::SeqCst), 0);
    }
}
#[tokio::test]
async fn unit_authority_changes_before_or_after_cpu_work_and_later_rechecks_reject_proofs() {
    for at in [2, 3] {
        let f = Fixture::new(&source("Before"), &source("After"));
        let (a, r) = setup(&f, None);
        a.change_at.store(at, Ordering::SeqCst);
        assert!(matches!(
            load(&r, &a, AnchorSide::After, 0).await,
            Err(UnitReadError::Source(FileReadError::Access(
                ComparisonReadError::ContextChanged
            )))
        ));
    }
    let f = Fixture::new(&source("Before"), &source("After"));
    let (a, r) = setup(&f, None);
    let proof = load(&r, &a, AnchorSide::After, 0).await.unwrap();
    let actor = proof.context().scope.actor;
    assert!(r.recheck(Uuid::new_v4(), &proof).await.is_err());
    f.authority.fail_at.store(
        f.authority.calls.load(Ordering::SeqCst) + 1,
        Ordering::SeqCst,
    );
    assert!(r.recheck(actor, &proof).await.is_err());
    a.context.lock().unwrap().fingerprint = "d".repeat(64);
    assert!(r.recheck(actor, &proof).await.is_err());
}

#[tokio::test]
async fn source_windows_share_capacity_and_cancel_stalled_io_at_the_reader_deadline() {
    let mut f = Fixture::new(&source("Before"), &source("After"));
    Arc::get_mut(&mut f.reader).unwrap().timeout = Duration::from_millis(50);
    let (a, r) = setup(&f, None);
    let permits = f.reader.loads.clone().try_acquire_many_owned(2).unwrap();
    assert!(matches!(
        load(&r, &a, AnchorSide::After, 0).await,
        Err(UnitReadError::Source(FileReadError::Busy))
    ));
    assert_eq!(f.factory.connects.load(Ordering::SeqCst), 0);
    drop(permits);
    f.factory.provider.block.store(true, Ordering::SeqCst);
    assert!(matches!(
        load(&r, &a, AnchorSide::After, 0).await,
        Err(UnitReadError::Source(FileReadError::TimedOut))
    ));
    assert_eq!(f.reader.loads.available_permits(), 2);
    f.factory.provider.block.store(false, Ordering::SeqCst);
    let proof = load(&r, &a, AnchorSide::After, 0).await.unwrap();
    assert!(proof.text().contains("After"));
}
