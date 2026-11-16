use super::*;
use specspace_document_engine::catalog::CatalogEntry;

const HEAD: &str = "1111111111111111111111111111111111111111";
fn source(id: &str, section: &str, text: &str) -> String {
    format!(
        "---\ndocument_id: {id}\ntitle: Test\nowner_team: test\nschema_version: 1\nunknown: keep-exact\n---\n\n<!-- ss:section:{section} -->\n## Heading\n\n{text}\n"
    )
}
fn snapshot(repository: Uuid) -> SaveSnapshot {
    SaveSnapshot {
        repository_id: repository,
        commit: CommitSha::parse(HEAD).unwrap(),
        catalog: Catalog {
            schema_version: 1,
            documents: vec![
                CatalogEntry {
                    document_id: "DOC-A".into(),
                    path: "docs/a.md".into(),
                },
                CatalogEntry {
                    document_id: "DOC-B".into(),
                    path: "docs/b.md".into(),
                },
            ],
        },
        files: vec![
            SourceFile {
                path: "docs/a.md".into(),
                markdown: source("DOC-A", "SEC-A", "first"),
            },
            SourceFile {
                path: "docs/b.md".into(),
                markdown: source("DOC-B", "SEC-B", "second"),
            },
        ],
        asset_paths: HashSet::new(),
    }
}
fn request(snapshot: &SaveSnapshot) -> SaveRequest {
    SaveRequest {
        expected_head_sha: HEAD.into(),
        expected_proposal_version: 12,
        message: "Update proposal".into(),
        documents: snapshot
            .catalog
            .documents
            .iter()
            .zip(&snapshot.files)
            .map(|(entry, file)| DocumentSave {
                document_id: entry.document_id.clone(),
                expected_document_sha256: hash(file.markdown.as_bytes()),
                markdown: file.markdown.clone(),
            })
            .collect(),
    }
}
fn intent(repository: Uuid, request: SaveRequest) -> ValidatedSave {
    ValidatedSave::new(Uuid::new_v4(), Uuid::new_v4(), repository, request).unwrap()
}

#[test]
fn all_changed_documents_share_one_plan_while_untouched_source_is_never_rewritten() {
    let repo = Uuid::new_v4();
    let mut base = snapshot(repo);
    let mut body = request(&base);
    base.catalog.documents.push(CatalogEntry {
        document_id: "DOC-C".into(),
        path: "docs/c.md".into(),
    });
    base.files.push(SourceFile {
        path: "docs/c.md".into(),
        markdown: source("DOC-C", "SEC-C", "Cafe\u{301} untouched"),
    });
    body.documents[0].markdown = body.documents[0]
        .markdown
        .replace("## Heading", "## Renamed");
    body.documents[1].markdown = body.documents[1]
        .markdown
        .replace("second", "second **edited**");
    let expected = body
        .documents
        .iter()
        .map(|change| change.markdown.clone())
        .collect::<Vec<_>>();
    let save = intent(repo, body);
    let plan = save.prepare(base).unwrap();
    assert_eq!(plan.actor(), save.actor());
    assert_eq!(plan.proposal(), save.proposal());
    assert_eq!(plan.repository(), save.repository());
    assert_eq!(plan.fingerprint(), save.fingerprint());
    assert!(!plan.is_noop());
    assert_eq!(plan.expected_head().as_str(), HEAD);
    assert_eq!(plan.expected_proposal_version(), 12);
    assert_eq!(plan.documents().len(), 2);
    for (document, expected) in plan.documents().iter().zip(expected) {
        assert_eq!(document.markdown, expected);
        assert!(document.markdown.contains("unknown: keep-exact"));
        assert_eq!(document.sha256, hash(expected.as_bytes()));
        assert_eq!(document.removed_section_ids, Some(vec![]));
    }
    let files = plan.file_changes();
    assert_eq!(files.len(), 2);
    assert!(
        files.iter().all(
            |file| matches!(file, FileChange::Put { path, .. } if path.as_str() != "docs/c.md")
        )
    );
}

#[test]
fn no_change_and_changes_normalizing_back_to_git_never_create_file_mutations() {
    let repo = Uuid::new_v4();
    let mut base = snapshot(repo);
    base.files[0].markdown.push_str("Cafe\u{301}\n");
    let plan = intent(repo, request(&base)).prepare(base).unwrap();
    assert!(plan.is_noop());
    assert!(plan.file_changes().is_empty());
    let mut base = snapshot(repo);
    base.files[0].markdown.push_str("Café\n");
    let mut body = request(&base);
    body.documents[0].markdown = body.documents[0]
        .markdown
        .replace("Café", "Cafe\u{301}")
        .replace('\n', "\r\n");
    assert!(intent(repo, body).prepare(base).unwrap().is_noop());
}

#[test]
fn changed_source_is_normalized_without_reserializing_markdown_and_reports_normalization_only_edits()
 {
    let repo = Uuid::new_v4();
    let mut base = snapshot(repo);
    base.files[0]
        .markdown
        .push_str("Cafe\u{301}\n```text\nkeep [literal]\n```\n");
    let mut body = request(&base);
    body.documents[0].markdown = body.documents[0]
        .markdown
        .replace("Cafe\u{301}", "Café")
        .replace('\n', "\r\n");
    let expected = body.documents[0].markdown.replace("\r\n", "\n");
    let plan = intent(repo, body).prepare(base).unwrap();
    assert_eq!(plan.documents().len(), 1);
    assert_eq!(plan.documents()[0].markdown, expected);
    assert!(plan.documents()[0].normalization_changed);
    assert!(plan.documents()[0].normalization_only);
    assert!(
        plan.documents()[0]
            .markdown
            .contains("```text\nkeep [literal]\n```")
    );
}

#[test]
fn pinned_repository_head_document_hash_and_catalog_identity_are_independent_gates() {
    let repo = Uuid::new_v4();
    let base = snapshot(repo);
    let save = intent(repo, request(&base));
    let mut wrong = snapshot(Uuid::new_v4());
    assert!(matches!(
        save.prepare(wrong),
        Err(SaveError::SnapshotMismatch)
    ));
    wrong = snapshot(repo);
    wrong.commit = CommitSha::parse("2".repeat(40)).unwrap();
    assert!(matches!(
        save.prepare(wrong),
        Err(SaveError::SnapshotMismatch)
    ));
    wrong = snapshot(repo);
    wrong.files[0].markdown.push_str("external change");
    assert!(matches!(save.prepare(wrong), Err(SaveError::DocumentChanged(id)) if id == "DOC-A"));
    let mut body = request(&base);
    body.documents[0].markdown = body.documents[0]
        .markdown
        .replace("document_id: DOC-A", "document_id: DOC-OTHER");
    assert!(
        matches!(intent(repo, body).prepare(base), Err(SaveError::Validation(report))
        if report.findings.iter().any(|finding| finding.code == "DOCUMENT_ID_MISMATCH"))
    );
    let mut wrong = snapshot(repo);
    wrong.catalog.documents[0].path = ".specspace/private.txt".into();
    assert!(matches!(
        save.prepare(wrong),
        Err(SaveError::Invalid("INVALID_CATALOG"))
    ));
}

#[test]
fn deleting_an_id_requires_repairing_incoming_references_in_the_same_atomic_plan() {
    let repo = Uuid::new_v4();
    let mut base = snapshot(repo);
    base.files[0]
        .markdown
        .push_str("[incoming](ss:DOC-B#SEC-B)\n");
    let mut body = request(&base);
    body.documents[1].markdown = source("DOC-B", "SEC-NEW", "new section");
    assert!(
        matches!(intent(repo, body.clone()).prepare(snapshot_with_link(repo)), Err(SaveError::Validation(report))
        if report.findings.iter().any(|finding| finding.code == "SECTION_REFERENCE_MISSING" && finding.path == "docs/a.md"))
    );
    body.documents[0].markdown = body.documents[0]
        .markdown
        .replace("[incoming](ss:DOC-B#SEC-B)", "incoming removed");
    let plan = intent(repo, body).prepare(base).unwrap();
    assert_eq!(plan.documents().len(), 2);
    assert_eq!(
        plan.documents()[1].removed_section_ids,
        Some(vec!["SEC-B".into()])
    );
}
fn snapshot_with_link(repo: Uuid) -> SaveSnapshot {
    let mut base = snapshot(repo);
    base.files[0]
        .markdown
        .push_str("[incoming](ss:DOC-B#SEC-B)\n");
    base
}

#[test]
fn raw_repair_of_broken_front_matter_does_not_claim_knowledge_of_old_section_removals() {
    let repo = Uuid::new_v4();
    let mut base = snapshot(repo);
    base.files[1].markdown = "---\nunfinished source".into();
    let mut body = request(&base);
    body.documents[1].markdown = source("DOC-B", "SEC-NEW", "repaired");
    let plan = intent(repo, body).prepare(base).unwrap();
    assert_eq!(plan.documents().len(), 1);
    assert_eq!(plan.documents()[0].removed_section_ids, None);
}

#[test]
fn fingerprint_binds_exact_raw_intent_scope_versions_hashes_and_message() {
    let actor = Uuid::new_v4();
    let proposal = Uuid::new_v4();
    let repo = Uuid::new_v4();
    let body = request(&snapshot(repo));
    let first = ValidatedSave::new(actor, proposal, repo, body.clone()).unwrap();
    assert_eq!(
        first.fingerprint(),
        ValidatedSave::new(actor, proposal, repo, body.clone())
            .unwrap()
            .fingerprint()
    );
    for change in 0..9 {
        let mut body = body.clone();
        let mut next_actor = actor;
        let mut next_proposal = proposal;
        let mut next_repo = repo;
        match change {
            0 => next_actor = Uuid::new_v4(),
            1 => next_proposal = Uuid::new_v4(),
            2 => next_repo = Uuid::new_v4(),
            3 => body.expected_head_sha = "2".repeat(40),
            4 => body.expected_proposal_version += 1,
            5 => body.message.push_str(" changed"),
            6 => body.documents[0].markdown.push_str("changed"),
            7 => body.documents[0].expected_document_sha256 = "3".repeat(64),
            _ => body.documents[0].document_id = "DOC-OTHER".into(),
        }
        assert_ne!(
            first.fingerprint(),
            ValidatedSave::new(next_actor, next_proposal, next_repo, body)
                .unwrap()
                .fingerprint()
        );
    }
}

#[test]
fn buffered_intent_hash_matches_protocol_bytes_across_escapes_unicode_and_buffer_boundaries() {
    let actor = Uuid::new_v4();
    let proposal = Uuid::new_v4();
    let repo = Uuid::new_v4();
    let mut body = request(&snapshot(repo));
    body.documents[0]
        .markdown
        .push_str(&"שָׁלוֹם \"quoted\"\\\n".repeat(10_000));
    let expected =
        hash(&serde_json::to_vec(&("specspace-save-v1", actor, proposal, repo, &body)).unwrap());
    let intent = ValidatedSave::new(actor, proposal, repo, body).unwrap();
    assert_eq!(intent.fingerprint(), expected);
}

#[test]
fn malformed_and_oversized_save_requests_are_rejected_before_snapshot_processing() {
    let repo = Uuid::new_v4();
    let original = request(&snapshot(repo));
    for change in 0..12 {
        let mut body = original.clone();
        match change {
            0 => body.expected_head_sha = "main".into(),
            1 => body.expected_proposal_version = 0,
            2 => body.message = " \n ".into(),
            3 => body.message = "Save\n\nspecspace-actor: forged".into(),
            4 => body.documents[0].document_id = "../secret".into(),
            5 => body.documents[1] = body.documents[0].clone(),
            6 => body.documents[0].expected_document_sha256 = "x".repeat(64),
            7 => body.documents[0].markdown.push('\0'),
            8 => body.documents[0].markdown = "x".repeat(MAX_FILE_BYTES + 1),
            9 => body.documents.clear(),
            10 => {
                body.documents = (0..51)
                    .map(|index| DocumentSave {
                        document_id: format!("DOC-{index}"),
                        expected_document_sha256: "1".repeat(64),
                        markdown: "x".into(),
                    })
                    .collect()
            }
            _ => {
                body.documents = (0..6)
                    .map(|index| DocumentSave {
                        document_id: format!("DOC-{index}"),
                        expected_document_sha256: "1".repeat(64),
                        markdown: "x".repeat(MAX_FILE_BYTES),
                    })
                    .collect()
            }
        }
        assert!(
            ValidatedSave::new(Uuid::new_v4(), Uuid::new_v4(), repo, body).is_err(),
            "case {change}"
        );
    }
    assert!(ValidatedSave::new(Uuid::nil(), Uuid::new_v4(), repo, original).is_err());
}

#[test]
fn normalized_output_cannot_expand_past_the_atomic_write_byte_limit() {
    let repo = Uuid::new_v4();
    let mut base = snapshot(repo);
    base.files.clear();
    base.catalog.documents.clear();
    for index in 0..9 {
        let id = format!("DOC-{index}");
        let path = format!("docs/{index}.md");
        base.catalog.documents.push(CatalogEntry {
            document_id: id.clone(),
            path: path.clone(),
        });
        base.files.push(SourceFile {
            path,
            markdown: source(&id, &format!("SEC-{index}"), "old"),
        });
    }
    let mut body = request(&base);
    for change in &mut body.documents {
        change.markdown.push_str(&"\u{0344}".repeat(300_000));
    }
    let save = intent(repo, body);
    assert!(matches!(
        save.prepare(base),
        Err(SaveError::Invalid("SAVE_BYTE_LIMIT"))
    ));
}

#[test]
fn protected_source_and_missing_alt_remain_visible_warnings_and_asset_verification_is_explicit() {
    let repo = Uuid::new_v4();
    let mut base = snapshot(repo);
    let image = format!("assets/{}.png", "a".repeat(64));
    base.asset_paths.insert(image.clone());
    let mut body = request(&base);
    body.documents[0].markdown.push_str(&format!(
        "\n<!-- retained opaque source -->\n\n![]({image})\n"
    ));
    let plan = intent(repo, body).prepare(base).unwrap();
    assert!(plan.validation().ready());
    assert!(
        plan.validation()
            .findings
            .iter()
            .any(|finding| finding.code == "IMAGE_ALT_REQUIRED")
    );
    assert!(
        plan.validation()
            .findings
            .iter()
            .any(|finding| finding.code == "UNSUPPORTED_HTML")
    );
    assert!(
        plan.documents()[0]
            .markdown
            .contains("<!-- retained opaque source -->")
    );
    assert_eq!(
        plan.validation()
            .required_assets
            .iter()
            .cloned()
            .collect::<Vec<_>>(),
        [image]
    );
}
