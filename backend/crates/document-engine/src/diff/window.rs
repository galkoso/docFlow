//! Bounded, byte-exact materialization of an alignment. Cursors are coordinates
//! within one immutable diff, not authorization or review anchors.
use super::{Kind, MAX_CHUNKS, MAX_FILE_BYTES, TextDiff, count};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MIN_WINDOW_BYTES: usize = 1024;
pub const MAX_WINDOW_BYTES: usize = 64 * 1024;
const MAX_SEGMENTS: usize = 128;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RawCursor {
    pub chunk: usize,
    /// UTF-8 byte offset in this chunk's nonempty text (old text for equal).
    pub offset: usize,
}
#[derive(Debug, PartialEq, Eq)]
pub enum WindowError {
    InvalidCursor,
    InvalidLimit,
    InvalidEvidence,
}
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RawSegment {
    pub chunk_index: usize,
    pub kind: Kind,
    pub old_start_byte: usize,
    pub old_end_byte: usize,
    pub new_start_byte: usize,
    pub new_end_byte: usize,
    pub old_start_line: usize,
    pub new_start_line: usize,
    /// Byte columns, not grapheme, display or UTF-16 columns. A nonzero column
    /// signals a window continuing inside a line; UTF-8 scalars are never split.
    pub old_start_column_bytes: usize,
    pub new_start_column_bytes: usize,
    pub continues_chunk: bool,
    pub old_text: String,
    pub new_text: String,
}
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RawWindow {
    pub old_sha256: String,
    pub new_sha256: String,
    pub old_bytes: usize,
    pub new_bytes: usize,
    pub old_lines: usize,
    pub new_lines: usize,
    pub detailed: bool,
    pub total_chunks: usize,
    pub cursor: RawCursor,
    pub next: Option<RawCursor>,
    /// Sum of both source-text fields, before JSON escaping. Equal text counts twice.
    pub returned_text_bytes: usize,
    pub segments: Vec<RawSegment>,
}
impl TextDiff {
    pub fn window(
        &self,
        old: &str,
        new: &str,
        cursor: RawCursor,
        limit: usize,
    ) -> Result<RawWindow, WindowError> {
        if !(MIN_WINDOW_BYTES..=MAX_WINDOW_BYTES).contains(&limit) {
            return Err(WindowError::InvalidLimit);
        }
        validate(self, old, new)?;
        if cursor.chunk > self.chunks.len()
            || cursor.chunk == self.chunks.len() && cursor.offset != 0
        {
            return Err(WindowError::InvalidCursor);
        }
        let mut position = cursor;
        let mut remaining = limit;
        let mut segments = vec![];
        while let Some(chunk) = self.chunks.get(position.chunk) {
            let a = &old[chunk.old_start_byte..chunk.old_end_byte];
            let b = &new[chunk.new_start_byte..chunk.new_end_byte];
            let text = if chunk.kind == Kind::Insert { b } else { a };
            if position.offset >= text.len() || !text.is_char_boundary(position.offset) {
                return Err(WindowError::InvalidCursor);
            }
            let factor = if chunk.kind == Kind::Equal { 2 } else { 1 };
            let mut end = position.offset + (remaining / factor).min(text.len() - position.offset);
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            if end == position.offset || segments.len() == MAX_SEGMENTS {
                break;
            }
            let (a_offset, a_end) = if chunk.kind == Kind::Insert {
                (0, 0)
            } else {
                (position.offset, end)
            };
            let (b_offset, b_end) = if chunk.kind == Kind::Delete {
                (0, 0)
            } else {
                (position.offset, end)
            };
            let (old_line, old_column) = advance(chunk.old_start_line, &a[..a_offset]);
            let (new_line, new_column) = advance(chunk.new_start_line, &b[..b_offset]);
            let old_text = a[a_offset..a_end].to_owned();
            let new_text = b[b_offset..b_end].to_owned();
            remaining -= old_text.len() + new_text.len();
            segments.push(RawSegment {
                chunk_index: position.chunk,
                kind: chunk.kind,
                old_start_byte: chunk.old_start_byte + a_offset,
                old_end_byte: chunk.old_start_byte + a_end,
                new_start_byte: chunk.new_start_byte + b_offset,
                new_end_byte: chunk.new_start_byte + b_end,
                old_start_line: old_line,
                new_start_line: new_line,
                old_start_column_bytes: old_column,
                new_start_column_bytes: new_column,
                continues_chunk: end < text.len(),
                old_text,
                new_text,
            });
            position = if end == text.len() {
                RawCursor {
                    chunk: position.chunk + 1,
                    offset: 0,
                }
            } else {
                RawCursor {
                    chunk: position.chunk,
                    offset: end,
                }
            };
        }
        Ok(RawWindow {
            old_sha256: self.old_sha256.clone(),
            new_sha256: self.new_sha256.clone(),
            old_bytes: self.old_bytes,
            new_bytes: self.new_bytes,
            old_lines: self.old_lines,
            new_lines: self.new_lines,
            detailed: self.detailed,
            total_chunks: self.chunks.len(),
            cursor,
            next: (position.chunk < self.chunks.len()).then_some(position),
            returned_text_bytes: limit - remaining,
            segments,
        })
    }
}
fn advance(line: usize, prefix: &str) -> (usize, usize) {
    (
        line + prefix.bytes().filter(|b| *b == b'\n').count(),
        prefix.len() - prefix.rfind('\n').map_or(0, |n| n + 1),
    )
}
fn validate(diff: &TextDiff, old: &str, new: &str) -> Result<(), WindowError> {
    if old.len() > MAX_FILE_BYTES
        || new.len() > MAX_FILE_BYTES
        || diff.old_bytes != old.len()
        || diff.new_bytes != new.len()
        || diff.old_lines != count(old)
        || diff.new_lines != count(new)
        || diff.old_sha256 != format!("{:x}", Sha256::digest(old.as_bytes()))
        || diff.new_sha256 != format!("{:x}", Sha256::digest(new.as_bytes()))
        || diff.chunks.len() > MAX_CHUNKS
    {
        return Err(WindowError::InvalidEvidence);
    }
    let (mut old_pos, mut new_pos, mut old_line, mut new_line) = (0, 0, 1, 1);
    for chunk in &diff.chunks {
        let a = old
            .get(chunk.old_start_byte..chunk.old_end_byte)
            .ok_or(WindowError::InvalidEvidence)?;
        let b = new
            .get(chunk.new_start_byte..chunk.new_end_byte)
            .ok_or(WindowError::InvalidEvidence)?;
        if (
            chunk.old_start_byte,
            chunk.new_start_byte,
            chunk.old_start_line,
            chunk.new_start_line,
        ) != (old_pos, new_pos, old_line, new_line)
            || chunk.old_line_count != count(a)
            || chunk.new_line_count != count(b)
            || a.is_empty() && b.is_empty()
            || match chunk.kind {
                Kind::Equal => a != b,
                Kind::Delete => !b.is_empty(),
                Kind::Insert => !a.is_empty(),
            }
        {
            return Err(WindowError::InvalidEvidence);
        }
        old_pos = chunk.old_end_byte;
        new_pos = chunk.new_end_byte;
        old_line += chunk.old_line_count;
        new_line += chunk.new_line_count;
    }
    if old_pos != old.len() || new_pos != new.len() {
        return Err(WindowError::InvalidEvidence);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
