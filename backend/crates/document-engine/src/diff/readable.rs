//! Bounded readable projection. Every byte belongs to a unit even when YAML,
//! definitions, HTML or unsupported syntax cannot be rendered as ordinary GFM.
//! This is derived content, not authorization or a review approval.
use crate::{
    editing::{self, EditorKind},
    markdown,
    render::{self, RenderEvent},
};
use pulldown_cmark::{Event, Options, Parser, Tag};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;

const MAX_EVENTS: usize = 32_000;
const MAX_EXPANDED_BYTES: usize = 8 * 1024 * 1024;
#[derive(Debug, PartialEq, Eq)]
pub enum ReadableError {
    Source(&'static str),
    EventLimit,
    ExpansionLimit,
    DepthLimit,
    InvalidRange,
}
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum UnitKind {
    Metadata,
    Heading,
    Paragraph,
    List,
    Quote,
    Code,
    Table,
    Protected,
    Trailing,
}
/// Complete source coverage plus inert render events. Do not serialize a whole
/// projection into an unbounded response; the serving layer must window units.
pub struct ReadableDocument {
    pub document_id: String,
    pub sha256: String,
    pub source_bytes: usize,
    pub units: Vec<ReadableUnit>,
}
pub struct ReadableUnit {
    pub kind: UnitKind,
    pub source_start_byte: usize,
    pub body_start_byte: usize,
    pub source_end_byte: usize,
    /// Unique heading identity, when this top-level unit is a heading.
    /// Duplicate marker IDs remain visible in section_ids but are never trusted.
    pub heading_id: Option<String>,
    pub section_ids: Vec<String>,
    pub protected_reason: Option<String>,
    pub raw_sha256: String,
    /// Excludes source coordinates/search ordinals; includes GFM formatting,
    /// destinations, titles, list/table/task metadata and exact visible text.
    pub semantic_sha256: Option<String>,
    pub code_info: Vec<String>,
    pub events: Vec<RenderEvent>,
}
pub fn prepare(source: &str) -> Result<ReadableDocument, ReadableError> {
    let (_, body_offset) = markdown::front_matter(source).map_err(ReadableError::Source)?;
    crate::save_validation::check_reference_expansion(source).map_err(ReadableError::Source)?;
    let code_info = preflight(&source[body_offset..])?;
    let document = markdown::parse(source).map_err(ReadableError::Source)?;
    let model = editing::parse(source).map_err(ReadableError::Source)?;
    let events = render::events(&document).map_err(ReadableError::Source)?;
    let mut units = vec![unit(
        source,
        UnitKind::Metadata,
        0,
        model.preamble.len(),
        model.preamble.len(),
        Some("FRONT_MATTER".into()),
    )?];
    let mut headings: Vec<_> = document
        .blocks
        .iter()
        .filter(|block| block.kind == "heading" && !block.section_id.is_empty())
        .collect();
    headings.sort_by_key(|heading| heading.source_start);
    let mut occurrences = HashMap::<&str, usize>::new();
    for id in &model.section_ids {
        *occurrences.entry(id).or_default() += 1;
    }
    let mut cursor = model.preamble.len();
    for block in &model.blocks {
        let start = block
            .source_start_byte
            .checked_sub(block.prefix.len())
            .ok_or(ReadableError::InvalidRange)?;
        if start != cursor {
            return Err(ReadableError::InvalidRange);
        }
        let kind = match block.kind {
            EditorKind::Heading => UnitKind::Heading,
            EditorKind::Paragraph => UnitKind::Paragraph,
            EditorKind::List => UnitKind::List,
            EditorKind::Quote => UnitKind::Quote,
            EditorKind::Code => UnitKind::Code,
            EditorKind::Table => UnitKind::Table,
            EditorKind::Protected => UnitKind::Protected,
        };
        let mut item = unit(
            source,
            kind,
            start,
            block.source_start_byte,
            block.source_end_byte,
            block.protected_reason.clone(),
        )?;
        let first =
            headings.partition_point(|heading| heading.source_start < block.source_start_byte);
        let last = headings.partition_point(|heading| heading.source_start < block.source_end_byte);
        item.section_ids = headings[first..last]
            .iter()
            .map(|heading| heading.section_id.clone())
            .collect();
        item.heading_id = block
            .section_id
            .clone()
            .filter(|id| occurrences.get(id.as_str()) == Some(&1));
        cursor = block.source_end_byte;
        units.push(item);
    }
    if cursor < source.len() {
        units.push(unit(
            source,
            UnitKind::Trailing,
            cursor,
            cursor,
            source.len(),
            Some("TRAILING_SOURCE".into()),
        )?);
    }
    for (relative, info) in code_info {
        let start = body_offset + relative;
        let index = units.partition_point(|unit| unit.source_end_byte <= start);
        let item = units.get_mut(index).ok_or(ReadableError::InvalidRange)?;
        item.code_info.push(info);
    }
    for event in events {
        let index = units.partition_point(|unit| unit.source_end_byte <= event.source_start_byte);
        let item = units.get_mut(index).ok_or(ReadableError::InvalidRange)?;
        if event.source_start_byte < item.body_start_byte
            || event.source_end_byte > item.source_end_byte
        {
            return Err(ReadableError::InvalidRange);
        }
        item.events.push(event);
    }
    for item in &mut units {
        if item.protected_reason.is_none() {
            // Source/search IDs and byte positions are deliberately omitted.
            let semantic: Vec<_> = item
                .events
                .iter()
                .map(|event| {
                    (
                        event.kind,
                        event.tag,
                        &event.text,
                        event.level,
                        event.list_start,
                        &event.destination,
                        &event.title,
                        &event.alignments,
                        event.checked,
                        event.hard_break,
                    )
                })
                .collect();
            item.semantic_sha256 = Some(hash(
                &serde_json::to_vec(&(&semantic, &item.code_info))
                    .map_err(|_| ReadableError::InvalidRange)?,
            ));
        }
    }
    Ok(ReadableDocument {
        document_id: model.document_id,
        sha256: model.sha256,
        source_bytes: source.len(),
        units,
    })
}
fn unit(
    source: &str,
    kind: UnitKind,
    start: usize,
    body: usize,
    end: usize,
    protected_reason: Option<String>,
) -> Result<ReadableUnit, ReadableError> {
    let raw = source
        .get(start..end)
        .filter(|_| start <= body && body <= end)
        .ok_or(ReadableError::InvalidRange)?;
    Ok(ReadableUnit {
        kind,
        source_start_byte: start,
        body_start_byte: body,
        source_end_byte: end,
        heading_id: None,
        section_ids: vec![],
        protected_reason,
        raw_sha256: hash(raw.as_bytes()),
        semantic_sha256: None,
        code_info: vec![],
        events: vec![],
    })
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn preflight(body: &str) -> Result<Vec<(usize, String)>, ReadableError> {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let (mut count, mut expanded, mut depth) = (0usize, 0usize, 0usize);
    let mut code_info = vec![];
    for (event, range) in Parser::new_ext(body, options).into_offset_iter() {
        count += 1;
        if count > MAX_EVENTS {
            return Err(ReadableError::EventLimit);
        }
        match &event {
            Event::Start(_) => {
                depth += 1;
                if depth > 64 {
                    return Err(ReadableError::DepthLimit);
                }
            }
            Event::End(_) => {
                depth = depth.saturating_sub(1);
            }
            _ => {}
        }
        if let Event::Start(Tag::CodeBlock(kind)) = &event {
            let info = match kind {
                pulldown_cmark::CodeBlockKind::Fenced(info) => info.to_string(),
                pulldown_cmark::CodeBlockKind::Indented => String::new(),
            };
            code_info.push((range.start, info));
        }
        let bytes = match event {
            Event::Text(text) | Event::Code(text) | Event::Html(text) | Event::InlineHtml(text) => {
                text.len()
            }
            Event::Start(
                Tag::Link {
                    dest_url, title, ..
                }
                | Tag::Image {
                    dest_url, title, ..
                },
            ) => dest_url.len().saturating_add(title.len()),
            Event::Start(Tag::CodeBlock(pulldown_cmark::CodeBlockKind::Fenced(info))) => info.len(),
            _ => 0,
        };
        expanded = expanded.saturating_add(bytes);
        if expanded > MAX_EXPANDED_BYTES {
            return Err(ReadableError::ExpansionLimit);
        }
    }
    Ok(code_info)
}

#[cfg(test)]
mod tests;
