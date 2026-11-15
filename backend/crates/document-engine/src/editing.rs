//! Lossless Markdown editing boundary. Unchanged blocks reuse original bytes;
//! rich-editor replacements are bounded, parsed blocks rather than a rewrite of
//! the document. Unsupported source is retained as protected atoms.
use crate::markdown::{self, Document};
use pulldown_cmark::{Event, Options, Parser, Tag};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    ops::Range,
};

pub const MAX_EDITOR_BLOCKS: usize = 10_000;
const MAX_SOURCE_BYTES: usize = 2 * 1024 * 1024;
const FRAGMENT_HEADER: &str =
    "---\ndocument_id: X\ntitle: X\nowner_team: X\nschema_version: 1\n---\n";

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum EditorKind {
    Heading,
    Paragraph,
    List,
    Quote,
    Code,
    Table,
    Protected,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct EditorBlock {
    pub key: String,
    pub kind: EditorKind,
    /// Immutable source before this block, including its stable heading marker.
    pub prefix: String,
    pub markdown: String,
    pub source_start_byte: usize,
    pub source_end_byte: usize,
    pub section_id: Option<String>,
    pub protected_reason: Option<String>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct EditorDocument {
    pub document_id: String,
    pub sha256: String,
    /// Exact YAML source, including unknown fields and original layout.
    pub preamble: String,
    pub blocks: Vec<EditorBlock>,
    pub suffix: String,
    /// Includes headings nested inside lists/quotes, retaining duplicates so the
    /// assembler can reject them across the complete document.
    pub section_ids: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum EditorBlockEdit {
    Keep { key: String },
    Replace { key: String, markdown: String },
    Insert { markdown: String },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct EditorEdit {
    pub expected_sha256: String,
    /// Complete new order. Missing old blocks require an explicit deletion below.
    pub blocks: Vec<EditorBlockEdit>,
    pub deleted_keys: Vec<String>,
    /// Includes IDs removed inside a changed list/quote, not just top-level
    /// heading deletions. The caller must confirm incoming-reference validation.
    #[serde(default)]
    pub deleted_section_ids: Vec<String>,
}

#[derive(Debug)]
pub struct EditedDocument {
    pub source: String,
    pub sha256: String,
    pub changed: bool,
    /// The repository validator must check incoming references before a commit.
    pub removed_section_ids: Vec<String>,
}

struct Span {
    range: Range<usize>,
    kind: EditorKind,
    deep: bool,
}

fn spans(source: &str, offset: usize) -> Result<Vec<Span>, &'static str> {
    let mut spans = vec![];
    let mut depth = 0usize;
    let mut current: Option<Span> = None;
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    for (event, range) in Parser::new_ext(&source[offset..], options).into_offset_iter() {
        let range = range.start + offset..range.end + offset;
        match event {
            Event::Start(tag) => {
                if depth == 0 {
                    let kind = match tag {
                        Tag::Heading { .. } => EditorKind::Heading,
                        Tag::Paragraph => EditorKind::Paragraph,
                        Tag::List(_) => EditorKind::List,
                        Tag::BlockQuote(_) => EditorKind::Quote,
                        Tag::CodeBlock(_) => EditorKind::Code,
                        Tag::Table(_) => EditorKind::Table,
                        _ => EditorKind::Protected,
                    };
                    current = Some(Span {
                        range,
                        kind,
                        deep: false,
                    });
                }
                depth += 1;
                if depth > 64
                    && let Some(span) = &mut current
                {
                    span.deep = true;
                }
            }
            Event::End(_) => {
                depth = depth.saturating_sub(1);
                if let Some(span) = &mut current {
                    span.range.end = span.range.end.max(range.end);
                }
                if depth == 0
                    && let Some(span) = current.take()
                {
                    // Heading anchors remain in the exact prefix of their heading.
                    if markdown::marker(source[span.range.clone()].trim()).is_none() {
                        spans.push(span);
                    }
                }
            }
            _ if depth == 0 && markdown::marker(source[range.clone()].trim()).is_none() => {
                spans.push(Span {
                    range,
                    kind: EditorKind::Protected,
                    deep: false,
                });
            }
            _ => {}
        }
        if spans.len() > MAX_EDITOR_BLOCKS {
            return Err("EDITOR_BLOCK_LIMIT");
        }
    }
    Ok(spans)
}

struct SourceLookup<'a> {
    document: &'a Document,
    headings: HashMap<usize, &'a str>,
    findings: Vec<&'a markdown::Finding>,
    max_end: Vec<usize>,
}
impl<'a> SourceLookup<'a> {
    fn new(document: &'a Document) -> Self {
        let headings = document
            .blocks
            .iter()
            .filter(|block| block.kind == "heading" && !block.section_id.is_empty())
            .map(|block| (block.source_start, block.section_id.as_str()))
            .collect();
        let mut findings: Vec<_> = document.findings.iter().collect();
        findings.sort_by_key(|finding| finding.source_start);
        let mut max = 0;
        let max_end = findings
            .iter()
            .map(|finding| {
                max = max.max(finding.source_end);
                max
            })
            .collect();
        Self {
            document,
            headings,
            findings,
            max_end,
        }
    }
    fn reason(&self, range: &Range<usize>) -> Option<&'static str> {
        let first = self.max_end.partition_point(|end| *end <= range.start);
        self.findings
            .get(first)
            .filter(|finding| finding.source_start < range.end)
            .map(|finding| finding.code)
    }
}

fn block(
    lookup: &SourceLookup<'_>,
    range: Range<usize>,
    prefix: &str,
    kind: EditorKind,
    deep: bool,
) -> EditorBlock {
    let protected_reason = if deep {
        Some("EDITOR_DEPTH_LIMIT")
    } else if kind == EditorKind::Protected {
        Some("UNSUPPORTED_EDITOR_BLOCK")
    } else {
        lookup.reason(&range)
    };
    let document = lookup.document;
    EditorBlock {
        key: format!(
            "{:x}",
            Sha256::digest(format!("{}:{}:{}", document.sha256, range.start, range.end))
        ),
        kind,
        prefix: prefix.into(),
        markdown: document.source[range.clone()].into(),
        source_start_byte: range.start,
        source_end_byte: range.end,
        section_id: if kind == EditorKind::Heading {
            lookup.headings.get(&range.start).map(|id| (*id).to_owned())
        } else {
            None
        },
        protected_reason: protected_reason.map(str::to_owned),
    }
}

pub fn parse(source: &str) -> Result<EditorDocument, &'static str> {
    let document = markdown::parse(source)?;
    let lookup = SourceLookup::new(&document);
    let offset = 4 + source[4..].find("\n---\n").ok_or("INVALID_FRONT_MATTER")? + 5;
    let mut blocks = vec![];
    let mut cursor = offset;
    for span in spans(source, offset)? {
        if span.range.start < cursor || span.range.end > source.len() {
            return Err("INVALID_EDITOR_RANGE");
        }
        let gap = &source[cursor..span.range.start];
        let mut prefix_start = cursor;
        // Preserve reference definitions and other parser-omitted source as its
        // own protected block. A trailing stable anchor travels with its heading.
        if !gap.trim().is_empty() {
            let anchor = if span.kind == EditorKind::Heading {
                gap.rfind("<!-- ss:section:")
                    .filter(|start| markdown::marker(gap[*start..].trim()).is_some())
            } else {
                None
            };
            let opaque_end = anchor.map_or(gap.len(), |start| gap[..start].trim_end().len());
            if opaque_end > 0 {
                blocks.push(block(
                    &lookup,
                    cursor..cursor + opaque_end,
                    "",
                    EditorKind::Protected,
                    false,
                ));
                prefix_start = cursor + opaque_end;
            }
        }
        blocks.push(block(
            &lookup,
            span.range.clone(),
            &source[prefix_start..span.range.start],
            span.kind,
            span.deep,
        ));
        cursor = span.range.end;
        if blocks.len() > MAX_EDITOR_BLOCKS {
            return Err("EDITOR_BLOCK_LIMIT");
        }
    }
    let mut suffix = &source[cursor..];
    if !suffix.trim().is_empty() {
        blocks.push(block(
            &lookup,
            cursor..source.len(),
            "",
            EditorKind::Protected,
            false,
        ));
        suffix = "";
    }
    if blocks.len() > MAX_EDITOR_BLOCKS {
        return Err("EDITOR_BLOCK_LIMIT");
    }
    Ok(EditorDocument {
        document_id: document.metadata.document_id,
        sha256: document.sha256,
        preamble: source[..offset].into(),
        blocks,
        suffix: suffix.into(),
        section_ids: document
            .blocks
            .iter()
            .filter(|block| block.kind == "heading" && !block.section_id.is_empty())
            .map(|block| block.section_id.clone())
            .collect(),
    })
}

fn append(source: &mut String, value: &str) -> Result<(), &'static str> {
    if source.len().saturating_add(value.len()) > MAX_SOURCE_BYTES {
        return Err("FILE_TOO_LARGE");
    }
    source.push_str(value);
    Ok(())
}

/// Validate one rich-editor serialization in a small synthetic document. The
/// original preamble is never copied/parsed per block, even for large YAML data.
fn fragment(
    value: &str,
    heading: Option<&str>,
) -> Result<(usize, EditorKind, Option<String>), &'static str> {
    let anchor = heading
        .map(|id| format!("<!-- ss:section:{id} -->\n"))
        .unwrap_or_default();
    let mut source = FRAGMENT_HEADER.to_owned();
    append(&mut source, &anchor)?;
    append(&mut source, value)?;
    let model = parse(&source)?;
    let [block] = model.blocks.as_slice() else {
        return Err("EDITOR_REPLACEMENT_NOT_ONE_BLOCK");
    };
    if block.protected_reason.is_some() {
        return Err("EDITOR_REPLACEMENT_UNSUPPORTED");
    }
    let start = block
        .source_start_byte
        .checked_sub(FRAGMENT_HEADER.len() + anchor.len())
        .ok_or("INVALID_EDITOR_RANGE")?;
    Ok((start, block.kind, block.section_id.clone()))
}

impl EditorDocument {
    pub fn unchanged(&self) -> EditorEdit {
        EditorEdit {
            expected_sha256: self.sha256.clone(),
            blocks: self
                .blocks
                .iter()
                .map(|block| EditorBlockEdit::Keep {
                    key: block.key.clone(),
                })
                .collect(),
            deleted_keys: vec![],
            deleted_section_ids: vec![],
        }
    }

    pub fn assemble(&self, edit: &EditorEdit) -> Result<EditedDocument, &'static str> {
        if edit.expected_sha256 != self.sha256 {
            return Err("EDITOR_BASE_MISMATCH");
        }
        if edit.blocks.len() > MAX_EDITOR_BLOCKS || edit.deleted_keys.len() > MAX_EDITOR_BLOCKS {
            return Err("EDITOR_BLOCK_LIMIT");
        }
        let by_key: HashMap<_, _> = self
            .blocks
            .iter()
            .map(|block| (block.key.as_str(), block))
            .collect();
        let mut seen = HashSet::new();
        for key in &edit.deleted_keys {
            let block = by_key.get(key.as_str()).ok_or("UNKNOWN_EDITOR_BLOCK")?;
            if block.protected_reason.is_some() {
                return Err("PROTECTED_EDITOR_BLOCK");
            }
            if !seen.insert(key.as_str()) {
                return Err("DUPLICATE_EDITOR_BLOCK");
            }
        }
        let mut source = self.preamble.clone();
        let mut expected = vec![];
        for change in &edit.blocks {
            let (key, replacement) = match change {
                EditorBlockEdit::Keep { key } => (Some(key.as_str()), None),
                EditorBlockEdit::Replace { key, markdown } => {
                    (Some(key.as_str()), Some(markdown.as_str()))
                }
                EditorBlockEdit::Insert { markdown } => (None, Some(markdown.as_str())),
            };
            if let Some(key) = key {
                let block = by_key.get(key).ok_or("UNKNOWN_EDITOR_BLOCK")?;
                if !seen.insert(key) {
                    return Err("DUPLICATE_EDITOR_BLOCK");
                }
                if replacement.is_some() && block.protected_reason.is_some() {
                    return Err("PROTECTED_EDITOR_BLOCK");
                }
                append(&mut source, &block.prefix)?;
                let start = source.len();
                if let Some(value) = replacement {
                    append(&mut source, value)?;
                    let (relative, kind, section) = fragment(value, block.section_id.as_deref())?;
                    if block.kind == EditorKind::Heading
                        && (kind != EditorKind::Heading || section != block.section_id)
                    {
                        return Err("SECTION_ID_CHANGED");
                    }
                    expected.push((start + relative, kind, section));
                } else {
                    append(&mut source, &block.markdown)?;
                    expected.push((start, block.kind, block.section_id.clone()));
                }
            } else {
                let value = replacement.ok_or("INVALID_EDITOR_EDIT")?;
                let start = source.len();
                append(&mut source, value)?;
                let (relative, kind, section) = fragment(value, None)?;
                expected.push((start + relative, kind, section));
            }
        }
        if seen.len() != self.blocks.len() {
            return Err("INCOMPLETE_EDITOR_DOCUMENT");
        }
        append(&mut source, &self.suffix)?;
        let result = parse(&source)?;
        let actual: HashMap<_, _> = result
            .blocks
            .iter()
            .map(|block| (block.source_start_byte, block))
            .collect();
        for (start, kind, section) in expected {
            let block = actual.get(&start).ok_or("EDITOR_BLOCK_BOUNDARY_CHANGED")?;
            if block.kind != kind {
                return Err("EDITOR_BLOCK_BOUNDARY_CHANGED");
            }
            if block.section_id != section {
                return Err("SECTION_ID_CHANGED");
            }
        }
        let original_sections: HashSet<_> = self.section_ids.iter().map(String::as_str).collect();
        let mut sections = HashSet::new();
        for id in result.section_ids.iter().map(String::as_str) {
            if !sections.insert(id) {
                return Err("DUPLICATE_SECTION_ID");
            }
        }
        let mut removed_section_ids: Vec<_> = original_sections
            .difference(&sections)
            .map(|id| (*id).to_owned())
            .collect();
        removed_section_ids.sort();
        let mut declared = edit.deleted_section_ids.clone();
        declared.sort();
        if declared != removed_section_ids {
            return Err("SECTION_DELETION_MISMATCH");
        }
        Ok(EditedDocument {
            changed: result.sha256 != self.sha256,
            sha256: result.sha256,
            source,
            removed_section_ids,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn source(body: &str) -> String {
        format!(
            "---\ndocument_id: DOC-EDITOR\ntitle: 'כותרת מקורית' # preserved comment\nowner_team: media\nschema_version: 1\ncustom:\n  nested: [one, two]\n---\n{body}"
        )
    }
    fn replace(model: &EditorDocument, index: usize, markdown: &str) -> EditorEdit {
        let mut edit = model.unchanged();
        edit.blocks[index] = EditorBlockEdit::Replace {
            key: model.blocks[index].key.clone(),
            markdown: markdown.into(),
        };
        edit
    }

    #[test]
    fn unchanged_round_trips_fixtures_and_all_supported_source_bytes() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../fixtures/repository/docs");
        for name in [
            "overview",
            "permissions",
            "media",
            "recordings",
            "player",
            "operations",
        ] {
            let source = std::fs::read_to_string(root.join(format!("{name}.md"))).unwrap();
            let model = parse(&source).unwrap();
            let result = model.assemble(&model.unchanged()).unwrap();
            assert_eq!(result.source, source, "{name}");
            assert!(!result.changed);
        }
        let source = source(&format!(
            "\n<!-- ss:section:SEC-FIRST -->\n# שלום עוֹלָם\n\nParagraph __bold__ *italic* ~~strike~~ [link][target] ![alt](assets/{}.png)\n\n1. numbered\n2. second\n\n- [x] task\n- nested\n  - child\n\n> quotation\n>\n> second paragraph\n\n~~~rust\n// untouched fence\nlet x = \"---\";\n~~~\n\n| first | second |\n| :--- | ---: |\n| א | ב |\n\n[target]: https://example.test/path \"title\"\n\n",
            "a".repeat(64)
        ));
        let model = parse(&source).unwrap();
        for kind in [
            EditorKind::Heading,
            EditorKind::Paragraph,
            EditorKind::List,
            EditorKind::Quote,
            EditorKind::Code,
            EditorKind::Table,
            EditorKind::Protected,
        ] {
            assert!(
                model.blocks.iter().any(|block| block.kind == kind),
                "missing {kind:?}"
            );
        }
        let result = model.assemble(&model.unchanged()).unwrap();
        assert_eq!(result.source, source);
        assert_eq!(result.sha256, model.sha256);
        assert!(result.removed_section_ids.is_empty());
    }

    #[test]
    fn heading_rename_preserves_anchor_preamble_and_every_other_block() {
        let source = source(
            "\n<!-- ss:section:SEC-KEEP -->\n## שם ישן\n\nParagraph with __old formatting__.\n\n```text\ncode stays verbatim\n```\n",
        );
        let model = parse(&source).unwrap();
        let heading = model
            .blocks
            .iter()
            .position(|block| block.kind == EditorKind::Heading)
            .unwrap();
        let result = model
            .assemble(&replace(&model, heading, "## שם חדש **מודגש**\n"))
            .unwrap();
        assert_eq!(
            result.source,
            source.replace("## שם ישן\n", "## שם חדש **מודגש**\n")
        );
        assert!(result.changed);
        assert!(result.removed_section_ids.is_empty());
        assert_eq!(
            markdown::parse(&result.source).unwrap().blocks[0].section_id,
            "SEC-KEEP"
        );
    }

    #[test]
    fn moves_keep_heading_ids_and_new_headings_require_unique_explicit_ids() {
        let source = source(
            "\n<!-- ss:section:SEC-FIRST -->\n## First\n\nFirst paragraph.\n\n<!-- ss:section:SEC-SECOND -->\n## Second\n\nSecond paragraph.\n",
        );
        let model = parse(&source).unwrap();
        assert_eq!(model.blocks.len(), 4);
        let mut edit = model.unchanged();
        edit.blocks.rotate_left(2);
        let moved = model.assemble(&edit).unwrap();
        assert!(moved.source.find("SEC-SECOND").unwrap() < moved.source.find("SEC-FIRST").unwrap());
        assert!(moved.removed_section_ids.is_empty());
        let mut edit = model.unchanged();
        edit.blocks.push(EditorBlockEdit::Insert {
            markdown: "\n<!-- ss:section:SEC-NEW -->\n#### חדש\n\n".into(),
        });
        let added = model.assemble(&edit).unwrap();
        assert_eq!(parse(&added.source).unwrap().section_ids.len(), 3);
        edit.blocks.pop();
        edit.blocks.push(EditorBlockEdit::Insert {
            markdown: "\n## Missing ID\n".into(),
        });
        assert_eq!(
            model.assemble(&edit).unwrap_err(),
            "EDITOR_REPLACEMENT_UNSUPPORTED"
        );
        edit.blocks.pop();
        edit.blocks.push(EditorBlockEdit::Insert {
            markdown: "\n<!-- ss:section:SEC-FIRST -->\n## Duplicate\n".into(),
        });
        assert_eq!(model.assemble(&edit).unwrap_err(), "DUPLICATE_SECTION_ID");
    }

    #[test]
    fn protected_html_reference_definitions_and_nested_unsupported_source_cannot_disappear() {
        let source = source(
            "\n<!-- ss:section:SEC-SAFE -->\n## Header\n\nNormal [reference][target].\n\n<script>private()</script>\n\n> text <span>unsupported</span>\n\n[target]: https://example.test/target\n",
        );
        let model = parse(&source).unwrap();
        assert_eq!(model.assemble(&model.unchanged()).unwrap().source, source);
        let protected: Vec<_> = model
            .blocks
            .iter()
            .enumerate()
            .filter(|(_, block)| block.protected_reason.is_some())
            .collect();
        assert!(protected.len() >= 3);
        for (index, block) in protected {
            assert_eq!(
                model
                    .assemble(&replace(&model, index, "replacement\n"))
                    .unwrap_err(),
                "PROTECTED_EDITOR_BLOCK"
            );
            let mut removed = model.unchanged();
            removed.blocks.remove(index);
            removed.deleted_keys.push(block.key.clone());
            assert_eq!(
                model.assemble(&removed).unwrap_err(),
                "PROTECTED_EDITOR_BLOCK"
            );
        }
    }

    #[test]
    fn omission_and_duplicate_keys_fail_and_explicit_deletion_reports_removed_ids() {
        let source = source("\n<!-- ss:section:SEC-DELETE -->\n## Delete me\n\nA paragraph.\n");
        let model = parse(&source).unwrap();
        let mut edit = model.unchanged();
        edit.blocks.remove(0);
        assert_eq!(
            model.assemble(&edit).unwrap_err(),
            "INCOMPLETE_EDITOR_DOCUMENT"
        );
        edit.deleted_keys.push(model.blocks[0].key.clone());
        edit.deleted_section_ids.push("SEC-DELETE".into());
        assert_eq!(
            model.assemble(&edit).unwrap().removed_section_ids,
            vec!["SEC-DELETE"]
        );
        let mut duplicate = model.unchanged();
        duplicate.blocks.push(EditorBlockEdit::Keep {
            key: model.blocks[0].key.clone(),
        });
        assert_eq!(
            model.assemble(&duplicate).unwrap_err(),
            "DUPLICATE_EDITOR_BLOCK"
        );
        let mut stale = model.unchanged();
        stale.expected_sha256 = "a".repeat(64);
        assert_eq!(model.assemble(&stale).unwrap_err(), "EDITOR_BASE_MISMATCH");
    }

    #[test]
    fn changed_block_boundaries_and_renumbered_headings_fail_instead_of_consuming_neighbors() {
        let source = source(
            "\n<!-- ss:section:SEC-KEEP -->\n## Heading\n\nFirst paragraph.\n\nSecond paragraph.\n",
        );
        let model = parse(&source).unwrap();
        assert!(
            model
                .assemble(&replace(&model, 0, "A paragraph instead.\n"))
                .is_err()
        );
        assert!(
            model
                .assemble(&replace(
                    &model,
                    0,
                    "<!-- ss:section:SEC-OTHER -->\n## Heading\n"
                ))
                .is_err()
        );
        assert_eq!(
            model
                .assemble(&replace(&model, 1, "one\n\ntwo\n"))
                .unwrap_err(),
            "EDITOR_REPLACEMENT_NOT_ONE_BLOCK"
        );
        let mut edit = model.unchanged();
        edit.blocks.insert(
            2,
            EditorBlockEdit::Insert {
                markdown: "text without separating newlines".into(),
            },
        );
        assert_eq!(
            model.assemble(&edit).unwrap_err(),
            "EDITOR_BLOCK_BOUNDARY_CHANGED"
        );
    }

    #[test]
    fn nested_section_duplicates_and_removed_sections_are_checked_for_the_full_document() {
        let source = source(
            "\n<!-- ss:section:SEC-TOP -->\n## Top\n\n> <!-- ss:section:SEC-NESTED -->\n> ### Nested\n>\n> quoted paragraph\n",
        );
        let model = parse(&source).unwrap();
        assert!(model.section_ids.contains(&"SEC-NESTED".into()));
        let quote = model
            .blocks
            .iter()
            .position(|block| block.kind == EditorKind::Quote)
            .unwrap();
        let renamed = replace(
            &model,
            quote,
            "> <!-- ss:section:SEC-NESTED -->\n> ### Renamed\n>\n> quoted paragraph\n",
        );
        assert!(
            model
                .assemble(&renamed)
                .unwrap()
                .removed_section_ids
                .is_empty()
        );
        let removed = replace(&model, quote, "> quoted paragraph only\n");
        assert_eq!(
            model.assemble(&removed).unwrap_err(),
            "SECTION_DELETION_MISMATCH"
        );
        let mut edit = model.unchanged();
        edit.blocks.push(EditorBlockEdit::Insert {
            markdown: "\n<!-- ss:section:SEC-NESTED -->\n## duplicate nested\n".into(),
        });
        assert_eq!(model.assemble(&edit).unwrap_err(), "DUPLICATE_SECTION_ID");
    }

    #[test]
    fn large_models_are_bounded_and_replacements_cannot_expand_past_the_source_limit() {
        let large = source(&"A paragraph.\n\n".repeat(MAX_EDITOR_BLOCKS));
        let model = parse(&large).unwrap();
        assert_eq!(model.blocks.len(), MAX_EDITOR_BLOCKS);
        assert!(!model.assemble(&model.unchanged()).unwrap().changed);
        assert_eq!(
            parse(&format!("{large}one more paragraph\n")).unwrap_err(),
            "EDITOR_BLOCK_LIMIT"
        );
        let small = parse(&source("A paragraph.\n")).unwrap();
        assert_eq!(
            small
                .assemble(&replace(&small, 0, &"a".repeat(MAX_SOURCE_BYTES)))
                .unwrap_err(),
            "FILE_TOO_LARGE"
        );
    }
}
