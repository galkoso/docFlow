//! Canonical, inert render events. No user HTML or executable markup is emitted.
use crate::markdown::{Document, allowed_destination, marker};
use pulldown_cmark::{Alignment, Event, Options, Parser, Tag, TagEnd};
use serde::Serialize;
use std::{collections::HashMap, ops::Range};

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum RenderKind {
    Open,
    Close,
    Text,
    Break,
    Rule,
    Image,
    Unsupported,
    TaskMarker,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum RenderTag {
    Paragraph,
    Heading,
    Quote,
    List,
    Item,
    Table,
    TableHead,
    TableRow,
    TableCell,
    Emphasis,
    Strong,
    Strike,
    CodeBlock,
    InlineCode,
    Link,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RenderEvent {
    pub kind: RenderKind,
    pub tag: Option<RenderTag>,
    pub text: Option<String>,
    pub block_id: Option<String>,
    pub offset_utf16: Option<u32>,
    pub section_id: Option<String>,
    pub level: Option<u8>,
    pub list_start: Option<u64>,
    pub destination: Option<String>,
    pub title: Option<String>,
    pub alignments: Vec<String>,
    pub checked: Option<bool>,
    pub hard_break: Option<bool>,
    pub source_start_byte: usize,
    pub source_end_byte: usize,
}

impl RenderEvent {
    fn new(kind: RenderKind, range: &Range<usize>) -> Self {
        Self {
            kind,
            tag: None,
            text: None,
            block_id: None,
            offset_utf16: None,
            section_id: None,
            level: None,
            list_start: None,
            destination: None,
            title: None,
            alignments: vec![],
            checked: None,
            hard_break: None,
            source_start_byte: range.start,
            source_end_byte: range.end,
        }
    }
}

fn close_tag(tag: TagEnd) -> Option<RenderTag> {
    Some(match tag {
        TagEnd::Paragraph => RenderTag::Paragraph,
        TagEnd::Heading(_) => RenderTag::Heading,
        TagEnd::BlockQuote(_) => RenderTag::Quote,
        TagEnd::List(_) => RenderTag::List,
        TagEnd::Item => RenderTag::Item,
        TagEnd::Table => RenderTag::Table,
        TagEnd::TableHead => RenderTag::TableHead,
        TagEnd::TableRow => RenderTag::TableRow,
        TagEnd::TableCell => RenderTag::TableCell,
        TagEnd::Emphasis => RenderTag::Emphasis,
        TagEnd::Strong => RenderTag::Strong,
        TagEnd::Strikethrough => RenderTag::Strike,
        TagEnd::CodeBlock => RenderTag::CodeBlock,
        TagEnd::Link => RenderTag::Link,
        _ => return None,
    })
}

/// The render/search pipelines use the same parser and visible block identities.
/// Consumers render only the known tags, interpolate text, and never trust HTML.
pub fn events(document: &Document) -> Result<Vec<RenderEvent>, &'static str> {
    let after_open = document
        .source
        .strip_prefix("---\n")
        .ok_or("INVALID_FRONT_MATTER")?;
    let offset = 4 + after_open.find("\n---\n").ok_or("INVALID_FRONT_MATTER")? + 5;
    let blocks: HashMap<_, _> = document
        .blocks
        .iter()
        .map(|block| (block.source_start, block))
        .collect();
    let mut output = vec![];
    let mut active = None;
    let mut text_offset = 0u32;
    let mut image: Option<(RenderEvent, usize)> = None;
    let mut depth = 0usize;
    for (event, range) in Parser::new_ext(
        &document.source[offset..],
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS,
    )
    .into_offset_iter()
    {
        let range = (range.start + offset)..(range.end + offset);
        if let Some((image_event, nesting)) = &mut image {
            match event {
                Event::Start(Tag::Image { .. }) => *nesting += 1,
                Event::End(TagEnd::Image) => *nesting -= 1,
                Event::Text(text) | Event::Code(text) => image_event
                    .text
                    .get_or_insert_with(String::new)
                    .push_str(&text),
                Event::SoftBreak | Event::HardBreak => {
                    image_event.text.get_or_insert_with(String::new).push(' ')
                }
                _ => {}
            }
            if *nesting == 0 {
                output.push(image.take().ok_or("RENDER_INVALID")?.0);
            }
            continue;
        }
        let mut item = RenderEvent::new(RenderKind::Open, &range);
        let inline_code = matches!(&event, Event::Code(_));
        let hard_break = matches!(&event, Event::HardBreak);
        if matches!(
            &event,
            Event::Start(Tag::HtmlBlock) | Event::End(TagEnd::HtmlBlock)
        ) {
            continue;
        }
        match event {
            Event::Start(Tag::Image {
                dest_url, title, ..
            }) => {
                item.kind = RenderKind::Image;
                item.destination =
                    allowed_destination(&dest_url, true).then(|| dest_url.into_string());
                item.title = Some(title.into_string());
                image = Some((item, 1));
                continue;
            }
            Event::Start(tag) => {
                depth += 1;
                if depth > 64 {
                    return Err("RENDER_DEPTH_EXCEEDED");
                }
                if matches!(
                    tag,
                    Tag::Paragraph
                        | Tag::Heading { .. }
                        | Tag::CodeBlock(_)
                        | Tag::TableCell
                        | Tag::Item
                ) {
                    active = blocks.get(&range.start).copied();
                    text_offset = 0;
                }
                item.tag = Some(match tag {
                    Tag::Paragraph => RenderTag::Paragraph,
                    Tag::Heading { level, .. } => {
                        item.level = Some(level as u8);
                        item.section_id = active.map(|block| block.section_id.clone());
                        RenderTag::Heading
                    }
                    Tag::BlockQuote(_) => RenderTag::Quote,
                    Tag::List(start) => {
                        item.list_start = start;
                        RenderTag::List
                    }
                    Tag::Item => RenderTag::Item,
                    Tag::Table(alignments) => {
                        item.alignments = alignments
                            .iter()
                            .map(|alignment| {
                                match alignment {
                                    Alignment::None => "none",
                                    Alignment::Left => "left",
                                    Alignment::Center => "center",
                                    Alignment::Right => "right",
                                }
                                .into()
                            })
                            .collect();
                        RenderTag::Table
                    }
                    Tag::TableHead => RenderTag::TableHead,
                    Tag::TableRow => RenderTag::TableRow,
                    Tag::TableCell => RenderTag::TableCell,
                    Tag::Emphasis => RenderTag::Emphasis,
                    Tag::Strong => RenderTag::Strong,
                    Tag::Strikethrough => RenderTag::Strike,
                    Tag::CodeBlock(_) => RenderTag::CodeBlock,
                    Tag::Link {
                        dest_url, title, ..
                    } => {
                        item.destination =
                            allowed_destination(&dest_url, false).then(|| dest_url.into_string());
                        item.title = Some(title.into_string());
                        RenderTag::Link
                    }
                    _ => return Err("RENDER_UNSUPPORTED"),
                });
                output.push(item);
            }
            Event::End(tag) => {
                depth = depth.saturating_sub(1);
                if matches!(
                    tag,
                    TagEnd::Paragraph
                        | TagEnd::Heading(_)
                        | TagEnd::CodeBlock
                        | TagEnd::TableCell
                        | TagEnd::Item
                ) {
                    active = None;
                    text_offset = 0;
                }
                item.kind = RenderKind::Close;
                item.tag = close_tag(tag);
                if item.tag.is_none() {
                    return Err("RENDER_UNSUPPORTED");
                }
                output.push(item);
            }
            Event::Text(text) | Event::Code(text) => {
                if inline_code {
                    let mut open = RenderEvent::new(RenderKind::Open, &range);
                    open.tag = Some(RenderTag::InlineCode);
                    output.push(open);
                }
                item.kind = RenderKind::Text;
                item.text = Some(text.into_string());
                item.block_id = active.map(|block| block.block_id.clone());
                item.offset_utf16 = active.map(|_| text_offset);
                text_offset += item
                    .text
                    .as_deref()
                    .unwrap_or_default()
                    .encode_utf16()
                    .count() as u32;
                output.push(item);
                if inline_code {
                    let mut close = RenderEvent::new(RenderKind::Close, &range);
                    close.tag = Some(RenderTag::InlineCode);
                    output.push(close);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                item.kind = RenderKind::Break;
                item.hard_break = Some(hard_break);
                item.text = Some("\n".into());
                item.block_id = active.map(|block| block.block_id.clone());
                item.offset_utf16 = active.map(|_| text_offset);
                text_offset += 1;
                output.push(item);
            }
            Event::Html(html) | Event::InlineHtml(html) => {
                if marker(&html).is_none() {
                    item.kind = RenderKind::Unsupported;
                    output.push(item);
                }
            }
            Event::Rule => {
                item.kind = RenderKind::Rule;
                output.push(item);
            }
            Event::TaskListMarker(checked) => {
                item.kind = RenderKind::TaskMarker;
                item.checked = Some(checked);
                output.push(item);
            }
            _ => {
                item.kind = RenderKind::Unsupported;
                output.push(item);
            }
        }
        if output.len() > 100000 {
            return Err("RENDER_TOO_LARGE");
        }
    }
    if depth != 0 || image.is_some() {
        return Err("RENDER_INVALID");
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_text_offsets_match_search_across_formatting_tables_code_and_nested_lists() {
        let source = "---\ndocument_id: DOC-TEST\ntitle: בדיקה\nowner_team: test\nschema_version: 1\n---\n\n<!-- ss:section:SEC-TEST -->\n## כותרת\n\n**שלום** *עולם* `code` 😀\nשורה שנייה\n\n- parent\n  - child\n\n  after\n\n| א | ב |\n|---|---|\n| מחיקה | עריכה |\n\n```ts\nconst x = 1;\n```\n";
        let document = crate::markdown::parse(source).unwrap();
        let rendered = events(&document).unwrap();
        let mut texts = HashMap::<String, String>::new();
        for event in &rendered {
            if matches!(event.kind, RenderKind::Text | RenderKind::Break) {
                let id = event
                    .block_id
                    .as_ref()
                    .expect("visible text must be indexed");
                let current = texts.entry(id.clone()).or_default();
                assert_eq!(
                    event.offset_utf16.unwrap() as usize,
                    current.encode_utf16().count()
                );
                current.push_str(event.text.as_deref().unwrap());
            }
        }
        for block in &document.blocks {
            assert_eq!(texts.get(&block.block_id), Some(&block.text));
        }
        assert!(
            rendered
                .iter()
                .any(|event| event.tag == Some(RenderTag::Strong))
        );
        assert!(
            rendered
                .iter()
                .any(|event| event.tag == Some(RenderTag::Table))
        );
    }

    #[test]
    fn scripts_and_remote_images_never_become_executable_render_markup() {
        let document = crate::markdown::parse("---\ndocument_id: DOC-TEST\ntitle: Test\nowner_team: test\nschema_version: 1\n---\n\n<!-- ss:section:SEC-TEST -->\n## Test\n\n<script>alert(1)</script>\n\n[unsafe](javascript:alert%281%29) ![alt](https://remote.example/image.png)\n").unwrap();
        let rendered = events(&document).unwrap();
        assert!(
            rendered
                .iter()
                .any(|event| event.kind == RenderKind::Unsupported)
        );
        assert!(
            rendered
                .iter()
                .filter(
                    |event| event.tag == Some(RenderTag::Link) || event.kind == RenderKind::Image
                )
                .all(|event| event.destination.is_none())
        );
        assert!(
            !serde_json::to_string(&rendered)
                .unwrap()
                .contains("<script>")
        );
    }

    #[test]
    fn deeply_nested_markup_has_a_bounded_failure_and_image_paths_are_canonical() {
        let prefix = "---\ndocument_id: DOC-TEST\ntitle: Test\nowner_team: test\nschema_version: 1\n---\n\n<!-- ss:section:SEC-TEST -->\n## Test\n\n";
        let document =
            crate::markdown::parse(&format!("{prefix}{}text\n", "> ".repeat(70))).unwrap();
        assert!(matches!(events(&document), Err("RENDER_DEPTH_EXCEEDED")));
        let hash = "a".repeat(64);
        let document = crate::markdown::parse(&format!(
            "{prefix}![safe](assets/{hash}.png) ![blocked](assets/{hash}.svg)\n"
        ))
        .unwrap();
        let rendered = events(&document).unwrap();
        let images: Vec<_> = rendered
            .iter()
            .filter(|event| event.kind == RenderKind::Image)
            .collect();
        assert_eq!(
            images[0].destination.as_deref(),
            Some(format!("assets/{hash}.png").as_str())
        );
        assert!(images[1].destination.is_none());
    }
}
