//! Exact byte-preserving line alignment. Resource fallback is explicit and never
//! omits source: coarse replacements retain ranges covering both complete files.
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;

pub const MAX_FILE_BYTES: usize = 2 * 1024 * 1024;
const MAX_LINES: usize = 100_000;
const MAX_CELLS: usize = 2_000_000;
const MAX_CHUNKS: usize = 1024;

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum Kind {
    Equal,
    Delete,
    Insert,
}
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Chunk {
    pub kind: Kind,
    /// UTF-8 byte ranges, half open. An absent side has an empty range.
    pub old_start_byte: usize,
    pub old_end_byte: usize,
    pub new_start_byte: usize,
    pub new_end_byte: usize,
    /// One-based starting lines. Counts preserve missing final newlines.
    pub old_start_line: usize,
    pub new_start_line: usize,
    pub old_line_count: usize,
    pub new_line_count: usize,
}
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TextDiff {
    pub old_sha256: String,
    pub new_sha256: String,
    pub old_bytes: usize,
    pub new_bytes: usize,
    pub old_lines: usize,
    pub new_lines: usize,
    /// False means a bounded coarse replacement, never truncated content.
    pub detailed: bool,
    pub chunks: Vec<Chunk>,
}
#[derive(Debug, PartialEq, Eq)]
pub enum DiffError {
    FileTooLarge,
}
struct Line {
    end: usize,
    id: usize,
}
fn lines<'a>(source: &'a str, ids: &mut HashMap<&'a str, usize>) -> Option<Vec<Line>> {
    let mut result = Vec::new();
    let mut end = 0;
    for line in source.split_inclusive('\n') {
        if result.len() == MAX_LINES {
            return None;
        }
        end += line.len();
        let next = ids.len();
        let id = *ids.entry(line).or_insert(next);
        result.push(Line { end, id });
    }
    Some(result)
}
#[derive(Default)]
struct Builder {
    chunks: Vec<Chunk>,
    old: usize,
    new: usize,
    old_line: usize,
    new_line: usize,
}
impl Builder {
    fn push(
        &mut self,
        kind: Kind,
        old_end: usize,
        new_end: usize,
        old_count: usize,
        new_count: usize,
    ) {
        if old_count == 0 && new_count == 0 {
            return;
        }
        if let Some(last) = self.chunks.last_mut().filter(|last| last.kind == kind) {
            last.old_end_byte = old_end;
            last.new_end_byte = new_end;
            last.old_line_count += old_count;
            last.new_line_count += new_count;
        } else {
            self.chunks.push(Chunk {
                kind,
                old_start_byte: self.old,
                old_end_byte: old_end,
                new_start_byte: self.new,
                new_end_byte: new_end,
                old_start_line: self.old_line + 1,
                new_start_line: self.new_line + 1,
                old_line_count: old_count,
                new_line_count: new_count,
            });
        }
        self.old = old_end;
        self.new = new_end;
        self.old_line += old_count;
        self.new_line += new_count;
    }
}
fn count(source: &str) -> usize {
    source.split_inclusive('\n').count()
}
fn coarse(
    old: &str,
    new: &str,
    old_prefix: usize,
    new_prefix: usize,
    old_tail: usize,
    new_tail: usize,
) -> Builder {
    let mut out = Builder::default();
    out.push(
        Kind::Equal,
        old_prefix,
        new_prefix,
        count(&old[..old_prefix]),
        count(&new[..new_prefix]),
    );
    out.push(
        Kind::Delete,
        old_tail,
        new_prefix,
        count(&old[old_prefix..old_tail]),
        0,
    );
    out.push(
        Kind::Insert,
        old_tail,
        new_tail,
        0,
        count(&new[new_prefix..new_tail]),
    );
    out.push(
        Kind::Equal,
        old.len(),
        new.len(),
        count(&old[old_tail..]),
        count(&new[new_tail..]),
    );
    out
}

pub fn compare(old: &str, new: &str) -> Result<TextDiff, DiffError> {
    if old.len() > MAX_FILE_BYTES || new.len() > MAX_FILE_BYTES {
        return Err(DiffError::FileTooLarge);
    }
    let (out, detailed) = if old == new {
        let mut out = Builder::default();
        out.push(Kind::Equal, old.len(), new.len(), count(old), count(new));
        (out, true)
    } else if old.is_empty() || new.is_empty() {
        (coarse(old, new, 0, 0, old.len(), new.len()), true)
    } else {
        align(old, new)
    };
    Ok(TextDiff {
        old_sha256: format!("{:x}", Sha256::digest(old.as_bytes())),
        new_sha256: format!("{:x}", Sha256::digest(new.as_bytes())),
        old_bytes: old.len(),
        new_bytes: new.len(),
        old_lines: count(old),
        new_lines: count(new),
        detailed,
        chunks: out.chunks,
    })
}
fn align(old: &str, new: &str) -> (Builder, bool) {
    // Intern exact complete lines once. Alignment then compares integer IDs,
    // preventing repeated multi-kilobyte string comparisons in the DP matrix.
    let mut ids = HashMap::new();
    let (Some(left), Some(right)) = (lines(old, &mut ids), lines(new, &mut ids)) else {
        return (coarse(old, new, 0, 0, old.len(), new.len()), false);
    };
    let mut prefix = 0;
    while prefix < left.len().min(right.len()) && left[prefix].id == right[prefix].id {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < (left.len() - prefix).min(right.len() - prefix)
        && left[left.len() - 1 - suffix].id == right[right.len() - 1 - suffix].id
    {
        suffix += 1;
    }
    let (n, m) = (left.len() - prefix - suffix, right.len() - prefix - suffix);
    let old_prefix = if prefix == 0 { 0 } else { left[prefix - 1].end };
    let new_prefix = if prefix == 0 {
        0
    } else {
        right[prefix - 1].end
    };
    let old_tail = if n + prefix == 0 {
        0
    } else {
        left[n + prefix - 1].end
    };
    let new_tail = if m + prefix == 0 {
        0
    } else {
        right[m + prefix - 1].end
    };
    if n == 0 || m == 0 {
        return (
            coarse(old, new, old_prefix, new_prefix, old_tail, new_tail),
            true,
        );
    }
    let Some(cells) = (n + 1)
        .checked_mul(m + 1)
        .filter(|cells| *cells <= MAX_CELLS)
    else {
        return (
            coarse(old, new, old_prefix, new_prefix, old_tail, new_tail),
            false,
        );
    };
    let width = m + 1;
    let mut lcs = vec![0u32; cells];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i * width + j] = if left[prefix + i].id == right[prefix + j].id {
                1 + lcs[(i + 1) * width + j + 1]
            } else {
                lcs[(i + 1) * width + j].max(lcs[i * width + j + 1])
            };
        }
    }
    let mut out = Builder::default();
    out.push(Kind::Equal, old_prefix, new_prefix, prefix, prefix);
    let (mut i, mut j) = (0, 0);
    while i < n || j < m {
        if i < n && j < m && left[prefix + i].id == right[prefix + j].id {
            out.push(
                Kind::Equal,
                left[prefix + i].end,
                right[prefix + j].end,
                1,
                1,
            );
            i += 1;
            j += 1;
        } else if i < n && (j == m || lcs[(i + 1) * width + j] >= lcs[i * width + j + 1]) {
            out.push(Kind::Delete, left[prefix + i].end, out.new, 1, 0);
            i += 1;
        } else {
            out.push(Kind::Insert, out.old, right[prefix + j].end, 0, 1);
            j += 1;
        }
        if out.chunks.len() > MAX_CHUNKS {
            return (
                coarse(old, new, old_prefix, new_prefix, old_tail, new_tail),
                false,
            );
        }
    }
    out.push(Kind::Equal, old.len(), new.len(), suffix, suffix);
    (out, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn check(old: &str, new: &str) -> TextDiff {
        let diff = compare(old, new).unwrap();
        let (mut old_pos, mut new_pos, mut old_line, mut new_line) = (0, 0, 1, 1);
        let (mut before, mut after) = (String::new(), String::new());
        for chunk in &diff.chunks {
            assert_eq!(
                (chunk.old_start_byte, chunk.new_start_byte),
                (old_pos, new_pos)
            );
            assert_eq!(
                (chunk.old_start_line, chunk.new_start_line),
                (old_line, new_line)
            );
            let a = &old[chunk.old_start_byte..chunk.old_end_byte];
            let b = &new[chunk.new_start_byte..chunk.new_end_byte];
            assert_eq!(
                (count(a), count(b)),
                (chunk.old_line_count, chunk.new_line_count)
            );
            match chunk.kind {
                Kind::Equal => assert_eq!(a, b),
                Kind::Delete => assert!(b.is_empty()),
                Kind::Insert => assert!(a.is_empty()),
            }
            before.push_str(a);
            after.push_str(b);
            old_pos = chunk.old_end_byte;
            new_pos = chunk.new_end_byte;
            old_line += chunk.old_line_count;
            new_line += chunk.new_line_count;
        }
        assert_eq!(before, old);
        assert_eq!(after, new);
        assert_eq!((diff.old_bytes, diff.new_bytes), (old.len(), new.len()));
        assert_eq!((diff.old_lines, diff.new_lines), (count(old), count(new)));
        diff
    }
    #[test]
    fn raw_markdown_preserves_hebrew_marks_emoji_crlf_yaml_html_and_missing_final_newline() {
        let old = "---\r\nowner: private\r\n---\r\n# שָׁלוֹם 👨‍👩‍👧‍👦\r\n<script>x</script>\r\nlast";
        let new = "---\r\nowner: private\r\n---\r\n# שלום 👨‍👩‍👧‍👦\r\n<script>x</script>\r\nlast\r\n";
        let diff = check(old, new);
        assert!(diff.detailed);
        assert_eq!(diff.chunks[0].kind, Kind::Equal);
        assert_eq!(diff.chunks[0].old_line_count, 3);
        assert_ne!(diff.old_sha256, diff.new_sha256);
        assert!(check("", "").chunks.is_empty());
        assert_eq!(check("", "\n").chunks[0].kind, Kind::Insert);
        assert_eq!(check("\n", "").chunks[0].kind, Kind::Delete);
        assert_eq!(check("abc", "abc").chunks[0].old_line_count, 1);
    }
    #[test]
    fn detailed_alignment_is_minimal_for_every_short_binary_line_sequence() {
        let mut sequences = vec![Vec::new()];
        for length in 1..=5 {
            for bits in 0..(1 << length) {
                sequences.push(
                    (0..length)
                        .map(|n| if bits & (1 << n) == 0 { "a\n" } else { "b\n" })
                        .collect::<Vec<_>>(),
                );
            }
        }
        for a in &sequences {
            for b in &sequences {
                // Independent brute-force subsequence oracle, not the DP algorithm.
                let common = (0..(1 << a.len()))
                    .map(|mask| {
                        let candidate: Vec<_> = a
                            .iter()
                            .enumerate()
                            .filter(|(n, _)| mask & (1 << n) != 0)
                            .map(|(_, v)| *v)
                            .collect();
                        let mut wanted = candidate.iter();
                        let mut next = wanted.next();
                        for line in b {
                            if next == Some(line) {
                                next = wanted.next();
                            }
                        }
                        if next.is_none() { candidate.len() } else { 0 }
                    })
                    .max()
                    .unwrap();
                let diff = check(&a.concat(), &b.concat());
                assert!(diff.detailed);
                let changed: usize = diff
                    .chunks
                    .iter()
                    .filter(|c| c.kind != Kind::Equal)
                    .map(|c| c.old_line_count + c.new_line_count)
                    .sum();
                assert_eq!(changed, a.len() + b.len() - 2 * common);
            }
        }
    }
    #[test]
    fn adversarial_work_and_output_fallback_remain_complete_and_keep_common_edges() {
        let old = format!(
            "prefix\n{}suffix\n",
            (0..4000).map(|n| format!("old-{n}\n")).collect::<String>()
        );
        let new = format!(
            "prefix\n{}suffix\n",
            (0..4000).map(|n| format!("new-{n}\n")).collect::<String>()
        );
        let diff = check(&old, &new);
        assert!(!diff.detailed);
        assert_eq!(diff.chunks.len(), 4);
        assert_eq!(diff.chunks[0].kind, Kind::Equal);
        assert_eq!(diff.chunks[3].kind, Kind::Equal);
        let old = (0..700)
            .map(|n| format!("same-{n}\nold-{n}\n"))
            .collect::<String>();
        let new = (0..700)
            .map(|n| format!("same-{n}\nnew-{n}\n"))
            .collect::<String>();
        let diff = check(&old, &new);
        assert!(!diff.detailed);
        assert!(diff.chunks.len() <= 4);
    }
    #[test]
    fn excessive_line_counts_and_full_file_replacement_never_hide_bytes_and_size_limits_are_explicit()
     {
        let many = "\n".repeat(MAX_LINES + 1);
        let diff = check(&many, &format!("x{many}"));
        assert!(!diff.detailed);
        let old = "a".repeat(MAX_FILE_BYTES);
        let new = "b".repeat(MAX_FILE_BYTES);
        let diff = check(&old, &new);
        assert!(diff.detailed);
        assert_eq!(diff.chunks.len(), 2);
        assert_eq!(
            compare(&(old + "a"), "").unwrap_err(),
            DiffError::FileTooLarge
        );
    }
}
