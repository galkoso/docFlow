//! Exact source evidence for discussions. Persistence must additionally bind this
//! evidence to an authorized repository, snapshot, side, path and blob. A text
//! match alone never authorizes a read or moves a discussion to another revision.
use crate::markdown;
use pulldown_cmark::{Event, Options, Parser, Tag};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{collections::HashSet, ops::Range};

const MAX_SOURCE_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_SELECTED_LINES: usize = 500;
pub const MAX_QUOTE_CHARACTERS: usize = 1_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Selection {
    Document {
        document_id: String,
    },
    Section {
        document_id: String,
        section_id: String,
    },
    Lines,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AnchorError {
    SourceTooLarge,
    InvalidDocument,
    DocumentMismatch,
    AmbiguousSections,
    SectionNotFound,
    InvalidLines,
}

/// Constructible only by selecting from the original source. No Deserialize:
/// a browser-supplied quote, digest or byte offset is never source evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TextAnchor {
    selection: Selection,
    source_sha256: String,
    range_sha256: String,
    start_byte: usize,
    end_byte: usize,
    start_line: usize,
    end_line: usize,
    quote: String,
    quote_truncated: bool,
}

impl TextAnchor {
    pub fn document(source: &str, document_id: &str) -> Result<Self, AnchorError> {
        document_body(source, document_id)?;
        Ok(Self::capture(
            source,
            Selection::Document {
                document_id: document_id.into(),
            },
            0..source.len(),
        ))
    }

    /// Select the marked heading and its complete source up to the next heading
    /// marker. Images, blank lines, code and non-visible Markdown remain intact.
    /// Section IDs are resolved across the whole document, never a changed-file
    /// manifest or rendered text search.
    pub fn section(source: &str, document_id: &str, section_id: &str) -> Result<Self, AnchorError> {
        let body_offset = document_body(source, document_id)?;
        let mut pending = None;
        let mut seen = HashSet::new();
        let mut selected = None;
        let mut selected_end = source.len();
        let options =
            Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
        for (event, range) in Parser::new_ext(&source[body_offset..], options).into_offset_iter() {
            match event {
                Event::Html(html) | Event::InlineHtml(html) => {
                    if let Some(id) = markdown::marker(&html) {
                        pending = Some((id.to_owned(), body_offset + range.start));
                    }
                }
                Event::Start(Tag::Heading { .. }) => {
                    let (id, start) = pending.take().ok_or(AnchorError::AmbiguousSections)?;
                    if !seen.insert(id.clone()) {
                        return Err(AnchorError::AmbiguousSections);
                    }
                    if selected.is_some() && selected_end == source.len() {
                        selected_end = start;
                    }
                    if id == section_id {
                        selected = Some(start);
                    }
                }
                _ => {}
            }
        }
        let start = selected.ok_or(AnchorError::SectionNotFound)?;
        Ok(Self::capture(
            source,
            Selection::Section {
                document_id: document_id.into(),
                section_id: section_id.into(),
            },
            start..selected_end,
        ))
    }

    /// One-based inclusive lines on an existing text side. The final newline
    /// does not invent another selectable line; an empty source has no lines.
    pub fn lines(source: &str, start: usize, end: usize) -> Result<Self, AnchorError> {
        check_size(source)?;
        if start == 0 || end < start || end - start >= MAX_SELECTED_LINES {
            return Err(AnchorError::InvalidLines);
        }
        let mut offset = 0;
        let mut selected_start = None;
        for (index, line) in source.split_inclusive('\n').enumerate() {
            let number = index + 1;
            if number == start {
                selected_start = Some(offset);
            }
            offset += line.len();
            if number == end {
                return Ok(Self::capture(
                    source,
                    Selection::Lines,
                    selected_start.ok_or(AnchorError::InvalidLines)?..offset,
                ));
            }
        }
        Err(AnchorError::InvalidLines)
    }

    fn capture(source: &str, selection: Selection, range: Range<usize>) -> Self {
        // All callers derive these UTF-8 boundaries directly from parser/line
        // offsets, rather than accepting arbitrary browser byte positions.
        let selected = &source[range.clone()];
        let quote: String = selected.chars().take(MAX_QUOTE_CHARACTERS).collect();
        let start_line = source[..range.start]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count()
            + 1;
        let selected_lines = selected.split_inclusive('\n').count();
        Self {
            selection,
            source_sha256: digest(source),
            range_sha256: digest(selected),
            start_byte: range.start,
            end_byte: range.end,
            start_line,
            end_line: start_line + selected_lines.saturating_sub(1),
            quote_truncated: quote.len() < selected.len(),
            quote,
        }
    }

    pub fn selection(&self) -> &Selection {
        &self.selection
    }
    pub fn source_sha256(&self) -> &str {
        &self.source_sha256
    }
    pub fn range_sha256(&self) -> &str {
        &self.range_sha256
    }
    pub fn byte_range(&self) -> Range<usize> {
        self.start_byte..self.end_byte
    }
    pub fn line_range(&self) -> Range<usize> {
        self.start_line..self.end_line + 1
    }
    pub fn quote(&self) -> &str {
        &self.quote
    }
    pub fn quote_truncated(&self) -> bool {
        self.quote_truncated
    }

    /// Conservative equality only. An altered file is never searched for the
    /// same text, section ID or line number, even if that text still exists.
    pub fn matches_source(&self, source: &str) -> bool {
        source.len() <= MAX_SOURCE_BYTES && digest(source) == self.source_sha256
    }
}

fn check_size(source: &str) -> Result<(), AnchorError> {
    if source.len() > MAX_SOURCE_BYTES {
        Err(AnchorError::SourceTooLarge)
    } else {
        Ok(())
    }
}

fn document_body(source: &str, document_id: &str) -> Result<usize, AnchorError> {
    check_size(source)?;
    let (metadata, body) =
        markdown::front_matter(source).map_err(|_| AnchorError::InvalidDocument)?;
    if metadata.document_id != document_id {
        return Err(AnchorError::DocumentMismatch);
    }
    Ok(body)
}

fn digest(source: &str) -> String {
    format!("{:x}", Sha256::digest(source.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PREFIX: &str =
        "---\ndocument_id: DOC-ONE\ntitle: בדיקה\nowner_team: core\nschema_version: 1\n---\n";

    #[test]
    fn crlf_front_matter_keeps_exact_document_section_and_line_offsets() {
        let prefix = PREFIX.replace('\n', "\r\n");
        let body = "<!-- ss:section:SEC-CRLF -->\r\n# שלום 🦀\r\n\r\nExact text\r\n";
        let source = format!("{prefix}{body}");
        let (_, offset) = markdown::front_matter(&source).unwrap();
        assert_eq!(offset, prefix.len());
        let document = TextAnchor::document(&source, "DOC-ONE").unwrap();
        assert_eq!(document.quote(), source);
        assert!(document.matches_source(&source));
        let section = TextAnchor::section(&source, "DOC-ONE", "SEC-CRLF").unwrap();
        assert_eq!(section.byte_range(), prefix.len()..source.len());
        assert_eq!(section.quote(), body);
        assert_eq!(section.line_range(), 7..11);
        let parsed = markdown::parse(&source).unwrap();
        assert_eq!(parsed.source, source);
        assert!(
            parsed
                .blocks
                .iter()
                .all(|block| source.is_char_boundary(block.source_start)
                    && source.is_char_boundary(block.source_end))
        );
        let mixed = format!("{PREFIX}{body}");
        assert_eq!(
            TextAnchor::section(&mixed, "DOC-ONE", "SEC-CRLF")
                .unwrap()
                .quote(),
            body
        );
    }

    #[test]
    fn line_ranges_preserve_unicode_crlf_and_missing_final_newline() {
        let source = "first\r\nשָׁלוֹם 🦀\r\nlast";
        let anchor = TextAnchor::lines(source, 2, 3).unwrap();
        assert_eq!(anchor.quote(), "שָׁלוֹם 🦀\r\nlast");
        assert_eq!(&source[anchor.byte_range()], anchor.quote());
        assert_eq!(anchor.line_range(), 2..4);
        assert_eq!(anchor.range_sha256(), digest(anchor.quote()));
        assert!(anchor.matches_source(source));
        assert!(!anchor.matches_source(&source.replace("\r\n", "\n")));
    }

    #[test]
    fn empty_sources_and_trailing_newlines_do_not_create_phantom_lines() {
        assert_eq!(TextAnchor::lines("", 1, 1), Err(AnchorError::InvalidLines));
        assert_eq!(
            TextAnchor::lines("a\n", 2, 2),
            Err(AnchorError::InvalidLines)
        );
        assert_eq!(TextAnchor::lines("\n", 1, 1).unwrap().quote(), "\n");
        assert_eq!(TextAnchor::lines("a\n\n", 2, 2).unwrap().quote(), "\n");
    }

    #[test]
    fn range_limits_reject_invalid_and_overflowing_requests() {
        let source = "x\n".repeat(MAX_SELECTED_LINES + 1);
        for (start, end) in [
            (0, 1),
            (2, 1),
            (1, MAX_SELECTED_LINES + 1),
            (usize::MAX, usize::MAX),
        ] {
            assert_eq!(
                TextAnchor::lines(&source, start, end),
                Err(AnchorError::InvalidLines)
            );
        }
        assert!(TextAnchor::lines(&source, 1, MAX_SELECTED_LINES).is_ok());
    }

    #[test]
    fn section_retains_images_blank_lines_and_code_until_next_section_marker() {
        let first = "<!-- ss:section:SEC-A -->\n# ראשון\n\n![תמונה](assets/a.png)\n\n```md\n<!-- ss:section:SEC-B -->\n# fake\n```\n\n";
        let last = "<!-- ss:section:SEC-B -->\n## שני\n\nlast without newline";
        let source = format!("{PREFIX}{first}{last}");
        let anchor = TextAnchor::section(&source, "DOC-ONE", "SEC-A").unwrap();
        assert_eq!(anchor.quote(), first);
        assert_eq!(
            anchor.byte_range(),
            PREFIX.len()..PREFIX.len() + first.len()
        );
        assert_eq!(
            TextAnchor::section(&source, "DOC-ONE", "SEC-B")
                .unwrap()
                .quote(),
            last
        );
    }

    #[test]
    fn duplicate_or_unmarked_headings_fail_closed_even_outside_selection() {
        for tail in ["<!-- ss:section:SEC-A -->\n# duplicate\n", "# unmarked\n"] {
            let source = format!("{PREFIX}<!-- ss:section:SEC-A -->\n# first\n\n{tail}");
            assert_eq!(
                TextAnchor::section(&source, "DOC-ONE", "SEC-A"),
                Err(AnchorError::AmbiguousSections)
            );
        }
    }

    #[test]
    fn empty_heading_can_still_anchor_its_non_textual_section() {
        let section = "<!-- ss:section:SEC-A -->\n#\n\n![image](assets/a.png)\n";
        let source = format!("{PREFIX}{section}");
        assert_eq!(
            TextAnchor::section(&source, "DOC-ONE", "SEC-A")
                .unwrap()
                .quote(),
            section
        );
    }

    #[test]
    fn document_identity_and_stable_section_are_required() {
        let source = format!("{PREFIX}<!-- ss:section:SEC-A -->\n# title\n");
        assert_eq!(
            TextAnchor::document(&source, "DOC-OTHER"),
            Err(AnchorError::DocumentMismatch)
        );
        assert_eq!(
            TextAnchor::document("no metadata", "DOC-ONE"),
            Err(AnchorError::InvalidDocument)
        );
        assert_eq!(
            TextAnchor::section(&source, "DOC-ONE", "SEC-MISSING"),
            Err(AnchorError::SectionNotFound)
        );
        let full = TextAnchor::document(&source, "DOC-ONE").unwrap();
        assert_eq!(full.byte_range(), 0..source.len());
        assert_eq!(full.quote(), source);
    }

    #[test]
    fn bounded_quote_does_not_truncate_source_evidence_or_split_unicode() {
        let source = format!("{}\nlast", "א🦀".repeat(MAX_QUOTE_CHARACTERS));
        let anchor = TextAnchor::lines(&source, 1, 2).unwrap();
        assert_eq!(anchor.quote().chars().count(), MAX_QUOTE_CHARACTERS);
        assert!(anchor.quote_truncated());
        assert_eq!(anchor.byte_range(), 0..source.len());
        assert_eq!(anchor.range_sha256(), digest(&source));
        let exact = "🦀".repeat(MAX_QUOTE_CHARACTERS);
        assert!(!TextAnchor::lines(&exact, 1, 1).unwrap().quote_truncated());
    }

    #[test]
    fn matching_line_or_section_in_changed_content_never_relocates_evidence() {
        let source = format!("{PREFIX}<!-- ss:section:SEC-A -->\n# title\n\nsame text\n");
        let anchor = TextAnchor::section(&source, "DOC-ONE", "SEC-A").unwrap();
        let original = anchor.clone();
        assert!(!anchor.matches_source(&format!("{source}\nnew content")));
        assert!(!anchor.matches_source(&source.replace("same text", "new text")));
        assert_eq!(anchor, original);
        assert_eq!(
            serde_json::to_value(&anchor).unwrap()["selection"]["kind"],
            "section"
        );
    }

    #[test]
    fn source_size_is_bounded_for_every_selection() {
        let source = "x".repeat(MAX_SOURCE_BYTES + 1);
        assert_eq!(
            TextAnchor::lines(&source, 1, 1),
            Err(AnchorError::SourceTooLarge)
        );
        assert_eq!(
            TextAnchor::document(&source, "DOC-ONE"),
            Err(AnchorError::SourceTooLarge)
        );
        assert_eq!(
            TextAnchor::section(&source, "DOC-ONE", "SEC-A"),
            Err(AnchorError::SourceTooLarge)
        );
    }
}
