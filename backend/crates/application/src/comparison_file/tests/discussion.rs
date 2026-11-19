use super::*;
use crate::comparison_file::discussion::{
    AnchorReadError, AnchorSide, TextSelection, VerifiedFileAnchor,
};
use specspace_document_engine::discussion_anchor::AnchorError;

const PREFIX: &str =
    "---\ndocument_id: DOC-ONE\ntitle: title\nowner_team: core\nschema_version: 1\n---\n";

async fn anchor(
    f: &Fixture,
    side: AnchorSide,
    selection: TextSelection,
) -> Result<VerifiedFileAnchor, AnchorReadError> {
    let context = f.authority.context.lock().unwrap().clone();
    f.reader
        .load_discussion_anchor(
            context.scope.actor,
            context.scope.proposal,
            context.snapshot,
            &context.change.id,
            side,
            selection,
        )
        .await
}

#[tokio::test]
async fn historical_renamed_sides_keep_their_own_refs_paths_and_verified_hashes() {
    let before = format!("{PREFIX}<!-- ss:section:SEC-A -->\n# לפני\n\nold text\n");
    let after = format!("{PREFIX}<!-- ss:section:SEC-A -->\n# אחרי\n\n![image](assets/a.png)\n");
    let f = Fixture::new(&before, &after);
    let old = anchor(&f, AnchorSide::Before, TextSelection::Document {})
        .await
        .unwrap();
    let new = anchor(
        &f,
        AnchorSide::After,
        TextSelection::Section {
            section_id: "SEC-A".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(old.commit(), &sha('1'));
    assert_eq!(new.commit(), &sha('2'));
    assert_ne!(new.commit(), &new.context().scope.recorded_head);
    assert_eq!(old.source().path, "docs/old.md");
    assert_eq!(new.source().path, "docs/new.md");
    assert_eq!(old.text().quote(), before);
    assert!(new.text().quote().contains("![image]"));
    assert_eq!(
        new.text().source_sha256(),
        new.source().sha256.as_deref().unwrap()
    );
    assert_eq!(old.context().snapshot, new.context().snapshot);
    assert_eq!(f.factory.provider.reads.load(Ordering::SeqCst), 2);
    assert_eq!(f.factory.connects.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn absent_and_empty_sides_never_borrow_the_opposite_source() {
    for absent in [false, true] {
        let f = Fixture::new("", "after");
        if absent {
            f.authority.context.lock().unwrap().change.before = None;
        }
        let result = anchor(
            &f,
            AnchorSide::Before,
            TextSelection::Lines {
                start_line: 1,
                end_line: 1,
            },
        )
        .await;
        assert!(matches!(
            (absent, result),
            (true, Err(AnchorReadError::Source(FileReadError::NotFound)))
                | (
                    false,
                    Err(AnchorReadError::Selection(AnchorError::InvalidLines))
                )
        ));
    }
}

#[tokio::test]
async fn provider_tampering_and_manifest_document_mismatch_cannot_create_evidence() {
    let source = format!("{PREFIX}<!-- ss:section:SEC-A -->\n# title\n");
    let f = Fixture::new(&source, &source);
    f.factory.provider.sources.lock().unwrap().1 = source.replace("title", "forged");
    assert!(matches!(
        anchor(&f, AnchorSide::After, TextSelection::Document {}).await,
        Err(AnchorReadError::Source(FileReadError::InvalidEvidence))
    ));
    let f = Fixture::new(&source, &source);
    f.authority
        .context
        .lock()
        .unwrap()
        .change
        .after
        .as_mut()
        .unwrap()
        .document_id = Some("DOC-OTHER".into());
    assert!(matches!(
        anchor(&f, AnchorSide::After, TextSelection::Document {}).await,
        Err(AnchorReadError::Selection(AnchorError::DocumentMismatch))
    ));
}

#[tokio::test]
async fn malformed_selections_and_forged_browser_evidence_fail_before_source_io() {
    let f = Fixture::new("before", "after");
    for selection in [
        TextSelection::Section {
            section_id: "../secret".into(),
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
        assert!(anchor(&f, AnchorSide::After, selection).await.is_err());
    }
    assert_eq!(f.authority.calls.load(Ordering::SeqCst), 0);
    assert_eq!(f.factory.provider.reads.load(Ordering::SeqCst), 0);
    for value in [
        serde_json::json!({"kind":"document","path":"docs/private.md"}),
        serde_json::json!({"kind":"lines","start_line":1,"end_line":1,"quote":"forged"}),
        serde_json::json!({"kind":"section","section_id":"SEC-A","sha256":"0".repeat(64)}),
    ] {
        assert!(serde_json::from_value::<TextSelection>(value).is_err());
    }
}

#[tokio::test]
async fn revocation_or_scope_change_around_anchor_computation_discards_the_proof() {
    // Cold file loads have five authorization boundaries; selection adds one
    // before CPU work and one after it, including when no provider call is made.
    for boundary in [6, 7] {
        for change in [false, true] {
            let f = Fixture::new("before", "after");
            if change {
                f.authority.change_at.store(boundary, Ordering::SeqCst);
            } else {
                f.authority.fail_at.store(boundary, Ordering::SeqCst);
            }
            assert!(
                anchor(
                    &f,
                    AnchorSide::After,
                    TextSelection::Lines {
                        start_line: 1,
                        end_line: 1
                    }
                )
                .await
                .is_err()
            );
        }
    }
}

#[tokio::test]
async fn later_rechecks_reject_another_actor_or_changed_scope_without_relocating() {
    let f = Fixture::new("before", "after");
    let proof = anchor(
        &f,
        AnchorSide::After,
        TextSelection::Lines {
            start_line: 1,
            end_line: 1,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        f.reader
            .recheck_discussion_anchor(Uuid::new_v4(), &proof)
            .await,
        Err(FileReadError::NotFound)
    );
    f.authority.context.lock().unwrap().scope.version += 1;
    assert_eq!(
        f.reader
            .recheck_discussion_anchor(proof.context().scope.actor, &proof)
            .await,
        Err(FileReadError::Access(ComparisonReadError::ContextChanged))
    );
    assert_eq!(proof.text().quote(), "after");
    assert_eq!(proof.commit(), &sha('2'));
}
