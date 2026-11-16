use super::*;
use crate::catalog::CatalogEntry;

fn document(id: &str, body: &str) -> String {
    format!(
        "---\ndocument_id: {id}\ntitle: Test\nowner_team: test\nschema_version: 1\n---\n\n{body}"
    )
}
fn catalog(files: &[(String, String)]) -> Catalog {
    Catalog {
        schema_version: 1,
        documents: files
            .iter()
            .map(|(path, source)| CatalogEntry {
                document_id: markdown::parse(source).unwrap().metadata.document_id,
                path: path.clone(),
            })
            .collect(),
    }
}
fn files(a: &str, b: &str) -> Vec<(String, String)> {
    vec![
        ("docs/a.md".into(), document("DOC-A", a)),
        ("docs/b.md".into(), document("DOC-B", b)),
    ]
}
const A: &str = "<!-- ss:section:SEC-A -->\n## First\n\n";
const B: &str = "<!-- ss:section:SEC-B -->\n## Second\n\n";

#[test]
fn references_cover_local_cross_document_paths_nested_sections_and_reference_definitions() {
    let image = format!("assets/{}.png", "a".repeat(64));
    let a = format!(
        "{A}[local](#SEC-A) [other](ss:DOC-B#SEC-B) [path](docs/b.md#SEC-NESTED) [top](ss:DOC-B) [ref][x]\n\n[x]: ss:DOC-B#SEC-B\n\n![alt]({image})\n\n`[not a link](#MISSING)`\n"
    );
    let files = files(
        &a,
        &format!("{B}> <!-- ss:section:SEC-NESTED -->\n> ### Nested\n"),
    );
    let report = validate(&catalog(&files), &files, &HashSet::from([image.clone()]));
    assert!(report.ready(), "{:?}", report.findings);
    assert_eq!(report.section_ids["DOC-B"], ["SEC-B", "SEC-NESTED"]);
    assert_eq!(report.required_assets, BTreeSet::from([image]));
}

#[test]
fn removed_sections_block_incoming_references_in_untouched_documents_with_source_locations() {
    let a = format!("{A}עברית [incoming](ss:DOC-B#SEC-B)\n");
    let files = files(&a, "replacement without the deleted heading\n");
    let report = validate(&catalog(&files), &files, &HashSet::new());
    let finding = report
        .findings
        .iter()
        .find(|finding| finding.code == "SECTION_REFERENCE_MISSING")
        .unwrap();
    assert_eq!(finding.path, "docs/a.md");
    assert!(
        files[0].1[finding.source_start_byte.unwrap()..finding.source_end_byte.unwrap()]
            .starts_with("[incoming]")
    );
    assert!(!report.ready());
    let repaired = files
        .iter()
        .map(|(path, source)| {
            (
                path.clone(),
                source.replace("[incoming](ss:DOC-B#SEC-B)", "incoming removed"),
            )
        })
        .collect::<Vec<_>>();
    assert!(validate(&catalog(&repaired), &repaired, &HashSet::new()).ready());
}

#[test]
fn section_ownership_missing_documents_and_missing_images_cannot_resolve_accidentally() {
    let files = files(
        &format!(
            "{A}[wrong owner](ss:DOC-A#SEC-B) [local](#SEC-B) [missing](ss:DOC-ABSENT)\n\n![alt](assets/{}.png)",
            "a".repeat(64)
        ),
        B,
    );
    let report = validate(&catalog(&files), &files, &HashSet::new());
    assert!(!report.ready());
    assert_eq!(
        report
            .findings
            .iter()
            .filter(|finding| finding.code == "SECTION_REFERENCE_MISSING")
            .count(),
        2
    );
    assert!(
        report
            .findings
            .iter()
            .any(|finding| finding.code == "DOCUMENT_REFERENCE_MISSING")
    );
    assert!(
        report
            .findings
            .iter()
            .any(|finding| finding.code == "ASSET_MISSING")
    );
}

#[test]
fn empty_and_nested_headings_are_in_the_global_identity_check_and_orphan_markers_block() {
    let files = files(
        "<!-- ss:section:SEC-DUP -->\n##\n",
        "> <!-- ss:section:SEC-DUP -->\n> ## Nested\n\n<!-- ss:section:SEC-LOST -->\n\nparagraph\n",
    );
    let report = validate(&catalog(&files), &files, &HashSet::new());
    assert!(!report.ready());
    assert_eq!(report.section_ids["DOC-A"], ["SEC-DUP"]);
    assert!(
        report
            .findings
            .iter()
            .any(|finding| finding.code == "DUPLICATE_SECTION_ID")
    );
    assert!(
        report
            .findings
            .iter()
            .any(|finding| finding.code == "ORPHAN_SECTION_MARKER")
    );
}

#[test]
fn catalog_orphans_missing_files_and_forbidden_paths_remain_blockers() {
    let files = files(A, B);
    let mut catalog = catalog(&files);
    catalog.documents[1].path = ".specspace/private.md".into();
    let report = validate(&catalog, &files, &HashSet::new());
    for code in [
        "UNCATALOGUED_DOCUMENT",
        "CATALOG_FILE_MISSING",
        "PATH_FORBIDDEN",
    ] {
        assert!(
            report.findings.iter().any(|finding| finding.code == code),
            "{code}"
        );
    }
    assert!(!report.ready());
}

#[test]
fn uri_decoding_is_strict_once_and_cannot_escape_canonical_repository_paths() {
    let mut files = files(
        &format!(
            "{A}[unicode](docs/%D7%A7%D7%95%D7%91%D7%A5%20%D7%A0%D7%95%D7%A1%D7%A3.md#%53EC-B)\n"
        ),
        B,
    );
    files[1].0 = "docs/קובץ נוסף.md".into();
    assert!(validate(&catalog(&files), &files, &HashSet::new()).ready());
    files[0].1.push_str("[escape](docs/%2e%2e/private.md) [bad](#%FF) [malformed](#%G0) [double](ss:DOC-B#%2553EC-B)\n");
    let report = validate(&catalog(&files), &files, &HashSet::new());
    assert!(!report.ready());
    assert_eq!(
        report
            .findings
            .iter()
            .filter(|finding| finding.code == "INVALID_INTERNAL_REFERENCE")
            .count(),
        4
    );
}

#[test]
fn truncated_warning_output_cannot_hide_a_later_blocker() {
    let files = files(
        &format!(
            "{A}{}\n[bad](ss:DOC-ABSENT)\n",
            "paragraph <span>source</span>\n\n".repeat(600)
        ),
        B,
    );
    let report = validate(&catalog(&files), &files, &HashSet::new());
    assert_eq!(report.findings.len(), MAX_FINDINGS);
    assert!(report.findings_truncated());
    assert!(!report.ready());
    assert!(
        report
            .findings
            .iter()
            .all(|finding| finding.severity == Severity::Warning)
    );
    assert!(report.blockers_total > 0);
    assert!(report.findings_total > MAX_FINDINGS);
}

#[test]
fn parser_reference_expansion_stops_with_an_explicit_incomplete_blocking_report() {
    let id = "D".repeat(120);
    let section = "S".repeat(120);
    let files = vec![
        (
            "docs/a.md".into(),
            document(
                "DOC-A",
                &format!(
                    "{A}{}\n\n[r]: ss:{id}#{section}\n",
                    "[x][r] ".repeat(150_000)
                ),
            ),
        ),
        (
            "docs/b.md".into(),
            document(&id, &format!("<!-- ss:section:{section} -->\n## Heading\n")),
        ),
    ];
    let report = validate(&catalog(&files), &files, &HashSet::new());
    assert!(!report.ready());
    assert!(!report.complete);
    assert!(
        report
            .findings
            .iter()
            .any(|finding| finding.code == "REFERENCE_EXPANSION_LIMIT")
    );
}

#[test]
fn a_large_reference_title_cannot_hide_a_later_broken_reference_as_plain_text() {
    let files = files(
        &format!(
            "{A}[one][large] [two][large] [missing]\n\n[large]: https://example.test/ \"{}\"\n[missing]: ss:DOC-ABSENT\n",
            "x".repeat(60_000)
        ),
        B,
    );
    let offset = body_offset(&files[0].1).unwrap();
    let missing = Parser::new_ext(&files[0].1[offset..], options())
        .filter(|event| {
            matches!(event,
        Event::Start(Tag::Link { dest_url, .. }) if dest_url.as_ref() == "ss:DOC-ABSENT")
        })
        .count();
    assert_eq!(
        missing, 0,
        "the parser drops this reference after exhausting its fuel"
    );
    let report = validate(&catalog(&files), &files, &HashSet::new());
    assert!(!report.ready());
    assert!(!report.complete);
    assert!(
        report
            .findings
            .iter()
            .any(|finding| finding.code == "REFERENCE_EXPANSION_LIMIT")
    );
}

#[test]
fn reference_probe_handles_case_escaped_labels_multiline_titles_and_quoted_definitions() {
    let files = files(
        &format!(
            "{A}[one][mixed] [two][brack\\]et]\n\n[MIXED]: ss:DOC-B#SEC-B\n  \"a multiline\n  title\"\n\n> [brack\\]et]: ss:DOC-B#SEC-B\n"
        ),
        B,
    );
    let report = validate(&catalog(&files), &files, &HashSet::new());
    assert!(report.ready(), "{:?}", report.findings);
    let files = self::files(&format!("{A}[x][r]\n\n[r]: ss:DOC-B\n[r]: ss:DOC-A\n"), B);
    let report = validate(&catalog(&files), &files, &HashSet::new());
    assert!(report.ready(), "{:?}", report.findings);
    let files = self::files(
        &format!("{A}[x][r]\n\n[r]: ss:DOC-ABSENT\n[r]: ss:DOC-B\n"),
        B,
    );
    let report = validate(&catalog(&files), &files, &HashSet::new());
    assert!(!report.ready());
    assert!(report.complete);
    assert!(
        report
            .findings
            .iter()
            .any(|finding| finding.code == "DOCUMENT_REFERENCE_MISSING")
    );
}

#[test]
fn normalization_changes_only_unicode_and_line_endings_and_bounds_the_result() {
    let raw = "---\r\nunknown: keep\r\n---\r\n\r\n# Cafe\u{301}\r\n```\r\ncode\r\n```";
    let normalized = normalize(raw).unwrap();
    assert_eq!(
        normalized,
        "---\nunknown: keep\n---\n\n# Café\n```\ncode\n```"
    );
    assert_eq!(normalize(&normalized).unwrap(), normalized);
    assert_eq!(normalize("x\ry").unwrap(), "x\ny");
    assert_eq!(normalize("bad\0source"), Err("INVALID_CONTENT"));
    assert_eq!(normalize(&"x".repeat(MAX_FILE + 1)), Err("FILE_TOO_LARGE"));
    assert_eq!(
        normalize(&"\u{0344}".repeat(600_000)),
        Err("FILE_TOO_LARGE")
    );
}

#[test]
fn diagnostic_json_is_bounded_and_does_not_serialize_internal_book_identity_maps() {
    let mut files = files(A, B);
    files[0].0 = "א".repeat(100_000);
    let report = validate(&catalog(&files), &files, &HashSet::new());
    assert!(!report.ready());
    assert!(report.findings[0].path_truncated);
    assert_eq!(report.findings[0].path.chars().count(), 240);
    let json = serde_json::to_value(&report).unwrap();
    assert!(json.get("section_ids").is_none());
    assert!(json.get("required_assets").is_none());
    assert_eq!(json["findings_truncated"], false);
    assert!(serde_json::to_vec(&report).unwrap().len() < 4096);
}

#[test]
fn adversarial_duplicate_definitions_stop_at_an_explicit_global_scan_limit() {
    let files = files(
        &format!(
            "{A}[x][r]\n\n{}",
            "[r]: https://example.test/x\n".repeat(30_000)
        ),
        B,
    );
    let report = validate(&catalog(&files), &files, &HashSet::new());
    assert!(!report.ready());
    assert!(!report.complete);
    assert!(
        report
            .findings
            .iter()
            .any(|finding| finding.code == "REFERENCE_DEFINITION_WORK_LIMIT")
    );
}

#[test]
fn the_whole_synthetic_book_passes_with_its_exact_asset_manifest() {
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../fixtures/repository");
    let catalog: Catalog = serde_json::from_str(
        &std::fs::read_to_string(root.join(".specspace/catalog.json")).unwrap(),
    )
    .unwrap();
    let files = catalog
        .documents
        .iter()
        .map(|entry| {
            (
                entry.path.clone(),
                std::fs::read_to_string(root.join(&entry.path)).unwrap(),
            )
        })
        .collect::<Vec<_>>();
    let assets = std::fs::read_dir(root.join("assets"))
        .unwrap()
        .map(|entry| format!("assets/{}", entry.unwrap().file_name().to_str().unwrap()))
        .collect();
    let report = validate(&catalog, &files, &assets);
    assert!(report.ready(), "{:?}", report.findings);
    assert_eq!(report.findings_total, 0);
}

#[test]
fn references_that_fit_each_parser_budget_still_obey_the_whole_book_work_limit() {
    let destination = format!("https://example.test/{}", "x".repeat(80));
    let files = (0..500)
        .map(|index| {
            (
                format!("docs/{index}.md"),
                document(
                    &format!("DOC-{index}"),
                    &format!(
                        "<!-- ss:section:SEC-{index} -->\n## Heading\n\n{}\n\n[r]: {destination}\n",
                        "[x][r] ".repeat(900)
                    ),
                ),
            )
        })
        .collect::<Vec<_>>();
    let report = validate(&catalog(&files), &files, &HashSet::new());
    assert!(!report.ready());
    assert!(!report.complete);
    assert!(
        report
            .findings
            .iter()
            .any(|finding| finding.code == "REFERENCE_WORK_LIMIT")
    );
    assert!(
        !report
            .findings
            .iter()
            .any(|finding| finding.code == "REFERENCE_EXPANSION_LIMIT")
    );
}
