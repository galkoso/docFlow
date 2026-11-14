use crate::find::VisibleBlock;
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;

#[derive(Debug, Deserialize, Serialize)]
pub struct FrontMatter {
    pub document_id: String,
    pub title: String,
    pub owner_team: String,
    pub schema_version: u32,
}

#[derive(Debug, Serialize)]
pub struct Finding {
    pub code: &'static str,
    pub source_start: usize,
    pub source_end: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct TextBlock {
    pub block_id: String,
    pub section_id: String,
    pub kind: &'static str,
    pub text: String,
    pub source_start: usize,
    pub source_end: usize,
}

/// Source is retained byte-for-byte. Parsing never serializes the original file.
#[derive(Debug)]
pub struct Document {
    pub metadata: FrontMatter,
    pub source: String,
    pub sha256: String,
    pub blocks: Vec<TextBlock>,
    pub findings: Vec<Finding>,
}

impl Document {
    pub fn visible_blocks(&self) -> Vec<VisibleBlock<'_>> {
        self.blocks
            .iter()
            .map(|block| VisibleBlock {
                document_id: &self.metadata.document_id,
                section_id: &block.section_id,
                block_id: &block.block_id,
                text: &block.text,
            })
            .collect()
    }
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 120
        && value
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'-')
}

fn marker(html: &str) -> Option<&str> {
    html.trim()
        .strip_prefix("<!-- ss:section:")?
        .strip_suffix(" -->")
        .filter(|id| identifier(id))
}

fn flush(current: &mut Option<TextBlock>, blocks: &mut Vec<TextBlock>, source_end: usize) {
    if let Some(mut block) = current.take()
        && !block.text.is_empty()
    {
        block.source_end = source_end;
        block.block_id = format!("block-{}", blocks.len());
        blocks.push(block);
    }
}

fn allowed_destination(destination: &str, image: bool) -> bool {
    // URLs are never fetched by this engine. Images are repository-relative only.
    if image {
        return destination.starts_with("assets/")
            && !destination.contains("..")
            && !destination.contains('\\');
    }
    let lower = destination.to_ascii_lowercase();
    lower.starts_with("https://")
        || destination.starts_with('#')
        || destination.starts_with("ss:")
        || (destination.starts_with("docs/") && !destination.contains(".."))
}

pub fn parse(source: &str) -> Result<Document, &'static str> {
    if source.len() > 2 * 1024 * 1024 {
        return Err("FILE_TOO_LARGE");
    }
    let after_open = source
        .strip_prefix("---\n")
        .ok_or("FRONT_MATTER_REQUIRED")?;
    let closing = after_open.find("\n---\n").ok_or("INVALID_FRONT_MATTER")?;
    let metadata: FrontMatter =
        serde_yaml_ng::from_str(&after_open[..closing]).map_err(|_| "INVALID_FRONT_MATTER")?;
    if !identifier(&metadata.document_id)
        || metadata.schema_version != 1
        || metadata.title.trim().is_empty()
    {
        return Err("INVALID_FRONT_MATTER");
    }
    let offset = 4 + closing + 5;
    let body = &source[offset..];
    let mut document = Document {
        metadata,
        source: source.into(),
        sha256: format!("{:x}", Sha256::digest(source)),
        blocks: vec![],
        findings: vec![],
    };
    let mut current: Option<TextBlock> = None;
    let mut section_id = String::new();
    let mut pending_section: Option<String> = None;
    let mut seen_sections = HashSet::new();
    let mut image_depth: usize = 0;
    let mut image_has_alt = false;
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    for (event, range) in Parser::new_ext(body, options).into_offset_iter() {
        let start = offset + range.start;
        let end = offset + range.end;
        match event {
            Event::Html(html) | Event::InlineHtml(html) => {
                if let Some(id) = marker(&html) {
                    pending_section = Some(id.into());
                } else {
                    document.findings.push(Finding {
                        code: "UNSUPPORTED_HTML",
                        source_start: start,
                        source_end: end,
                    });
                }
            }
            Event::Start(Tag::Image { dest_url, .. }) => {
                image_depth += 1;
                image_has_alt = false;
                if !allowed_destination(&dest_url, true) {
                    document.findings.push(Finding {
                        code: "EXTERNAL_OR_UNSAFE_IMAGE",
                        source_start: start,
                        source_end: end,
                    });
                }
            }
            Event::End(TagEnd::Image) => {
                image_depth = image_depth.saturating_sub(1);
                if !image_has_alt {
                    document.findings.push(Finding {
                        code: "IMAGE_ALT_REQUIRED",
                        source_start: start,
                        source_end: end,
                    });
                }
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                if !allowed_destination(&dest_url, false) {
                    document.findings.push(Finding {
                        code: "UNSAFE_LINK",
                        source_start: start,
                        source_end: end,
                    });
                }
            }
            Event::Start(tag) => {
                let kind = match tag {
                    Tag::Heading { level, .. } => {
                        if !matches!(
                            level,
                            HeadingLevel::H1
                                | HeadingLevel::H2
                                | HeadingLevel::H3
                                | HeadingLevel::H4
                        ) {
                            document.findings.push(Finding {
                                code: "UNSUPPORTED_HEADING_LEVEL",
                                source_start: start,
                                source_end: end,
                            });
                        }
                        if let Some(id) = pending_section.take() {
                            if !seen_sections.insert(id.clone()) {
                                document.findings.push(Finding {
                                    code: "DUPLICATE_SECTION_ID",
                                    source_start: start,
                                    source_end: end,
                                });
                            }
                            section_id = id;
                        } else {
                            document.findings.push(Finding {
                                code: "SECTION_ID_REQUIRED",
                                source_start: start,
                                source_end: end,
                            });
                            section_id.clear();
                        }
                        Some("heading")
                    }
                    Tag::Paragraph => Some("paragraph"),
                    Tag::TableCell => Some("table_cell"),
                    Tag::CodeBlock(_) => Some("code"),
                    Tag::Item => Some("list_item"),
                    _ => None,
                };
                if let Some(kind) = kind {
                    flush(&mut current, &mut document.blocks, start);
                    current = Some(TextBlock {
                        block_id: String::new(),
                        section_id: section_id.clone(),
                        kind,
                        text: String::new(),
                        source_start: start,
                        source_end: end,
                    });
                }
            }
            Event::End(
                TagEnd::Heading(_)
                | TagEnd::Paragraph
                | TagEnd::TableCell
                | TagEnd::CodeBlock
                | TagEnd::Item,
            ) => {
                flush(&mut current, &mut document.blocks, end);
            }
            Event::Text(text) | Event::Code(text) => {
                if image_depth > 0 {
                    image_has_alt |= !text.trim().is_empty();
                } else if let Some(block) = &mut current {
                    block.text.push_str(&text);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some(block) = &mut current {
                    block.text.push('\n');
                }
            }
            _ => {}
        }
    }
    flush(&mut current, &mut document.blocks, source.len());
    Ok(document)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::find::find;

    const PREFIX: &str =
        "---\ndocument_id: DOC-TEST\ntitle: בדיקה\nowner_team: media\nschema_version: 1\n---\n\n";

    #[test]
    fn visible_search_excludes_metadata_comments_and_link_destinations() {
        let source = format!(
            "{PREFIX}<!-- ss:section:SEC-ONE -->\n## כותרת\n\n[מילה](https://example.com/hidden) **מחיקה**\n\n<!-- secret -->\n\n```rust\nlet visible_code = 1;\n```\n"
        );
        let document = parse(&source).unwrap();
        assert_eq!(document.source, source);
        assert_eq!(
            find("sha", &document.visible_blocks(), "מחיקה", 0, 100)
                .unwrap()
                .total_matches,
            1
        );
        assert_eq!(
            find("sha", &document.visible_blocks(), "visible_code", 0, 100)
                .unwrap()
                .total_matches,
            1
        );
        for hidden in ["DOC-TEST", "owner_team", "hidden", "secret", "ss:section"] {
            assert_eq!(
                find("sha", &document.visible_blocks(), hidden, 0, 100)
                    .unwrap()
                    .total_matches,
                0,
                "indexed hidden {hidden}"
            );
        }
        assert!(
            document
                .blocks
                .iter()
                .all(|block| block.section_id == "SEC-ONE")
        );
    }

    #[test]
    fn tables_lists_and_paragraphs_remain_separate_search_boundaries() {
        let source = format!(
            "{PREFIX}<!-- ss:section:SEC-TABLE -->\n## Table\n\n| First | Second |\n|---|---|\n| alpha | beta |\n\n- gamma\n- delta\n\nparagraph one\n\nparagraph two\n"
        );
        let document = parse(&source).unwrap();
        let blocks = document.visible_blocks();
        assert_eq!(
            find("sha", &blocks, "alpha", 0, 100).unwrap().total_matches,
            1
        );
        assert_eq!(
            find("sha", &blocks, "gamma", 0, 100).unwrap().total_matches,
            1
        );
        for crossing in ["alpha beta", "gammadelta", "oneparagraph"] {
            assert_eq!(
                find("sha", &blocks, crossing, 0, 100)
                    .unwrap()
                    .total_matches,
                0
            );
        }
    }

    #[test]
    fn heading_rename_keeps_id_and_invalid_content_has_located_findings() {
        let original = format!("{PREFIX}<!-- ss:section:SEC-ONE -->\n## Old title\n");
        assert_eq!(
            parse(&original).unwrap().blocks[0].section_id,
            parse(&original.replace("Old title", "שם חדש"))
                .unwrap()
                .blocks[0]
                .section_id
        );
        let invalid = format!(
            "{original}\n<!-- ss:section:SEC-ONE -->\n## Duplicate\n\n<script>alert(1)</script>\n\n![](https://example.com/a.png)\n\n[bad](javascript:alert%281%29)\n"
        );
        let document = parse(&invalid).unwrap();
        for code in [
            "DUPLICATE_SECTION_ID",
            "UNSUPPORTED_HTML",
            "EXTERNAL_OR_UNSAFE_IMAGE",
            "IMAGE_ALT_REQUIRED",
            "UNSAFE_LINK",
        ] {
            assert!(
                document.findings.iter().any(
                    |finding| finding.code == code && finding.source_end > finding.source_start
                ),
                "missing {code}"
            );
        }
        assert_eq!(document.source, invalid);
    }
}
