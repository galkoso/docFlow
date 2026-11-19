//! Exact, complete source partitions for section review. Parsing is not read
//! authority, a visit receipt or an approval. Every source byte stays assigned
//! to the preamble or one stable section, including protected/non-rendered text.
use crate::markdown;
use pulldown_cmark::{Event, Options, Parser, Tag};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;

pub const MAX_SECTIONS: usize = 2048;
#[derive(Debug, PartialEq, Eq)]
pub enum SectionError {
    Source(&'static str),
    DocumentMismatch,
    AmbiguousSections,
    SectionLimit,
}
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRange {
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: usize,
    pub end_line: usize,
    pub sha256: String,
}
pub struct Section {
    pub id: String,
    pub range: SourceRange,
}
pub struct Sections {
    pub source_sha256: String,
    pub preamble: SourceRange,
    pub sections: Vec<Section>,
}
pub fn prepare(source: &str, document_id: &str) -> Result<Sections, SectionError> {
    let (metadata, offset) = markdown::front_matter(source).map_err(SectionError::Source)?;
    if metadata.document_id != document_id {
        return Err(SectionError::DocumentMismatch);
    }
    // Check expansion before parsing; reference-heavy input must not bypass the
    // limits used by the readable/editor and publication pipelines.
    crate::save_validation::check_reference_expansion(source).map_err(SectionError::Source)?;
    let mut pending = None;
    let mut seen = HashSet::new();
    let mut starts = Vec::new();
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    for (event, range) in Parser::new_ext(&source[offset..], options).into_offset_iter() {
        match event {
            Event::Html(html) | Event::InlineHtml(html) => {
                if let Some(id) = markdown::marker(&html) {
                    pending = Some((id.to_owned(), offset + range.start));
                }
            }
            Event::Start(Tag::Heading { .. }) => {
                let (id, start) = pending.take().ok_or(SectionError::AmbiguousSections)?;
                if !seen.insert(id.clone()) {
                    return Err(SectionError::AmbiguousSections);
                }
                if starts.len() == MAX_SECTIONS {
                    return Err(SectionError::SectionLimit);
                }
                starts.push((id, start));
            }
            _ => {}
        }
    }
    let mut line = 1;
    let preamble = capture(
        source,
        0,
        starts.first().map_or(source.len(), |(_, start)| *start),
        &mut line,
    );
    let mut sections = Vec::with_capacity(starts.len());
    for (n, (id, start)) in starts.iter().enumerate() {
        let end = starts.get(n + 1).map_or(source.len(), |(_, start)| *start);
        sections.push(Section {
            id: id.clone(),
            range: capture(source, *start, end, &mut line),
        });
    }
    Ok(Sections {
        source_sha256: digest(source),
        preamble,
        sections,
    })
}
pub fn whole(source: &str) -> SourceRange {
    capture(source, 0, source.len(), &mut 1)
}
fn capture(source: &str, start: usize, end: usize, line: &mut usize) -> SourceRange {
    let selected = &source[start..end];
    let start_line = *line;
    *line += selected.bytes().filter(|b| *b == b'\n').count();
    SourceRange {
        start_byte: start,
        end_byte: end,
        start_line,
        end_line: start_line + selected.split_inclusive('\n').count().saturating_sub(1),
        sha256: digest(selected),
    }
}
fn digest(source: &str) -> String {
    format!("{:x}", Sha256::digest(source.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discussion_anchor::TextAnchor;
    const PREFIX: &str = "---\r\ndocument_id: DOC-ONE\r\ntitle: Original\r\nowner_team: core\r\nschema_version: 1\r\n---\r\n";
    #[test]
    fn partitions_cover_original_unicode_crlf_and_protected_source_without_gaps() {
        let source = format!(
            "{PREFIX}Preamble\r\n<!-- ss:section:SEC-A -->\r\n# שלום 🌍\r\n\r\n<script>untrusted()</script>\r\n![image](assets/a.png)\r\n<!-- ss:section:SEC-B -->\r\n## Second\r\n\r\n```md\r\n<!-- ss:section:SEC-FAKE -->\r\n# code\r\n```\r\n[ref]: https://example.com\r\nlast"
        );
        let map = prepare(&source, "DOC-ONE").unwrap();
        assert_eq!(map.sections.len(), 2);
        let ranges: Vec<_> = std::iter::once(&map.preamble)
            .chain(map.sections.iter().map(|s| &s.range))
            .collect();
        let mut end = 0;
        for range in ranges {
            assert_eq!(range.start_byte, end);
            assert!(source.is_char_boundary(range.start_byte));
            assert!(source.is_char_boundary(range.end_byte));
            assert_eq!(
                range.sha256,
                digest(&source[range.start_byte..range.end_byte])
            );
            end = range.end_byte;
        }
        assert_eq!(end, source.len());
        assert_eq!(map.source_sha256, digest(&source));
        for section in map.sections {
            let anchor = TextAnchor::section(&source, "DOC-ONE", &section.id).unwrap();
            assert_eq!(
                section.range.start_byte..section.range.end_byte,
                anchor.byte_range()
            );
            assert_eq!(
                section.range.start_line..section.range.end_line + 1,
                anchor.line_range()
            );
        }
    }
    #[test]
    fn headings_require_unique_stable_markers_and_exact_document_identity() {
        for body in [
            "# Unmarked\n",
            "<!-- ss:section:SEC-A -->\n# First\n<!-- ss:section:SEC-A -->\n# Duplicate\n",
        ] {
            assert!(matches!(
                prepare(&format!("{PREFIX}{body}"), "DOC-ONE"),
                Err(SectionError::AmbiguousSections)
            ));
        }
        assert!(matches!(
            prepare(PREFIX, "DOC-OTHER"),
            Err(SectionError::DocumentMismatch)
        ));
        let empty = prepare(PREFIX, "DOC-ONE").unwrap();
        assert!(empty.sections.is_empty());
        assert_eq!(empty.preamble.end_byte, PREFIX.len());
    }
    #[test]
    fn excessive_sections_are_explicit_and_empty_sources_have_bounded_whole_ranges() {
        let mut source = PREFIX.to_owned();
        for n in 0..=MAX_SECTIONS {
            source.push_str(&format!("<!-- ss:section:SEC-{n} -->\n# Heading\n"));
        }
        assert!(matches!(
            prepare(&source, "DOC-ONE"),
            Err(SectionError::SectionLimit)
        ));
        assert_eq!(whole("").start_line, 1);
        assert_eq!(whole("").end_byte, 0);
        assert_eq!(whole("one\r\nlast").end_line, 2);
    }
}
