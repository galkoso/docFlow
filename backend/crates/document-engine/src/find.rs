use serde::Serialize;
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

/// Paragraphs, table cells, headings and code blocks are independent boundaries.
pub struct VisibleBlock<'a> {
    pub document_id: &'a str,
    pub section_id: &'a str,
    pub block_id: &'a str,
    pub text: &'a str,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Match {
    pub match_id: String,
    pub document_id: String,
    pub section_id: String,
    pub block_id: String,
    pub start_utf16: u32,
    pub end_utf16: u32,
    pub book_order: usize,
    pub excerpt: String,
}

#[derive(Debug, Serialize)]
pub struct MatchPage {
    pub snapshot_sha: String,
    pub total_matches: usize,
    pub matches: Vec<Match>,
    pub next_offset: Option<usize>,
}

struct Segment {
    normalized_start: usize,
    original_start: u32,
    original_end: u32,
    original_byte_start: u32,
    original_byte_end: u32,
}

fn ignored(c: char) -> bool {
    matches!(c,
        '\u{0591}'..='\u{05bd}' | '\u{05bf}' | '\u{05c1}'..='\u{05c2}' |
        '\u{05c4}'..='\u{05c5}' | '\u{05c7}' | '\u{061c}' |
        '\u{200e}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

fn normalize(text: &str) -> (String, Vec<Segment>) {
    let mut normalized = String::with_capacity(text.len());
    let mut segments: Vec<Segment> = Vec::new();
    let mut original_offset = 0;
    let mut last_space = false;
    for (byte_start, grapheme) in text.grapheme_indices(true) {
        let start = original_offset;
        original_offset += grapheme.encode_utf16().count() as u32;
        for c in grapheme.nfc().filter(|&c| !ignored(c)) {
            let c = if c.is_whitespace() {
                ' '
            } else {
                c.to_ascii_lowercase()
            };
            if c == ' ' && last_space {
                if let Some(segment) = segments.last_mut() {
                    segment.original_end = original_offset;
                    segment.original_byte_end = (byte_start + grapheme.len()) as u32;
                }
                continue;
            }
            segments.push(Segment {
                normalized_start: normalized.len(),
                original_start: start,
                original_end: original_offset,
                original_byte_start: byte_start as u32,
                original_byte_end: (byte_start + grapheme.len()) as u32,
            });
            normalized.push(c);
            last_space = c == ' ';
        }
    }
    (normalized, segments)
}

/// Blocks must come from one complete authorized snapshot, in catalog order.
/// Normalization maps are released per block; only the requested page is retained.
/// The service layer binds the offset to query+snapshot in an opaque cursor.
pub fn find(
    snapshot_sha: &str,
    blocks: &[VisibleBlock<'_>],
    query: &str,
    offset: usize,
    limit: usize,
) -> Result<MatchPage, &'static str> {
    if query.chars().count() > 200 {
        return Err("QUERY_TOO_LONG");
    }
    if !(1..=500).contains(&limit) {
        return Err("INVALID_LIMIT");
    }
    let needle = normalize(query).0;
    let mut page = MatchPage {
        snapshot_sha: snapshot_sha.into(),
        total_matches: 0,
        matches: vec![],
        next_offset: None,
    };
    if needle.is_empty() {
        return Ok(page);
    }
    for (book_order, block) in blocks.iter().enumerate() {
        if block.text.len() > 2 * 1024 * 1024 {
            return Err("FILE_TOO_LARGE");
        }
        let (haystack, map) = normalize(block.text);
        let mut previous_range = None;
        for (start, _) in haystack.match_indices(&needle) {
            let end = start + needle.len();
            let first = map.partition_point(|segment| segment.normalized_start <= start) - 1;
            let last = map.partition_point(|segment| segment.normalized_start < end) - 1;
            let range = (map[first].original_start, map[last].original_end);
            if previous_range == Some(range) {
                continue;
            }
            previous_range = Some(range);
            let index = page.total_matches;
            page.total_matches += 1;
            if index >= offset && page.matches.len() < limit {
                let byte_start = map[first].original_byte_start as usize;
                let byte_end = map[last].original_byte_end as usize;
                let excerpt_start = block.text[..byte_start]
                    .grapheme_indices(true)
                    .rev()
                    .take(60)
                    .last()
                    .map_or(byte_start, |(start, _)| start);
                let excerpt_end = block.text[byte_end..]
                    .grapheme_indices(true)
                    .take(60)
                    .last()
                    .map_or(byte_end, |(start, text)| byte_end + start + text.len());
                let mut identity = Sha256::new();
                for part in [
                    snapshot_sha,
                    block.document_id,
                    block.section_id,
                    block.block_id,
                ] {
                    identity.update(part.as_bytes());
                    identity.update([0]);
                }
                identity.update(range.0.to_le_bytes());
                identity.update(range.1.to_le_bytes());
                page.matches.push(Match {
                    match_id: format!("{:x}", identity.finalize()),
                    document_id: block.document_id.into(),
                    section_id: block.section_id.into(),
                    block_id: block.block_id.into(),
                    start_utf16: range.0,
                    end_utf16: range.1,
                    book_order,
                    excerpt: block.text[excerpt_start..excerpt_end].into(),
                });
            }
        }
    }
    if offset.saturating_add(page.matches.len()) < page.total_matches {
        page.next_offset = Some(offset.saturating_add(page.matches.len()));
    }
    Ok(page)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(text: &str) -> VisibleBlock<'_> {
        VisibleBlock {
            document_id: "DOC-A",
            section_id: "SEC-A",
            block_id: "block-1",
            text,
        }
    }

    fn substring(text: &str, found: &Match) -> String {
        String::from_utf16(
            &text.encode_utf16().collect::<Vec<_>>()
                [found.start_utf16 as usize..found.end_utf16 as usize],
        )
        .unwrap()
    }

    #[test]
    fn hebrew_marks_and_bidi_keep_original_highlight_offsets() {
        let text = "😀 \u{200f}מְחִיקָה ומחיקה";
        let page = find("commit-a", &[block(text)], "מחיקה", 0, 100).unwrap();
        assert_eq!(page.total_matches, 2);
        assert_eq!(substring(text, &page.matches[0]), "מְחִיקָה");
        assert_eq!(substring(text, &page.matches[1]), "מחיקה");
        assert_eq!(page.matches[0].start_utf16, 4);
    }

    #[test]
    fn normalization_preserves_graphemes_whitespace_and_punctuation() {
        let text = "Cafe\u{301} A\t \nB 👩‍👩‍👧‍👦 מקף־עברי";
        assert_eq!(
            find("sha", &[block(text)], "CAFé", 0, 100)
                .unwrap()
                .total_matches,
            1
        );
        let page = find("sha", &[block(text)], "a b", 0, 100).unwrap();
        assert_eq!(substring(text, &page.matches[0]), "A\t \nB");
        let emoji = find("sha", &[block(text)], "👩", 0, 100).unwrap();
        assert_eq!(emoji.total_matches, 1);
        assert_eq!(substring(text, &emoji.matches[0]), "👩‍👩‍👧‍👦");
        assert_eq!(
            find("sha", &[block(text)], "מקף-עברי", 0, 100)
                .unwrap()
                .total_matches,
            0
        );
    }

    #[test]
    fn matches_are_literal_and_never_cross_block_boundaries() {
        let blocks = [block("alpha [x]* <tag>"), block("beta")];
        assert_eq!(
            find("sha", &blocks, "[x]* <tag>", 0, 100)
                .unwrap()
                .total_matches,
            1
        );
        assert_eq!(
            find("sha", &blocks, "tag>beta", 0, 100)
                .unwrap()
                .total_matches,
            0
        );
        assert_eq!(find("sha", &blocks, "", 0, 100).unwrap().total_matches, 0);
        assert!(find("sha", &blocks, &"x".repeat(201), 0, 100).is_err());
    }

    #[test]
    fn pagination_counts_the_full_snapshot_including_unrendered_blocks() {
        let blocks = [
            block("match match"),
            block("hidden match"),
            block("final match"),
        ];
        let first = find("snapshot-1", &blocks, "match", 0, 2).unwrap();
        assert_eq!(first.total_matches, 4);
        assert_eq!(first.next_offset, Some(2));
        let second = find("snapshot-1", &blocks, "match", 2, 2).unwrap();
        assert_eq!(second.total_matches, 4);
        assert_eq!(second.matches[0].book_order, 1);
        assert_eq!(second.matches[1].book_order, 2);
        assert_eq!(second.next_offset, None);
    }

    #[test]
    fn excerpts_surround_the_match_without_cutting_graphemes_and_ids_are_snapshot_bound() {
        let text = format!("{} מְחִיקָה {}", "👩‍👩‍👧‍👦".repeat(100), "😀".repeat(100));
        let first = find("sha-one", &[block(&text)], "מחיקה", 0, 100).unwrap();
        let again = find("sha-one", &[block(&text)], "מחיקה", 0, 100).unwrap();
        let next = find("sha-two", &[block(&text)], "מחיקה", 0, 100).unwrap();
        assert_eq!(first.matches[0].match_id, again.matches[0].match_id);
        assert_ne!(first.matches[0].match_id, next.matches[0].match_id);
        assert!(first.matches[0].excerpt.contains("מְחִיקָה"));
        assert!(first.matches[0].excerpt.starts_with("👩‍👩‍👧‍👦"));
        assert!(first.matches[0].excerpt.ends_with('😀'));
        assert!(first.matches[0].excerpt.graphemes(true).count() < 150);
    }
}
