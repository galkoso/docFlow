use super::*;
use crate::{
    comparison_file::{
        book::*,
        discussion::{AnchorReadError, TextSelection},
    },
    review_snapshot::ReviewBookDocument,
};
use specspace_document_engine::discussion_anchor::{AnchorError, Selection};

const SOURCE: &str = "---\r\ndocument_id: DOC-ONE\r\ntitle: Original\r\nowner_team: core\r\nschema_version: 1\r\n---\r\n<!-- ss:section:SEC-ONE -->\r\n# שלום 🌍\r\n\r\nOriginal unchanged text\r\n![original](assets/a.png)\r\n";
struct BookAuthority {
    context: Mutex<BookDocumentContext>,
    calls: AtomicUsize,
    fail_at: AtomicUsize,
    change_at: AtomicUsize,
}
#[async_trait]
impl ReviewBookAuthorization for BookAuthority {
    async fn resolve_document(
        &self,
        actor: Uuid,
        proposal: Uuid,
        snapshot: Uuid,
        entry: &str,
    ) -> Result<BookDocumentContext, FileReadError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let mut context = self.context.lock().unwrap();
        if call == self.fail_at.load(Ordering::SeqCst) {
            return Err(FileReadError::NotFound);
        }
        if call == self.change_at.load(Ordering::SeqCst) {
            context.scope.version += 1;
        }
        if context.scope.actor != actor
            || context.scope.proposal != proposal
            || context.snapshot != snapshot
            || context.document.id != entry
        {
            return Err(FileReadError::NotFound);
        }
        Ok(context.clone())
    }
}
fn book(f: &Fixture) -> (Arc<BookAuthority>, Arc<ReviewBookReader>) {
    let context = f.authority.context.lock().unwrap().clone();
    let source = context.change.after.as_ref().unwrap().clone();
    let authority = Arc::new(BookAuthority {
        context: Mutex::new(BookDocumentContext {
            scope: context.scope.clone(),
            snapshot: context.snapshot,
            fingerprint: "a".repeat(64),
            head: context.head.clone(),
            document: ReviewBookDocument {
                id: ReviewBookDocument::identity(
                    context.scope.connection.repository,
                    &context.head,
                    &source,
                )
                .unwrap(),
                source,
            },
        }),
        calls: AtomicUsize::new(0),
        fail_at: AtomicUsize::new(0),
        change_at: AtomicUsize::new(0),
    });
    let reader = Arc::new(f.reader.with_book_authorization(authority.clone()));
    (authority, reader)
}
async fn load(
    authority: &BookAuthority,
    reader: &ReviewBookReader,
    selection: TextSelection,
) -> Result<VerifiedBookAnchor, AnchorReadError> {
    let c = authority.context.lock().unwrap().clone();
    reader
        .load_anchor(
            c.scope.actor,
            c.scope.proposal,
            c.snapshot,
            &c.document.id,
            selection,
        )
        .await
}
#[tokio::test]
async fn unchanged_document_and_sections_use_exact_historical_source_without_diff_authority() {
    let f = Fixture::new(SOURCE, SOURCE);
    let (authority, reader) = book(&f);
    let proof = load(&authority, &reader, TextSelection::Document {})
        .await
        .unwrap();
    assert_eq!(proof.text().quote(), SOURCE);
    assert_eq!(proof.source().path, "docs/new.md");
    assert_eq!(proof.commit(), &sha('2'));
    assert_ne!(proof.commit(), &proof.context().scope.recorded_head);
    assert_eq!(
        proof.text().source_sha256(),
        proof.source().sha256.as_deref().unwrap()
    );
    let section = load(
        &authority,
        &reader,
        TextSelection::Section {
            section_id: "SEC-ONE".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        section.text().selection(),
        &Selection::Section {
            document_id: "DOC-ONE".into(),
            section_id: "SEC-ONE".into()
        }
    );
    assert!(section.text().quote().contains("# שלום 🌍\r\n"));
    assert!(section.text().quote().contains("![original]"));
    assert_eq!(f.authority.calls.load(Ordering::SeqCst), 0);
    assert_eq!(f.factory.provider.reads.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn selections_are_bounded_before_authorization_or_provider_io() {
    let f = Fixture::new(SOURCE, SOURCE);
    let (authority, reader) = book(&f);
    for selection in [
        TextSelection::Section {
            section_id: "../private".into(),
        },
        TextSelection::Lines {
            start_line: 0,
            end_line: 1,
        },
        TextSelection::Lines {
            start_line: 1,
            end_line: 501,
        },
    ] {
        assert!(load(&authority, &reader, selection).await.is_err());
    }
    assert_eq!(authority.calls.load(Ordering::SeqCst), 0);
    assert_eq!(f.factory.connects.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn invalid_manifest_descriptors_or_identity_never_reach_source_io() {
    for fault in 0..8 {
        let f = Fixture::new(SOURCE, SOURCE);
        let (authority, reader) = book(&f);
        {
            let mut c = authority.context.lock().unwrap();
            match fault {
                0 => c.document.id = "f".repeat(64),
                1 => c.document.source.path = "docs/../secret.md".into(),
                2 => c.document.source.executable = true,
                3 => c.document.source.catalog_position = None,
                4 => c.document.source.sha256 = None,
                5 => c.document.source.blob_oid = "not-a-blob".into(),
                6 => c.fingerprint = "invalid".into(),
                _ => {
                    c.document.source.bytes = 2 * 1024 * 1024 + 1;
                    c.document.id = ReviewBookDocument::identity(
                        c.scope.connection.repository,
                        &c.head,
                        &c.document.source,
                    )
                    .unwrap();
                }
            }
        }
        assert!(
            load(&authority, &reader, TextSelection::Document {})
                .await
                .is_err()
        );
        assert_eq!(
            f.factory.connects.load(Ordering::SeqCst),
            0,
            "fault {fault}"
        );
    }
}
#[tokio::test]
async fn provider_bytes_and_front_matter_must_match_even_for_line_selection() {
    let f = Fixture::new(SOURCE, SOURCE);
    let (authority, reader) = book(&f);
    f.factory.provider.sources.lock().unwrap().1 = SOURCE.replace("Original", "Altered!");
    assert!(matches!(
        load(&authority, &reader, TextSelection::Document {}).await,
        Err(AnchorReadError::Source(FileReadError::InvalidEvidence))
    ));
    for selection in [
        TextSelection::Document {},
        TextSelection::Section {
            section_id: "SEC-ONE".into(),
        },
        TextSelection::Lines {
            start_line: 1,
            end_line: 1,
        },
    ] {
        let f = Fixture::new(SOURCE, SOURCE);
        let (authority, reader) = book(&f);
        {
            let mut c = authority.context.lock().unwrap();
            c.document.source.document_id = Some("DOC-OTHER".into());
            c.document.id = ReviewBookDocument::identity(
                c.scope.connection.repository,
                &c.head,
                &c.document.source,
            )
            .unwrap();
        }
        assert!(matches!(
            load(&authority, &reader, selection).await,
            Err(AnchorReadError::Selection(AnchorError::DocumentMismatch))
        ));
    }
}
#[tokio::test]
async fn git_blob_identity_sha256_and_byte_count_are_independently_required() {
    for fault in 0..3 {
        let f = Fixture::new(SOURCE, SOURCE);
        let (authority, reader) = book(&f);
        {
            let mut c = authority.context.lock().unwrap();
            match fault {
                0 => c.document.source.blob_oid = "0".repeat(40),
                1 => c.document.source.sha256 = Some("0".repeat(64)),
                _ => c.document.source.bytes += 1,
            }
            c.document.id = ReviewBookDocument::identity(
                c.scope.connection.repository,
                &c.head,
                &c.document.source,
            )
            .unwrap();
        }
        assert!(matches!(
            load(&authority, &reader, TextSelection::Document {}).await,
            Err(AnchorReadError::Source(FileReadError::InvalidEvidence))
        ));
        assert_eq!(f.factory.provider.reads.load(Ordering::SeqCst), 1);
    }
}
#[tokio::test]
async fn access_loss_or_context_change_at_every_boundary_discards_source_evidence() {
    for boundary in 1..=4 {
        for changed in [false, true] {
            let f = Fixture::new(SOURCE, SOURCE);
            let (authority, reader) = book(&f);
            if changed {
                if boundary == 1 {
                    continue;
                }
                authority.change_at.store(boundary, Ordering::SeqCst);
            } else {
                authority.fail_at.store(boundary, Ordering::SeqCst);
            }
            assert!(
                load(&authority, &reader, TextSelection::Document {})
                    .await
                    .is_err()
            );
        }
    }
}
#[tokio::test]
async fn later_rechecks_do_not_adopt_another_actor_or_changed_snapshot() {
    let f = Fixture::new(SOURCE, SOURCE);
    let (authority, reader) = book(&f);
    let proof = load(
        &authority,
        &reader,
        TextSelection::Lines {
            start_line: 7,
            end_line: 8,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        proof.text().quote(),
        "<!-- ss:section:SEC-ONE -->\r\n# שלום 🌍\r\n"
    );
    assert_eq!(
        reader.recheck(Uuid::new_v4(), &proof).await,
        Err(FileReadError::NotFound)
    );
    authority.context.lock().unwrap().fingerprint = "b".repeat(64);
    assert_eq!(
        reader.recheck(proof.context().scope.actor, &proof).await,
        Err(FileReadError::Access(ComparisonReadError::ContextChanged))
    );
    assert_eq!(proof.context().fingerprint, "a".repeat(64));
}
#[tokio::test]
async fn provider_delays_and_cancellation_share_capacity_with_file_reads() {
    let f = Fixture::new(SOURCE, SOURCE);
    let (authority, reader) = book(&f);
    f.factory.provider.block.store(true, Ordering::SeqCst);
    let mut tasks = vec![];
    for _ in 0..2 {
        let a = authority.clone();
        let r = reader.clone();
        tasks.push(tokio::spawn(async move {
            load(&a, &r, TextSelection::Document {}).await
        }));
    }
    tokio::time::timeout(Duration::from_secs(1), async {
        while f.factory.provider.reads.load(Ordering::SeqCst) < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(matches!(
        load(&authority, &reader, TextSelection::Document {}).await,
        Err(AnchorReadError::Source(FileReadError::Busy))
    ));
    let c = f.authority.context.lock().unwrap().clone();
    assert!(matches!(
        f.reader
            .load(c.scope.actor, c.scope.proposal, c.snapshot, &c.change.id)
            .await,
        Err(FileReadError::Busy)
    ));
    for task in tasks {
        task.abort();
        assert!(matches!(task.await, Err(error) if error.is_cancelled()));
    }
    assert_eq!(f.reader.loads.available_permits(), 2);
}
#[tokio::test]
async fn deadline_returns_no_proof_and_releases_capacity_and_provider_backoff_stays_explicit() {
    let f = Fixture::new(SOURCE, SOURCE);
    let (authority, _) = book(&f);
    let mut base = f.reader.with_authorization(f.authority.clone());
    base.timeout = Duration::from_millis(10);
    let reader = base.with_book_authorization(authority.clone());
    f.factory.provider.block.store(true, Ordering::SeqCst);
    assert!(matches!(
        load(&authority, &reader, TextSelection::Document {}).await,
        Err(AnchorReadError::Source(FileReadError::TimedOut))
    ));
    assert_eq!(f.reader.loads.available_permits(), 2);
    f.factory.provider.block.store(false, Ordering::SeqCst);
    *f.factory.provider.failure.lock().unwrap() = Some(GitError::RateLimited {
        retry_after_seconds: Some(7),
    });
    assert!(matches!(
        load(&authority, &reader, TextSelection::Document {}).await,
        Err(AnchorReadError::Source(FileReadError::Provider(
            GitError::RateLimited {
                retry_after_seconds: Some(7)
            }
        )))
    ));
}
