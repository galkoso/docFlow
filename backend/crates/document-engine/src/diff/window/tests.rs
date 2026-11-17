use super::*;
use crate::diff::compare;
fn reconstruct(old: &str, new: &str, limit: usize) -> usize {
    let diff = compare(old, new).unwrap();
    let (mut before, mut after) = (String::new(), String::new());
    let mut cursor = RawCursor::default();
    let mut pages = 0;
    loop {
        let page = diff.window(old, new, cursor, limit).unwrap();
        assert_eq!(page.cursor, cursor);
        assert_eq!(page.old_sha256, diff.old_sha256);
        assert_eq!(page.new_sha256, diff.new_sha256);
        assert_eq!(page.detailed, diff.detailed);
        assert!(page.returned_text_bytes <= limit);
        assert!(page.segments.len() <= MAX_SEGMENTS);
        let replay = diff.window(old, new, cursor, limit).unwrap();
        assert_eq!(
            serde_json::to_value(&page).unwrap(),
            serde_json::to_value(replay).unwrap()
        );
        let mut bytes = 0;
        for segment in &page.segments {
            assert_eq!(segment.old_start_byte, before.len());
            assert_eq!(segment.new_start_byte, after.len());
            assert_eq!(
                &old[segment.old_start_byte..segment.old_end_byte],
                segment.old_text
            );
            assert_eq!(
                &new[segment.new_start_byte..segment.new_end_byte],
                segment.new_text
            );
            let chunk = &diff.chunks[segment.chunk_index];
            assert_eq!(
                (segment.old_start_line, segment.old_start_column_bytes),
                advance(
                    chunk.old_start_line,
                    &old[chunk.old_start_byte..segment.old_start_byte]
                )
            );
            assert_eq!(
                (segment.new_start_line, segment.new_start_column_bytes),
                advance(
                    chunk.new_start_line,
                    &new[chunk.new_start_byte..segment.new_start_byte]
                )
            );
            match segment.kind {
                Kind::Equal => assert_eq!(segment.old_text, segment.new_text),
                Kind::Delete => assert!(segment.new_text.is_empty()),
                Kind::Insert => assert!(segment.old_text.is_empty()),
            }
            bytes += segment.old_text.len() + segment.new_text.len();
            before.push_str(&segment.old_text);
            after.push_str(&segment.new_text);
        }
        assert_eq!(bytes, page.returned_text_bytes);
        pages += 1;
        if let Some(next) = page.next {
            assert!((next.chunk, next.offset) > (cursor.chunk, cursor.offset));
            assert!(bytes > 0);
            cursor = next;
        } else {
            break;
        }
        assert!(pages < 10_000);
    }
    assert_eq!(before, old);
    assert_eq!(after, new);
    pages
}
#[test]
fn windows_reconstruct_exact_unicode_crlf_html_yaml_and_unterminated_source() {
    let old = format!(
        "---\r\nbroken yaml\r\n---\r\n{}\r\n<script>before</script>\r\nlast",
        "שָׁלוֹם 👩🏽‍💻\t".repeat(600)
    );
    let new = format!(
        "---\r\nfixed: yes\r\n---\r\n{}\r\n<script>after</script>\r\nlast\r\n",
        "שָׁלוֹם 👩🏽‍💻\t".repeat(600)
    );
    for limit in [MIN_WINDOW_BYTES, 1025, 4097, MAX_WINDOW_BYTES] {
        reconstruct(&old, &new, limit);
    }
    for (old, new) in [
        ("", ""),
        ("", "hello"),
        ("goodbye", ""),
        ("\n", "\n"),
        ("a\r\nb", "a\r\nb"),
    ] {
        reconstruct(old, new, MIN_WINDOW_BYTES);
    }
}
#[test]
fn huge_lines_and_coarse_replacements_are_complete_across_windows() {
    let old = "א".repeat(MAX_FILE_BYTES / 2);
    let new = "😀".repeat(MAX_FILE_BYTES / 4);
    assert!(reconstruct(&old, &new, MAX_WINDOW_BYTES) > 1);
    let old = (0..2000).map(|n| format!("old-{n}\n")).collect::<String>();
    let new = (0..2000).map(|n| format!("new-{n}\n")).collect::<String>();
    assert!(!compare(&old, &new).unwrap().detailed);
    assert!(reconstruct(&old, &new, 1024) > 1);
    assert!(reconstruct(&"\n".repeat(100_001), &"x\n".repeat(100_001), 8192) > 1);
}
#[test]
fn segment_count_is_bounded_and_terminal_cursors_are_explicit() {
    let old = (0..200)
        .map(|n| format!("same-{n}\nold-{n}\n"))
        .collect::<String>();
    let new = (0..200)
        .map(|n| format!("same-{n}\nnew-{n}\n"))
        .collect::<String>();
    let diff = compare(&old, &new).unwrap();
    assert!(diff.detailed);
    let first = diff
        .window(&old, &new, RawCursor::default(), MAX_WINDOW_BYTES)
        .unwrap();
    assert_eq!(first.segments.len(), MAX_SEGMENTS);
    assert!(first.next.is_some());
    assert!(reconstruct(&old, &new, MAX_WINDOW_BYTES) > 1);
    let terminal = diff
        .window(
            &old,
            &new,
            RawCursor {
                chunk: diff.chunks.len(),
                offset: 0,
            },
            1024,
        )
        .unwrap();
    assert!(terminal.next.is_none() && terminal.segments.is_empty());
}
#[test]
fn malformed_cursors_limits_sources_and_alignment_never_panic_or_claim_a_window() {
    let diff = compare("😀\n", "😀\n").unwrap();
    for cursor in [
        RawCursor {
            chunk: 0,
            offset: 1,
        },
        RawCursor {
            chunk: 0,
            offset: 5,
        },
        RawCursor {
            chunk: usize::MAX,
            offset: usize::MAX,
        },
        RawCursor {
            chunk: 1,
            offset: 1,
        },
    ] {
        assert!(matches!(
            diff.window("😀\n", "😀\n", cursor, 1024),
            Err(WindowError::InvalidCursor)
        ));
    }
    for limit in [0, 1, 1023, MAX_WINDOW_BYTES + 1, usize::MAX] {
        assert!(matches!(
            diff.window("😀\n", "😀\n", RawCursor::default(), limit),
            Err(WindowError::InvalidLimit)
        ));
    }
    assert!(matches!(
        diff.window("😁\n", "😀\n", RawCursor::default(), 1024),
        Err(WindowError::InvalidEvidence)
    ));
    for variant in 0..4 {
        let mut malformed = compare("a\nb", "a\nc").unwrap();
        match variant {
            0 => malformed.chunks[0].old_end_byte = usize::MAX,
            1 => malformed.chunks[0].new_start_byte = 1,
            2 => malformed.chunks[0].kind = Kind::Delete,
            _ => malformed.chunks[0].old_start_line = 0,
        }
        assert!(matches!(
            malformed.window("a\nb", "a\nc", RawCursor::default(), 1024),
            Err(WindowError::InvalidEvidence)
        ));
    }
}
#[test]
fn coordinates_continue_long_lines_and_move_only_after_newline_bytes() {
    let source = format!("{}\r\nend", "x".repeat(1500));
    let diff = compare(&source, &source).unwrap();
    let page = diff
        .window(
            &source,
            &source,
            RawCursor {
                chunk: 0,
                offset: 1024,
            },
            1024,
        )
        .unwrap();
    assert_eq!(
        (
            page.segments[0].old_start_line,
            page.segments[0].old_start_column_bytes
        ),
        (1, 1024)
    );
    let page = diff
        .window(
            &source,
            &source,
            RawCursor {
                chunk: 0,
                offset: 1501,
            },
            1024,
        )
        .unwrap();
    assert_eq!(
        (
            page.segments[0].old_start_line,
            page.segments[0].old_start_column_bytes
        ),
        (1, 1501)
    );
    let page = diff
        .window(
            &source,
            &source,
            RawCursor {
                chunk: 0,
                offset: 1502,
            },
            1024,
        )
        .unwrap();
    assert_eq!(
        (
            page.segments[0].old_start_line,
            page.segments[0].old_start_column_bytes
        ),
        (2, 0)
    );
    assert_eq!(page.segments[0].old_text, "end");
}

#[test]
fn a_common_suffix_cannot_overflow_the_alignment_chunk_budget() {
    let old = format!(
        "old-only\n{}suffix\n",
        (0..341)
            .map(|n| format!("same-{n}\nold-{n}\n"))
            .collect::<String>()
    );
    let new = format!(
        "{}suffix\n",
        (0..341)
            .map(|n| format!("same-{n}\nnew-{n}\n"))
            .collect::<String>()
    );
    let diff = compare(&old, &new).unwrap();
    assert!(!diff.detailed);
    assert!(diff.chunks.len() <= MAX_CHUNKS);
    reconstruct(&old, &new, 1024);
}
