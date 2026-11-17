use super::*;
fn source(body: &str) -> String {
    format!(
        "---\ndocument_id: DOC-ONE\ntitle: Synthetic\nowner_team: test\nschema_version: 1\n---\n{body}"
    )
}
fn checked(before: Option<&str>, after: Option<&str>) -> ReadablePair {
    let pair = ReadablePair::prepare(before, after);
    assert!(pair.ready);
    let old = pair.before.document().map_or(0, |doc| doc.units.len());
    let new = pair.after.document().map_or(0, |doc| doc.units.len());
    let mut a: Vec<_> = pair
        .changes
        .iter()
        .filter_map(|row| row.before_index)
        .collect();
    let mut b: Vec<_> = pair
        .changes
        .iter()
        .filter_map(|row| row.after_index)
        .collect();
    a.sort();
    b.sort();
    assert_eq!(a, (0..old).collect::<Vec<_>>());
    assert_eq!(b, (0..new).collect::<Vec<_>>());
    let replay = ReadablePair::prepare(before, after);
    assert_eq!(pair.changes, replay.changes);
    let ids: std::collections::HashSet<_> = pair.changes.iter().map(|row| &row.id).collect();
    assert_eq!(ids.len(), pair.changes.len());
    for row in &pair.changes {
        let a = row
            .before_index
            .map(|i| &pair.before.document().unwrap().units[i]);
        let b = row
            .after_index
            .map(|i| &pair.after.document().unwrap().units[i]);
        assert_eq!(
            row.raw_changed,
            a.map(|u| &u.raw_sha256) != b.map(|u| &u.raw_sha256)
        );
        assert_eq!(
            row.position_changed,
            row.before_index
                .zip(row.after_index)
                .is_some_and(|(a, b)| a != b)
        );
    }
    pair
}
#[test]
fn stable_headings_pair_moves_and_edits_without_crossing_section_context() {
    let old = source(
        "<!-- ss:section:SEC-A -->\n# Alpha\n\nold A\n\n<!-- ss:section:SEC-B -->\n# Beta\n\nold B\n",
    );
    let new = source(
        "<!-- ss:section:SEC-B -->\n# Beta renamed\n\nnew B\n\n<!-- ss:section:SEC-A -->\n# Alpha\n\nnew A\n",
    );
    let pair = checked(Some(&old), Some(&new));
    let headings: Vec<_> = pair
        .changes
        .iter()
        .filter(|row| row.matched_by == Some(UnitMatch::HeadingId))
        .collect();
    assert_eq!(headings.len(), 2);
    assert!(headings.iter().all(|row| row.position_changed));
    assert!(headings.iter().any(|row| row.raw_changed));
    let paragraphs: Vec<_> = pair
        .changes
        .iter()
        .filter(|row| row.matched_by == Some(UnitMatch::SectionPosition))
        .collect();
    assert_eq!(paragraphs.len(), 2);
    for row in paragraphs {
        let a = &old[pair.before.document().unwrap().units[row.before_index.unwrap()]
            .body_start_byte
            ..pair.before.document().unwrap().units[row.before_index.unwrap()].source_end_byte];
        let b = &new[pair.after.document().unwrap().units[row.after_index.unwrap()].body_start_byte
            ..pair.after.document().unwrap().units[row.after_index.unwrap()].source_end_byte];
        assert_eq!(a.contains('A'), b.contains('A'));
        assert_eq!(row.render_changed, Some(true));
    }
}
#[test]
fn source_and_render_matching_retain_formatting_only_and_metadata_changes() {
    let old = source("\ntext **bold**\n\nunchanged\n");
    let new =
        source("\ntext __bold__\n\nunchanged\n").replace("title: Synthetic", "title: Changed");
    let pair = checked(Some(&old), Some(&new));
    assert!(pair.raw_changed);
    assert!(
        pair.changes
            .iter()
            .any(|row| row.matched_by == Some(UnitMatch::RenderedContent)
                && row.raw_changed
                && row.render_changed == Some(false))
    );
    assert!(
        pair.changes
            .iter()
            .any(|row| row.matched_by == Some(UnitMatch::ExactSource) && !row.raw_changed)
    );
    let metadata = pair
        .changes
        .iter()
        .find(|row| row.before_index == Some(0))
        .unwrap();
    assert_eq!(metadata.matched_by, Some(UnitMatch::Structural));
    assert!(metadata.raw_changed && metadata.render_changed.is_none());
}
#[test]
fn ambiguous_duplicates_and_changed_or_missing_heading_ids_never_gain_trusted_pairings() {
    let old = source("<!-- ss:section:SEC-A -->\n# Same\n\nrepeat\n\nrepeat\n");
    let new = source("<!-- ss:section:SEC-B -->\n# Same\n\nrepeat\n\nrepeat\n");
    let pair = checked(Some(&old), Some(&new));
    assert!(!pair.changes.iter().any(|row| matches!(
        row.matched_by,
        Some(UnitMatch::HeadingId | UnitMatch::RenderedContent | UnitMatch::SectionPosition)
    )));
    let paragraphs: Vec<_> = pair
        .changes
        .iter()
        .filter(|row| {
            row.before_index.is_some_and(|i| {
                pair.before.document().unwrap().units[i].kind == UnitKind::Paragraph
            }) || row.after_index.is_some_and(|i| {
                pair.after.document().unwrap().units[i].kind == UnitKind::Paragraph
            })
        })
        .collect();
    assert_eq!(paragraphs.len(), 4);
    assert!(paragraphs.iter().all(|row| row.matched_by.is_none()));
    let missing = checked(
        Some(&source("# Missing\n\nold\n")),
        Some(&source("# Missing\n\nnew\n")),
    );
    assert!(
        !missing
            .changes
            .iter()
            .any(|row| row.matched_by == Some(UnitMatch::SectionPosition))
    );
}
#[test]
fn insertions_with_edits_do_not_invent_positional_matches_when_counts_differ() {
    let old = source("<!-- ss:section:SEC-A -->\n# Alpha\n\nold\n");
    let new = source("<!-- ss:section:SEC-A -->\n# Alpha\n\ninserted\n\nedited\n");
    let pair = checked(Some(&old), Some(&new));
    assert!(
        !pair
            .changes
            .iter()
            .any(|row| row.matched_by == Some(UnitMatch::SectionPosition))
    );
    assert_eq!(
        pair.changes
            .iter()
            .filter(|row| row.status == UnitStatus::Added)
            .count(),
        2
    );
    assert_eq!(
        pair.changes
            .iter()
            .filter(|row| row.status == UnitStatus::Deleted)
            .count(),
        1
    );
}
#[test]
fn missing_sides_and_unavailable_present_source_are_distinct_and_explicit() {
    let text = source("hello\n");
    let added = checked(None, Some(&text));
    assert!(
        added
            .changes
            .iter()
            .all(|row| row.status == UnitStatus::Added)
    );
    let deleted = checked(Some(&text), None);
    assert!(
        deleted
            .changes
            .iter()
            .all(|row| row.status == UnitStatus::Deleted)
    );
    let same = checked(Some(&text), Some(&text));
    assert!(!same.raw_changed);
    assert!(
        same.changes
            .iter()
            .all(|row| row.status == UnitStatus::Unchanged && !row.position_changed)
    );
    for bad in ["", "broken", "---\r\ninvalid\r\n---\r\n"] {
        let pair = ReadablePair::prepare(Some(bad), Some(&text));
        assert!(!pair.ready && pair.changes.is_empty());
        assert!(matches!(pair.before, ReadableSide::Unavailable { .. }));
        assert!(pair.before.sha256().is_some());
        assert!(pair.after.document().is_some());
        assert!(pair.raw_changed);
    }
    let unchanged_invalid = ReadablePair::prepare(Some("broken"), Some("broken"));
    assert!(!unchanged_invalid.ready);
    assert!(!unchanged_invalid.raw_changed); // Still unavailable, never an empty READY comparison.
}
#[test]
fn row_identity_binds_the_complete_pair_not_just_local_ordinals_or_section_names() {
    let old = source("<!-- ss:section:SEC-A -->\n# Alpha\n\nold\n");
    let new = source("<!-- ss:section:SEC-A -->\n# Alpha\n\nnew\n");
    let a = checked(Some(&old), Some(&new));
    let changed_elsewhere = new.replace("title: Synthetic", "title: Other");
    let b = checked(Some(&old), Some(&changed_elsewhere));
    assert_eq!(a.changes.len(), b.changes.len());
    for (a, b) in a.changes.iter().zip(&b.changes) {
        assert_ne!(a.id, b.id);
    }
}

#[test]
fn identical_complete_sources_never_show_duplicate_or_protected_units_as_changes() {
    let text = source(
        "<!-- ss:section:SEC-SAME -->\n# Same\n\nrepeat\n\nrepeat\n\n<!-- ss:section:SEC-SAME -->\n# Same\n\n<script>x</script>\n",
    );
    let pair = checked(Some(&text), Some(&text));
    assert!(!pair.raw_changed);
    assert!(
        pair.changes
            .iter()
            .all(|row| row.status == UnitStatus::Unchanged
                && !row.raw_changed
                && !row.position_changed)
    );
}

#[test]
fn every_short_repeated_block_sequence_preserves_one_to_one_coverage_and_visible_reordering() {
    let mut bodies = vec![String::new()];
    for length in 1..=3 {
        for bits in 0..(1 << length) {
            bodies.push(
                (0..length)
                    .map(|n| {
                        if bits & (1 << n) == 0 {
                            "a\n\n"
                        } else {
                            "b\n\n"
                        }
                    })
                    .collect(),
            );
        }
    }
    for a in &bodies {
        for b in &bodies {
            let old = source(&format!("<!-- ss:section:SEC-ROOT -->\n# Root\n\n{a}"));
            let new = source(&format!("<!-- ss:section:SEC-ROOT -->\n# Root\n\n{b}"));
            let pair = checked(Some(&old), Some(&new));
            assert_eq!(pair.raw_changed, old != new);
            if old == new {
                assert!(
                    pair.changes
                        .iter()
                        .all(|row| row.status == UnitStatus::Unchanged)
                );
            } else {
                assert!(
                    pair.changes
                        .iter()
                        .any(|row| row.raw_changed || row.position_changed)
                );
            }
        }
    }
    let old = source("<!-- ss:section:SEC-ROOT -->\n# Root\n\na\n\nb\n\n");
    let new = source("<!-- ss:section:SEC-ROOT -->\n# Root\n\nb\n\na\n\n");
    let reordered = checked(Some(&old), Some(&new));
    assert!(
        reordered
            .changes
            .iter()
            .any(|row| row.position_changed && !row.raw_changed)
    );
}
