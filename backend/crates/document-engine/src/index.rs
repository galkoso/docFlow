//! Rebuildable normalized text plus a compact, versioned offset map for storage.
//! No stemming or SQL wildcard interpretation is part of literal find.
use crate::find::{self, MatchPage, Segment, VisibleBlock};
use sha2::{Digest, Sha256};

pub const NORMALIZER_VERSION: i32 = 1;

#[derive(Clone, Copy, Debug)]
struct Run {
    normalized_start: u32,
    original_start: u32,
    original_byte_start: u32,
    normalized_width: u32,
    original_width: u32,
    original_byte_width: u32,
    count: u32,
}

#[derive(Debug)]
struct Checkpoint {
    normalized: u32,
    original_end: u32,
    byte_end: u32,
    encoded_offset: u32,
}

#[derive(Debug)]
pub struct IndexedText {
    normalized: String,
    map: Vec<u8>,
    checkpoints: Vec<Checkpoint>,
}

pub struct IndexedBlock<'a> {
    pub block: VisibleBlock<'a>,
    pub book_order: usize,
    pub text: &'a IndexedText,
}

impl IndexedText {
    pub fn build(original: &str) -> Result<Self, &'static str> {
        if original.len() > 2 * 1024 * 1024 {
            return Err("FILE_TOO_LARGE");
        }
        let (normalized, segments) = find::normalize(original);
        let mut runs: Vec<Run> = Vec::new();
        let mut index = 0;
        while index < segments.len() {
            let first = &segments[index];
            let mut next = index + 1;
            while next < segments.len()
                && segments[next].original_start == first.original_start
                && segments[next].original_end == first.original_end
                && segments[next].original_byte_start == first.original_byte_start
                && segments[next].original_byte_end == first.original_byte_end
            {
                next += 1;
            }
            let end = segments
                .get(next)
                .map_or(normalized.len(), |segment| segment.normalized_start)
                as u32;
            let normalized_width = end - first.normalized_start as u32;
            let original_width = first.original_end - first.original_start;
            let original_byte_width = first.original_byte_end - first.original_byte_start;
            if let Some(last) = runs.last_mut().filter(|run| {
                run.normalized_width == normalized_width
                    && run.original_width == original_width
                    && run.original_byte_width == original_byte_width
                    && run.normalized_start + run.count * run.normalized_width
                        == first.normalized_start as u32
                    && run.original_start + run.count * run.original_width == first.original_start
                    && run.original_byte_start + run.count * run.original_byte_width
                        == first.original_byte_start
            }) {
                last.count += 1;
            } else {
                runs.push(Run {
                    normalized_start: first.normalized_start as u32,
                    original_start: first.original_start,
                    original_byte_start: first.original_byte_start,
                    normalized_width,
                    original_width,
                    original_byte_width,
                    count: 1,
                });
            }
            index = next;
        }
        Ok(Self::pack(normalized, &runs))
    }

    pub fn normalized(&self) -> &str {
        &self.normalized
    }
    pub fn estimated_bytes(&self) -> usize {
        self.normalized.capacity()
            + self.map.capacity()
            + self.checkpoints.capacity() * std::mem::size_of::<Checkpoint>()
    }

    fn pack(normalized: String, runs: &[Run]) -> Self {
        let mut bytes = b"SSM1".to_vec();
        let mut previous_original = 0i64;
        let mut previous_byte = 0i64;
        let mut checkpoints = Vec::with_capacity(runs.len().div_ceil(16));
        for (index, run) in runs.iter().enumerate() {
            if index.is_multiple_of(16) {
                checkpoints.push(Checkpoint {
                    normalized: run.normalized_start,
                    original_end: previous_original as u32,
                    byte_end: previous_byte as u32,
                    encoded_offset: bytes.len() as u32,
                });
            }
            let delta_original = i64::from(run.original_start) - previous_original;
            let delta_byte = i64::from(run.original_byte_start) - previous_byte;
            for value in [
                zigzag(delta_original),
                zigzag(delta_byte),
                u64::from(run.normalized_width),
                u64::from(run.original_width),
                u64::from(run.original_byte_width),
                u64::from(run.count),
            ] {
                write_varint(&mut bytes, value);
            }
            previous_original = i64::from(run.original_start)
                + i64::from(run.original_width) * i64::from(run.count);
            previous_byte = i64::from(run.original_byte_start)
                + i64::from(run.original_byte_width) * i64::from(run.count);
        }
        Self {
            normalized,
            map: bytes,
            checkpoints,
        }
    }

    pub fn encode_map(&self) -> Vec<u8> {
        self.map.clone()
    }

    pub fn fingerprint(original: &str, normalized: &str, map: &[u8]) -> String {
        let mut hash = Sha256::new();
        hash.update(NORMALIZER_VERSION.to_le_bytes());
        for bytes in [original.as_bytes(), normalized.as_bytes(), map] {
            hash.update((bytes.len() as u64).to_le_bytes());
            hash.update(bytes);
        }
        format!("{:x}", hash.finalize())
    }

    pub fn restore(
        original: &str,
        normalized: String,
        map: &[u8],
        fingerprint: &str,
    ) -> Result<Self, &'static str> {
        if original.len() > 2 * 1024 * 1024
            || normalized.len() > 8 * 1024 * 1024
            || map.len() > 4 + 28 * 2 * 1024 * 1024
            || !map.starts_with(b"SSM1")
            || Self::fingerprint(original, &normalized, map) != fingerprint
        {
            return Err("INVALID_SEARCH_INDEX");
        }
        let original_length = original.encode_utf16().count() as u64;
        let mut checkpoints = Vec::new();
        let mut run_count = 0usize;
        let mut expected_normalized = 0u64;
        let mut previous_original = (0u64, 0u64, 0u64, 0u64);
        let mut cursor = 4;
        while cursor < map.len() {
            if run_count >= original.len() {
                return Err("INVALID_SEARCH_INDEX");
            }
            let encoded_offset = cursor;
            let run = read_run(
                map,
                &mut cursor,
                expected_normalized as u32,
                previous_original.1 as u32,
                previous_original.3 as u32,
            )?;
            let normalized_end = u64::from(run.normalized_start)
                + u64::from(run.normalized_width) * u64::from(run.count);
            let original_end = u64::from(run.original_start)
                + u64::from(run.original_width) * u64::from(run.count);
            let byte_end = u64::from(run.original_byte_start)
                + u64::from(run.original_byte_width) * u64::from(run.count);
            if u64::from(run.normalized_start) != expected_normalized
                || run.normalized_width == 0
                || run.original_width == 0
                || run.original_byte_width == 0
                || run.count == 0
                || normalized_end > normalized.len() as u64
                || original_end > original_length
                || byte_end > original.len() as u64
                || u64::from(run.original_start) < previous_original.0
                || u64::from(run.original_start) + u64::from(run.original_width)
                    < previous_original.1
                || u64::from(run.original_byte_start) < previous_original.2
                || u64::from(run.original_byte_start) + u64::from(run.original_byte_width)
                    < previous_original.3
                || !normalized.is_char_boundary(run.normalized_start as usize)
                || !normalized.is_char_boundary(normalized_end as usize)
                || !original.is_char_boundary(run.original_byte_start as usize)
                || !original.is_char_boundary(byte_end as usize)
            {
                return Err("INVALID_SEARCH_INDEX");
            }
            if run_count.is_multiple_of(16) {
                checkpoints.push(Checkpoint {
                    normalized: expected_normalized as u32,
                    original_end: previous_original.1 as u32,
                    byte_end: previous_original.3 as u32,
                    encoded_offset: encoded_offset as u32,
                });
            }
            run_count += 1;
            expected_normalized = normalized_end;
            previous_original = (
                original_end - u64::from(run.original_width),
                original_end,
                byte_end - u64::from(run.original_byte_width),
                byte_end,
            );
        }
        if expected_normalized != normalized.len() as u64 {
            return Err("INVALID_SEARCH_INDEX");
        }
        Ok(Self {
            normalized,
            map: map.to_vec(),
            checkpoints,
        })
    }
}

struct MapCursor<'a> {
    text: &'a IndexedText,
    current: Option<Run>,
}
impl MapCursor<'_> {
    fn at(&mut self, position: usize) -> Result<Segment, &'static str> {
        let fits = |run: &Run| {
            position >= run.normalized_start as usize
                && position < (run.normalized_start + run.normalized_width * run.count) as usize
        };
        if !self.current.as_ref().is_some_and(fits) {
            let index = self
                .text
                .checkpoints
                .partition_point(|checkpoint| checkpoint.normalized as usize <= position);
            let checkpoint = self
                .text
                .checkpoints
                .get(index.checked_sub(1).ok_or("INVALID_SEARCH_INDEX")?)
                .ok_or("INVALID_SEARCH_INDEX")?;
            let mut encoded = checkpoint.encoded_offset as usize;
            let mut normalized = checkpoint.normalized;
            let mut original_end = checkpoint.original_end;
            let mut byte_end = checkpoint.byte_end;
            self.current = None;
            for _ in 0..16 {
                let run = read_run(
                    &self.text.map,
                    &mut encoded,
                    normalized,
                    original_end,
                    byte_end,
                )?;
                if fits(&run) {
                    self.current = Some(run);
                    break;
                }
                normalized = run.normalized_start + run.normalized_width * run.count;
                original_end = run.original_start + run.original_width * run.count;
                byte_end = run.original_byte_start + run.original_byte_width * run.count;
            }
        }
        let run = self.current.as_ref().ok_or("INVALID_SEARCH_INDEX")?;
        let offset = (position as u32 - run.normalized_start) / run.normalized_width;
        let original_start = run.original_start + offset * run.original_width;
        let original_byte_start = run.original_byte_start + offset * run.original_byte_width;
        Ok(Segment {
            normalized_start: position,
            original_start,
            original_end: original_start + run.original_width,
            original_byte_start,
            original_byte_end: original_byte_start + run.original_byte_width,
        })
    }
}

fn read_run(
    map: &[u8],
    cursor: &mut usize,
    normalized_start: u32,
    previous_original: u32,
    previous_byte: u32,
) -> Result<Run, &'static str> {
    let mut fields = [0u64; 6];
    for field in &mut fields {
        *field = read_varint(map, cursor)?;
    }
    let original_start = i64::from(previous_original)
        .checked_add(unzigzag(fields[0]))
        .and_then(|value| u32::try_from(value).ok())
        .ok_or("INVALID_SEARCH_INDEX")?;
    let original_byte_start = i64::from(previous_byte)
        .checked_add(unzigzag(fields[1]))
        .and_then(|value| u32::try_from(value).ok())
        .ok_or("INVALID_SEARCH_INDEX")?;
    let number = |value| u32::try_from(value).map_err(|_| "INVALID_SEARCH_INDEX");
    Ok(Run {
        normalized_start,
        original_start,
        original_byte_start,
        normalized_width: number(fields[2])?,
        original_width: number(fields[3])?,
        original_byte_width: number(fields[4])?,
        count: number(fields[5])?,
    })
}

fn zigzag(value: i64) -> u64 {
    ((value << 1) ^ (value >> 63)) as u64
}
fn unzigzag(value: u64) -> i64 {
    ((value >> 1) as i64) ^ -((value & 1) as i64)
}
fn write_varint(bytes: &mut Vec<u8>, mut value: u64) {
    while value >= 128 {
        bytes.push((value as u8) | 128);
        value >>= 7;
    }
    bytes.push(value as u8);
}
fn read_varint(bytes: &[u8], cursor: &mut usize) -> Result<u64, &'static str> {
    let mut value = 0u64;
    for shift in (0..=63).step_by(7) {
        let byte = *bytes.get(*cursor).ok_or("INVALID_SEARCH_INDEX")?;
        *cursor += 1;
        if shift == 63 && byte > 1 {
            return Err("INVALID_SEARCH_INDEX");
        }
        value |= u64::from(byte & 127) << shift;
        if byte < 128 {
            return Ok(value);
        }
    }
    Err("INVALID_SEARCH_INDEX")
}

pub fn normalized_query(query: &str) -> Result<String, &'static str> {
    if query.chars().count() > 200 {
        return Err("QUERY_TOO_LONG");
    }
    Ok(find::normalize(query).0)
}

pub fn find(
    snapshot: &str,
    blocks: &[IndexedBlock<'_>],
    query: &str,
    offset: usize,
    limit: usize,
) -> Result<MatchPage, &'static str> {
    if !(1..=500).contains(&limit) {
        return Err("INVALID_LIMIT");
    }
    let needle = normalized_query(query)?;
    let mut page = MatchPage {
        snapshot_sha: snapshot.into(),
        total_matches: 0,
        matches: vec![],
        next_offset: None,
    };
    if needle.is_empty() {
        return Ok(page);
    }
    for indexed in blocks {
        let mut previous = None;
        let mut cursor = MapCursor {
            text: indexed.text,
            current: None,
        };
        for (start, _) in indexed.text.normalized.match_indices(&needle) {
            let first = cursor.at(start)?;
            let last = cursor.at(start + needle.len() - 1)?;
            let range = (first.original_start, last.original_end);
            if previous == Some(range) {
                continue;
            }
            previous = Some(range);
            let index = page.total_matches;
            page.total_matches += 1;
            if index >= offset && page.matches.len() < limit {
                if !indexed
                    .block
                    .text
                    .is_char_boundary(first.original_byte_start as usize)
                    || !indexed
                        .block
                        .text
                        .is_char_boundary(last.original_byte_end as usize)
                {
                    return Err("INVALID_SEARCH_INDEX");
                }
                page.matches.push(find::matched(
                    snapshot,
                    &indexed.block,
                    indexed.book_order,
                    &first,
                    &last,
                ));
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
            document_id: "D1",
            section_id: "S1",
            block_id: "B1",
            text,
        }
    }

    #[test]
    fn persisted_maps_keep_exact_direct_search_semantics_across_unicode_and_pagination() {
        let cases = [
            (
                "😀 \u{200f}מְחִיקָה ומחיקה",
                vec!["מחיקה", "מ", " ", "😀", "\u{200f}"],
            ),
            (
                "Cafe\u{301} A\t \nB 👩‍👩‍👧‍👦 מקף־עברי",
                vec!["CAFé", "a b", "👩", "\u{200d}", "מקף-עברי", "־"],
            ),
            ("a [x]* <tag> a [x]*", vec!["[x]*", "<tag>", "a", ""]),
            (
                "\u{1100}\u{1161}\u{11a8} \u{0344}\u{0344} \u{200e}\u{05b0}",
                vec!["각", "\u{0308}", "\u{0301}", " ", "\u{05b0}"],
            ),
            (
                " \u{0301}\t\u{0301} \r\n\u{200f} \u{05b0}x",
                vec![" ", "\u{0301}", "x", "\u{0301} "],
            ),
        ];
        for (original, queries) in cases {
            let built = IndexedText::build(original).unwrap();
            let encoded = built.encode_map();
            let fingerprint = IndexedText::fingerprint(original, built.normalized(), &encoded);
            let restored =
                IndexedText::restore(original, built.normalized().into(), &encoded, &fingerprint)
                    .unwrap();
            for query in queries {
                for offset in [0, 1, 10] {
                    let direct =
                        crate::find::find("sha", &[block(original)], query, offset, 1).unwrap();
                    let indexed = find(
                        "sha",
                        &[IndexedBlock {
                            block: block(original),
                            book_order: 0,
                            text: &restored,
                        }],
                        query,
                        offset,
                        1,
                    )
                    .unwrap();
                    assert_eq!(
                        serde_json::to_value(indexed).unwrap(),
                        serde_json::to_value(direct).unwrap(),
                        "{query:?} in {original:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn deterministic_unicode_corpus_keeps_persisted_and_direct_ranges_identical() {
        let alphabet = [
            "a",
            "A",
            "מְ",
            "א",
            "e\u{301}",
            "👩‍👩‍👧‍👦",
            "\t",
            "\n",
            "\u{200f}",
            " \u{0301}",
            "\u{0344}",
            "\r\n",
            "\u{05b0}",
        ];
        let mut seed = 12345u64;
        for _ in 0..256 {
            let mut original = String::new();
            for _ in 0..32 {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                original.push_str(alphabet[(seed >> 32) as usize % alphabet.len()]);
            }
            let built = IndexedText::build(&original).unwrap();
            let map = built.encode_map();
            let hash = IndexedText::fingerprint(&original, built.normalized(), &map);
            let text =
                IndexedText::restore(&original, built.normalized().into(), &map, &hash).unwrap();
            for query in ["a", "מ", "א", "é", "👩", "\u{0301}", " ", "a a", "\u{05b0}"] {
                let direct = crate::find::find("sha", &[block(&original)], query, 0, 100).unwrap();
                let indexed = find(
                    "sha",
                    &[IndexedBlock {
                        block: block(&original),
                        book_order: 0,
                        text: &text,
                    }],
                    query,
                    0,
                    100,
                )
                .unwrap();
                assert_eq!(
                    serde_json::to_value(indexed).unwrap(),
                    serde_json::to_value(direct).unwrap(),
                    "{query:?} in {original:?}"
                );
            }
        }
    }

    #[test]
    fn regular_mapping_runs_are_compact_and_candidate_filtering_preserves_book_order() {
        for original in [
            "a".repeat(1024 * 1024),
            "אבג".repeat(10000),
            "👩‍👩‍👧‍👦".repeat(10000),
        ] {
            let indexed = IndexedText::build(&original).unwrap();
            assert!(
                indexed.encode_map().len() <= 64,
                "regular text should not persist a map entry per character"
            );
        }
        let text = IndexedText::build("last match").unwrap();
        let page = find(
            "sha",
            &[IndexedBlock {
                block: block("last match"),
                book_order: 999,
                text: &text,
            }],
            "match",
            0,
            100,
        )
        .unwrap();
        assert_eq!(page.total_matches, 1);
        assert_eq!(page.matches[0].book_order, 999);
    }

    #[test]
    fn sparse_maps_preserve_the_persisted_format_and_fit_a_large_book_working_set() {
        let simple = IndexedText::build("abc").unwrap();
        assert_eq!(simple.encode_map(), b"SSM1\x00\x00\x01\x01\x01\x03");
        let original = "טקסט הקשר עברי רגיל לבדיקת ביצועים. ".repeat(300);
        let built = IndexedText::build(&original).unwrap();
        let map = built.encode_map();
        let fingerprint = IndexedText::fingerprint(&original, built.normalized(), &map);
        let restored =
            IndexedText::restore(&original, built.normalized().into(), &map, &fingerprint).unwrap();
        assert_eq!(restored.encode_map(), map);
        assert!((restored.estimated_bytes() + original.len() + 256) * 1000 < 128 * 1024 * 1024);
    }

    #[test]
    fn corrupt_or_incompatible_persisted_maps_fail_explicitly() {
        let original = "abcdef";
        let text = IndexedText::build(original).unwrap();
        let map = text.encode_map();
        let fingerprint = IndexedText::fingerprint(original, text.normalized(), &map);
        assert!(
            IndexedText::restore("different", text.normalized().into(), &map, &fingerprint)
                .is_err()
        );
        for mut corrupt in [map.clone(), map[..map.len() - 1].to_vec(), Vec::new()] {
            if corrupt.len() == map.len() {
                corrupt[0] = b'X';
            }
            let hash = IndexedText::fingerprint(original, text.normalized(), &corrupt);
            assert!(
                IndexedText::restore(original, text.normalized().into(), &corrupt, &hash).is_err()
            );
        }
        let mut corrupt = map;
        let length = corrupt.len();
        corrupt[length - 4..].copy_from_slice(&u32::MAX.to_le_bytes());
        let hash = IndexedText::fingerprint(original, text.normalized(), &corrupt);
        assert!(IndexedText::restore(original, text.normalized().into(), &corrupt, &hash).is_err());
        assert!(IndexedText::build(&"x".repeat(2 * 1024 * 1024 + 1)).is_err());
    }
}
