use super::*;
fn source(body: &str) -> String {
    format!(
        "---\ndocument_id: DOC-ONE\ntitle: Synthetic\nowner_team: test\nschema_version: 1\n---\n{body}"
    )
}
fn check(source: &str) -> ReadableDocument {
    let projected = prepare(source).unwrap();
    let mut position = 0;
    let mut rebuilt = String::new();
    for unit in &projected.units {
        assert_eq!(unit.source_start_byte, position);
        let raw = &source[unit.source_start_byte..unit.source_end_byte];
        assert_eq!(unit.raw_sha256, hash(raw.as_bytes()));
        assert!(
            unit.body_start_byte >= unit.source_start_byte
                && unit.body_start_byte <= unit.source_end_byte
        );
        for event in &unit.events {
            assert!(
                event.source_start_byte >= unit.body_start_byte
                    && event.source_end_byte <= unit.source_end_byte
            );
        }
        rebuilt.push_str(raw);
        position = unit.source_end_byte;
    }
    assert_eq!(rebuilt, source);
    assert_eq!(projected.source_bytes, source.len());
    assert_eq!(projected.sha256, hash(source.as_bytes()));
    projected
}
#[test]
fn gfm_units_cover_exact_source_and_keep_stable_heading_identity() {
    let text = source(
        "\n<!-- ss:section:SEC-ONE -->\n## כותרת\n\n**שָׁלוֹם** 👩🏽‍💻\n\n- [x] parent\n  - nested\n\n> quote\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n```rust\nlet x = 1;\n```\n\n",
    );
    let result = check(&text);
    for kind in [
        UnitKind::Metadata,
        UnitKind::Heading,
        UnitKind::Paragraph,
        UnitKind::List,
        UnitKind::Quote,
        UnitKind::Table,
        UnitKind::Code,
    ] {
        assert!(
            result.units.iter().any(|unit| unit.kind == kind),
            "{kind:?}"
        );
    }
    let heading = result
        .units
        .iter()
        .find(|unit| unit.kind == UnitKind::Heading)
        .unwrap();
    assert_eq!(heading.heading_id.as_deref(), Some("SEC-ONE"));
    assert_eq!(heading.section_ids, ["SEC-ONE"]);
    assert!(heading.semantic_sha256.is_some());
}
#[test]
fn metadata_definitions_html_and_trailing_source_are_not_silently_erased() {
    let text = source(
        "\n[x]: https://example.com\n\n<!-- ss:section:SEC-ONE -->\n# Heading\n\n[link][x]\n\n<script>alert(1)</script>\n\n<!-- private comment -->\n\n",
    );
    let result = check(&text);
    assert_eq!(result.units[0].kind, UnitKind::Metadata);
    assert!(
        result
            .units
            .iter()
            .any(|unit| unit.protected_reason.is_some()
                && text[unit.source_start_byte..unit.source_end_byte].contains("[x]:"))
    );
    assert!(
        result
            .units
            .iter()
            .any(|unit| unit.protected_reason.is_some()
                && text[unit.source_start_byte..unit.source_end_byte].contains("<script>"))
    );
    assert!(
        result
            .units
            .iter()
            .flat_map(|unit| &unit.events)
            .all(|event| event
                .text
                .as_deref()
                .is_none_or(|text| !text.contains("alert(1)")))
    );
}
#[test]
fn semantic_identity_ignores_source_coordinates_but_keeps_formatting_and_link_targets() {
    let a = check(&source("\nhello **world** [link](https://a.example)\n"));
    let b = check(&source("\n\nhello __world__ [link](https://a.example)\n"));
    let c = check(&source("\nhello world [link](https://b.example)\n"));
    let paragraph = |doc: &ReadableDocument| {
        doc.units
            .iter()
            .find(|unit| unit.kind == UnitKind::Paragraph)
            .unwrap()
            .semantic_sha256
            .clone()
    };
    assert_eq!(paragraph(&a), paragraph(&b));
    assert_ne!(a.units[1].raw_sha256, b.units[1].raw_sha256);
    assert_ne!(paragraph(&a), paragraph(&c));
    let d = check(&source("\nhello **world** [link](https://b.example)\n"));
    assert_ne!(paragraph(&a), paragraph(&d));
}
#[test]
fn duplicate_and_missing_markers_never_invent_a_unique_heading_identity() {
    let result = check(&source(
        "<!-- ss:section:SEC-SAME -->\n# First\n\n<!-- ss:section:SEC-SAME -->\n# Second\n\n# Missing\n",
    ));
    let headings: Vec<_> = result
        .units
        .iter()
        .filter(|unit| unit.kind == UnitKind::Heading)
        .collect();
    assert_eq!(headings.len(), 3);
    assert!(headings.iter().all(|unit| unit.heading_id.is_none()));
    assert_eq!(headings[0].section_ids, ["SEC-SAME"]);
    assert_eq!(headings[1].section_ids, ["SEC-SAME"]);
}
#[test]
fn malformed_input_and_adversarial_ast_work_fail_explicitly_before_materialization() {
    assert!(matches!(
        prepare("broken"),
        Err(ReadableError::Source("FRONT_MATTER_REQUIRED"))
    ));
    assert!(matches!(
        prepare(&source(&"x".repeat(2 * 1024 * 1024))),
        Err(ReadableError::Source("FILE_TOO_LARGE"))
    ));
    assert!(matches!(
        prepare(&source(&"a\n\n".repeat(11_000))),
        Err(ReadableError::EventLimit)
    ));
    assert!(matches!(
        prepare(&source(&format!("{}deep\n", "> ".repeat(70)))),
        Err(ReadableError::DepthLimit)
    ));
    let destination = format!("https://example.com/{}", "x".repeat(100_000));
    let expanded = source(&format!("{}\n\n[x]: {destination}\n", "[x] ".repeat(150)));
    assert!(expanded.len() < 2 * 1024 * 1024);
    assert!(matches!(
        prepare(&expanded),
        Err(ReadableError::Source("REFERENCE_EXPANSION_LIMIT"))
    ));
}

#[test]
fn empty_unterminated_crlf_nested_and_reference_documents_keep_complete_units() {
    for body in [
        "",
        "\n\n",
        "plain",
        "```ts\nunterminated",
        "```\n",
        "---\n",
        "- first\n  - second\n\n  paragraph\n",
        "> <!-- ss:section:SEC-NESTED -->\n> ## Nested\n>\n> text\n",
        "text\r\n\r\nmore\r\n",
        "[one][x]\n\n[x]: https://example.com\n",
        "![alt](assets/0000000000000000000000000000000000000000000000000000000000000000.png)\n",
        "[unsafe](javascript:alert%281%29)\n",
    ] {
        check(&source(body));
    }
}

#[test]
fn code_language_changes_remain_part_of_readable_identity_even_inside_containers() {
    let a = check(&source("> ```rust\n> let x = 1;\n> ```\n"));
    let b = check(&source("> ```javascript\n> let x = 1;\n> ```\n"));
    let a = a
        .units
        .iter()
        .find(|unit| unit.kind == UnitKind::Quote)
        .unwrap();
    let b = b
        .units
        .iter()
        .find(|unit| unit.kind == UnitKind::Quote)
        .unwrap();
    assert_eq!(a.code_info, ["rust"]);
    assert_eq!(b.code_info, ["javascript"]);
    assert_ne!(a.semantic_sha256, b.semantic_sha256);
}
