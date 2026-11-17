use super::*;
use crate::diff::readable::prepare;
use serde_json::Value;
fn source(body: &str) -> String {
    format!(
        "---\ndocument_id: DOC-ONE\ntitle: Synthetic\nowner_team: test\nschema_version: 1\n---\n{body}"
    )
}
fn reconstruct(document: &ReadableDocument, index: usize, limit: usize) -> usize {
    let unit = &document.units[index];
    let original: Vec<_> = unit
        .events
        .iter()
        .map(|event| serde_json::to_value(event).unwrap())
        .collect();
    let mut rebuilt = vec![None::<Value>; original.len()];
    let mut cursor = EventCursor::default();
    let mut stack = Vec::<Value>::new();
    let mut pages = 0;
    loop {
        let page = document.event_window(index, cursor, limit).unwrap();
        assert_eq!(page.cursor, cursor);
        assert_eq!(page.document_sha256, document.sha256);
        assert_eq!(page.unit.raw_sha256, unit.raw_sha256);
        assert_eq!(
            serde_json::to_value(&page.start_stack).unwrap(),
            serde_json::to_value(&stack).unwrap()
        );
        assert!(serde_json::to_vec(&page).unwrap().len() <= MAX_EVENT_WINDOW_JSON_BYTES);
        assert!(page.returned_text_bytes <= limit && page.fragments.len() <= MAX_FRAGMENTS);
        let mut bytes = 0;
        for fragment in &page.fragments {
            let mut value = serde_json::to_value(&fragment.event).unwrap();
            let old = &original[fragment.event_index];
            let text = old["text"].as_str().unwrap_or("");
            assert_eq!(
                value["text"].as_str().unwrap_or(""),
                &text[fragment.text_start_byte..fragment.text_end_byte]
            );
            if let Some(offset) = old["offset_utf16"].as_u64() {
                assert_eq!(
                    value["offset_utf16"],
                    offset + text[..fragment.text_start_byte].encode_utf16().count() as u64
                );
            }
            bytes += fragment.text_end_byte - fragment.text_start_byte;
            match fragment.event.kind {
                RenderKind::Open => stack.push(value.clone()),
                RenderKind::Close => {
                    assert_eq!(stack.pop().unwrap()["tag"], value["tag"]);
                }
                _ => {}
            }
            if let Some(previous) = &mut rebuilt[fragment.event_index] {
                assert_eq!(fragment.event.kind, RenderKind::Text);
                assert_eq!(
                    previous["text"].as_str().unwrap().len(),
                    fragment.text_start_byte
                );
                let combined = format!(
                    "{}{}",
                    previous["text"].as_str().unwrap(),
                    value["text"].as_str().unwrap()
                );
                previous["text"] = combined.into();
            } else {
                assert_eq!(fragment.text_start_byte, 0);
                // The first fragment preserves all original event metadata.
                if old.get("text").is_some() {
                    value["text"] = text[..fragment.text_end_byte].into();
                }
                rebuilt[fragment.event_index] = Some(value);
            }
        }
        assert_eq!(bytes, page.returned_text_bytes);
        assert_eq!(
            serde_json::to_value(&page.end_stack).unwrap(),
            serde_json::to_value(&stack).unwrap()
        );
        pages += 1;
        if let Some(next) = page.next {
            assert!((next.event, next.offset) > (cursor.event, cursor.offset));
            cursor = next;
        } else {
            break;
        }
        assert!(pages < 10_000);
    }
    assert!(stack.is_empty());
    assert_eq!(
        rebuilt.into_iter().map(Option::unwrap).collect::<Vec<_>>(),
        original
    );
    pages
}
#[test]
fn event_windows_reconstruct_gfm_with_balanced_container_context_and_decoded_text_offsets() {
    let body = format!(
        "<!-- ss:section:SEC-ONE -->\n# Heading\n\n**{}** &amp; `code`\n\n- [x] parent\n  - child\n\n> quote\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n```rust\n{}\n```\n",
        "שָׁלוֹם 👩🏽‍💻 ".repeat(200),
        "code\t😀 ".repeat(300)
    );
    let document = prepare(&source(&body)).unwrap();
    let mut total_pages = 0;
    for (index, unit) in document.units.iter().enumerate() {
        if unit.protected_reason.is_none() {
            total_pages += reconstruct(&document, index, 1025);
        }
    }
    assert!(total_pages > document.units.len());
}
#[test]
fn encoded_budget_handles_control_text_and_huge_code_lines_without_truncation() {
    let body = format!("```\n{}\n```\n", "\u{1}".repeat(100_000));
    let document = prepare(&source(&body)).unwrap();
    let index = document
        .units
        .iter()
        .position(|u| u.kind == UnitKind::Code)
        .unwrap();
    assert!(reconstruct(&document, index, 65536) > 1);
    let page = document
        .event_window(index, EventCursor::default(), 65536)
        .unwrap();
    assert!(page.returned_text_bytes < 65536);
    let unicode = prepare(&source(&format!("```\n{}\n```\n", "👩🏽‍💻".repeat(10_000)))).unwrap();
    let index = unicode
        .units
        .iter()
        .position(|u| u.kind == UnitKind::Code)
        .unwrap();
    assert!(reconstruct(&unicode, index, 8192) > 1);
}
#[test]
fn row_pages_keep_complete_metadata_and_make_raw_fallback_explicit() {
    let old = source("<!-- ss:section:SEC-ONE -->\n# Before\n\nold\n");
    let new = source("<!-- ss:section:SEC-ONE -->\n# After\n\nnew\n");
    let pair = ReadablePair::prepare(Some(&old), Some(&new));
    let mut offset = 0;
    let mut rows = vec![];
    loop {
        let page = pair.page(offset, 1).unwrap();
        assert!(page.ready && page.raw_changed);
        assert_eq!(page.before.state, "READY");
        assert_eq!(page.before.sha256.as_deref(), pair.before.sha256());
        for row in page.rows {
            assert_eq!(
                row.before.as_ref().map(|u| u.index),
                row.change.before_index
            );
            assert_eq!(row.after.as_ref().map(|u| u.index), row.change.after_index);
            assert!(serde_json::to_vec(&row).unwrap().len() <= 4096);
            rows.push(row.change);
        }
        if let Some(next) = page.next_offset {
            assert!(next > offset);
            offset = next;
        } else {
            break;
        }
    }
    assert_eq!(rows, pair.changes);
    let missing = ReadablePair::prepare(None, Some(&new)).page(0, 10).unwrap();
    assert_eq!(missing.before.state, "ABSENT");
    let invalid = ReadablePair::prepare(Some("broken"), Some(&new))
        .page(0, 10)
        .unwrap();
    assert!(!invalid.ready && invalid.rows.is_empty());
    assert_eq!(invalid.before.state, "RAW_REQUIRED");
    assert_eq!(
        invalid.before.error_code.as_deref(),
        Some("FRONT_MATTER_REQUIRED")
    );
    assert_eq!(invalid.after.state, "READY");
}
#[test]
fn protected_source_and_atomic_oversized_attributes_request_raw_fallback_explicitly() {
    let document = prepare(&source("hello\n")).unwrap();
    assert!(matches!(
        document.event_window(0, EventCursor::default(), 1024),
        Err(ReadableWindowError::ProtectedSource)
    ));
    let large = prepare(&source(&format!(
        "[link](https://example.com/{})\n",
        "x".repeat(40_000)
    )))
    .unwrap();
    let mut cursor = EventCursor::default();
    loop {
        match large.event_window(1, cursor, 1024) {
            Ok(page) => {
                cursor = page.next.expect("oversized link cannot silently finish");
            }
            Err(ReadableWindowError::MetadataTooLarge) => break,
            _ => panic!("expected bounded raw fallback"),
        }
    }
    let image = prepare(&source(&format!(
        "![{}](assets/{}.png)\n",
        "a".repeat(70_000),
        "0".repeat(64)
    )))
    .unwrap();
    let page = image
        .event_window(1, EventCursor::default(), 65536)
        .unwrap();
    assert!(matches!(
        image.event_window(1, page.next.unwrap(), 65536),
        Err(ReadableWindowError::MetadataTooLarge)
    ));
}
#[test]
fn malformed_coordinates_and_evidence_fail_without_claiming_a_complete_window() {
    let document = prepare(&source("😀\n")).unwrap();
    let unit = &document.units[1];
    let text = unit
        .events
        .iter()
        .position(|e| e.kind == RenderKind::Text)
        .unwrap();
    for cursor in [
        EventCursor {
            event: usize::MAX,
            offset: 0,
        },
        EventCursor {
            event: text,
            offset: 1,
        },
        EventCursor {
            event: text,
            offset: unit.events[text].text.as_ref().unwrap().len(),
        },
        EventCursor {
            event: unit.events.len(),
            offset: 1,
        },
    ] {
        assert!(matches!(
            document.event_window(1, cursor, 1024),
            Err(ReadableWindowError::InvalidCursor)
        ));
    }
    for limit in [0, 1023, 65537, usize::MAX] {
        assert!(matches!(
            document.event_window(1, EventCursor::default(), limit),
            Err(ReadableWindowError::InvalidLimit)
        ));
    }
    let terminal = document
        .event_window(
            1,
            EventCursor {
                event: unit.events.len(),
                offset: 0,
            },
            1024,
        )
        .unwrap();
    assert!(terminal.next.is_none() && terminal.fragments.is_empty());
    let pair = ReadablePair::prepare(None, Some(&source("hello\n")));
    assert!(matches!(
        pair.page(MAX_ROWS + 1, 10),
        Err(ReadableWindowError::InvalidCursor)
    ));
    assert!(matches!(
        pair.page(0, 0),
        Err(ReadableWindowError::InvalidLimit)
    ));
    let mut invalid = prepare(&source("hello\n")).unwrap();
    invalid.units[1].events[0].tag = None;
    assert!(matches!(
        invalid.event_window(1, EventCursor::default(), 1024),
        Err(ReadableWindowError::InvalidEvidence)
    ));
}

#[test]
fn code_language_context_survives_resumption_and_unavailable_sides_stay_explicit() {
    let document = prepare(&source(&format!(
        "> ```rust\n> {}\n> ```\n",
        "code ".repeat(1000)
    )))
    .unwrap();
    let first = document
        .event_window(1, EventCursor::default(), 1024)
        .unwrap();
    assert_eq!(first.code_info.len(), 1);
    assert_eq!(first.code_info[0].info, "rust");
    let next = document.event_window(1, first.next.unwrap(), 1024).unwrap();
    assert_eq!(next.code_info.len(), 1);
    assert_eq!(next.code_info[0].info, "rust");
    assert_eq!(
        next.code_info[0].event_index,
        first.code_info[0].event_index
    );
    let pair = ReadablePair::prepare(None, Some("broken"));
    assert!(matches!(
        pair.before.event_window(0, EventCursor::default(), 1024),
        Err(ReadableWindowError::Unavailable)
    ));
    assert!(matches!(
        pair.after.event_window(0, EventCursor::default(), 1024),
        Err(ReadableWindowError::Unavailable)
    ));
}

#[test]
fn ordered_list_windows_preserve_item_numbers_inside_and_between_items() {
    let body = (10..30)
        .map(|n| format!("{n}. item {n} {}\n", "😀 text ".repeat(40)))
        .collect::<String>();
    let document = prepare(&source(&body)).unwrap();
    let unit_index = document
        .units
        .iter()
        .position(|unit| unit.kind == UnitKind::List)
        .unwrap();
    assert!(reconstruct(&document, unit_index, 1024) > 1);
    let unit = &document.units[unit_index];
    let items: Vec<_> = unit
        .events
        .iter()
        .enumerate()
        .filter(|(_, event)| event.kind == RenderKind::Open && event.tag == Some(RenderTag::Item))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(items.len(), 20);
    for (ordinal, index) in items.iter().copied().enumerate() {
        for event in [index, index + 1] {
            let window = document
                .event_window(unit_index, EventCursor { event, offset: 0 }, 1024)
                .unwrap();
            assert_eq!(window.start_ordered_lists.len(), 1);
            assert_eq!(
                window.start_ordered_lists[0].item_start,
                10 + ordinal as u64
            );
        }
    }
    let nested = prepare(&source(
        "1. parent\n\n   5. nested\n   6. next\n\n2. last\n",
    ))
    .unwrap();
    let index = nested
        .units
        .iter()
        .position(|unit| unit.kind == UnitKind::List)
        .unwrap();
    reconstruct(&nested, index, 1024);
    let items: Vec<_> = nested.units[index]
        .events
        .iter()
        .enumerate()
        .filter(|(_, event)| event.kind == RenderKind::Open && event.tag == Some(RenderTag::Item))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(items.len(), 4);
    let resumed = nested
        .event_window(
            index,
            EventCursor {
                event: items[2] + 1,
                offset: 0,
            },
            1024,
        )
        .unwrap();
    assert_eq!(
        resumed
            .start_ordered_lists
            .iter()
            .map(|list| list.item_start)
            .collect::<Vec<_>>(),
        [1, 6]
    );
}
