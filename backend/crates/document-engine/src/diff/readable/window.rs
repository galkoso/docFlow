//! Bounded readable DTOs. Window coordinates and container state refer to one
//! immutable unit; a serving layer must bind file/snapshot and current access.
use super::{
    ReadableDocument, ReadableError, ReadableUnit, UnitKind,
    pair::{ReadablePair, ReadableSide, UnitChange},
};
use crate::render::{RenderEvent, RenderKind, RenderTag};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io::{self, Write},
};

const MAX_ROWS: usize = 20_004;
const MAX_FRAGMENTS: usize = 128;
const STACK_BYTES: usize = 32 * 1024;
const FRAGMENT_BYTES: usize = 256 * 1024;
pub const MAX_READABLE_PAGE_JSON_BYTES: usize = 512 * 1024;
pub const MAX_EVENT_WINDOW_JSON_BYTES: usize = 384 * 1024;
#[derive(Debug, PartialEq, Eq)]
pub enum ReadableWindowError {
    InvalidCursor,
    InvalidLimit,
    Unavailable,
    ProtectedSource,
    MetadataTooLarge,
    InvalidEvidence,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct EventCursor {
    pub event: usize,
    /// UTF-8 offset in decoded text, not an offset in the original Markdown.
    pub offset: usize,
}
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UnitSummary {
    pub index: usize,
    pub kind: UnitKind,
    pub source_start_byte: usize,
    pub body_start_byte: usize,
    pub source_end_byte: usize,
    pub raw_sha256: String,
    pub semantic_sha256: Option<String>,
    pub heading_id: Option<String>,
    pub protected_reason: Option<String>,
    pub section_ids_total: usize,
    pub code_blocks_total: usize,
    pub events_total: usize,
}
impl UnitSummary {
    fn new(index: usize, unit: &ReadableUnit) -> Self {
        Self {
            index,
            kind: unit.kind,
            source_start_byte: unit.source_start_byte,
            body_start_byte: unit.body_start_byte,
            source_end_byte: unit.source_end_byte,
            raw_sha256: unit.raw_sha256.clone(),
            semantic_sha256: unit.semantic_sha256.clone(),
            heading_id: unit.heading_id.clone(),
            protected_reason: unit.protected_reason.clone(),
            section_ids_total: unit.section_ids.len(),
            code_blocks_total: unit.code_info.len(),
            events_total: unit.events.len(),
        }
    }
}
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReadableRow {
    pub change: UnitChange,
    pub before: Option<UnitSummary>,
    pub after: Option<UnitSummary>,
}
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReadableSideSummary {
    pub state: String,
    pub sha256: Option<String>,
    pub document_id: Option<String>,
    pub source_bytes: usize,
    pub units_total: usize,
    pub error_code: Option<String>,
}
impl ReadableSideSummary {
    fn new(side: &ReadableSide) -> Self {
        let (state, bytes, error) = match side {
            ReadableSide::Absent => ("ABSENT", 0, None),
            ReadableSide::Ready(document) => ("READY", document.source_bytes, None),
            ReadableSide::Unavailable {
                source_bytes,
                error,
                ..
            } => (
                "RAW_REQUIRED",
                *source_bytes,
                Some(error_code(error).into()),
            ),
        };
        Self {
            state: state.into(),
            sha256: side.sha256().map(str::to_owned),
            document_id: side.document().map(|d| d.document_id.clone()),
            source_bytes: bytes,
            units_total: side.document().map_or(0, |d| d.units.len()),
            error_code: error,
        }
    }
}
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReadablePage {
    pub ready: bool,
    pub raw_changed: bool,
    pub before: ReadableSideSummary,
    pub after: ReadableSideSummary,
    pub offset: usize,
    pub total: usize,
    pub next_offset: Option<usize>,
    pub rows: Vec<ReadableRow>,
}
impl ReadablePair {
    pub fn page(&self, offset: usize, limit: usize) -> Result<ReadablePage, ReadableWindowError> {
        if offset > MAX_ROWS {
            return Err(ReadableWindowError::InvalidCursor);
        }
        if !(1..=100).contains(&limit) {
            return Err(ReadableWindowError::InvalidLimit);
        }
        let mut rows = vec![];
        for change in self.changes.iter().skip(offset).take(limit) {
            let summary =
                |side: &ReadableSide, index: Option<usize>| -> Result<_, ReadableWindowError> {
                    index
                        .map(|index| {
                            side.document()
                                .and_then(|doc| doc.units.get(index))
                                .map(|unit| UnitSummary::new(index, unit))
                                .ok_or(ReadableWindowError::InvalidEvidence)
                        })
                        .transpose()
                };
            let row = ReadableRow {
                change: change.clone(),
                before: summary(&self.before, change.before_index)?,
                after: summary(&self.after, change.after_index)?,
            };
            if encoded_len(&row, 4096).is_none() {
                return Err(ReadableWindowError::MetadataTooLarge);
            }
            rows.push(row);
        }
        let next = offset + rows.len();
        let result = ReadablePage {
            ready: self.ready,
            raw_changed: self.raw_changed,
            before: ReadableSideSummary::new(&self.before),
            after: ReadableSideSummary::new(&self.after),
            offset,
            total: self.changes.len(),
            next_offset: (next < self.changes.len()).then_some(next),
            rows,
        };
        if encoded_len(&result, MAX_READABLE_PAGE_JSON_BYTES).is_none() {
            return Err(ReadableWindowError::MetadataTooLarge);
        }
        Ok(result)
    }
}
impl ReadableSide {
    pub fn event_window(
        &self,
        unit: usize,
        cursor: EventCursor,
        limit: usize,
    ) -> Result<EventWindow, ReadableWindowError> {
        self.document()
            .ok_or(ReadableWindowError::Unavailable)?
            .event_window(unit, cursor, limit)
    }
}
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct EventFragment {
    pub event_index: usize,
    /// Decoded-event text coordinates. Original source coordinates remain on
    /// event and can cover entities/escapes wider than this decoded fragment.
    pub text_start_byte: usize,
    pub text_end_byte: usize,
    pub event: RenderEvent,
}
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct EventCodeInfo {
    pub event_index: usize,
    pub info: String,
}
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct OrderedListResume {
    pub stack_depth: usize,
    /// Starting item number when reopening this list at the window boundary.
    pub item_start: u64,
}
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct EventWindow {
    pub document_sha256: String,
    pub unit: UnitSummary,
    pub cursor: EventCursor,
    pub next: Option<EventCursor>,
    /// Original known Open events, outermost first. Consumers reopen these for
    /// a standalone page and close end_stack in reverse without injecting HTML.
    pub start_stack: Vec<RenderEvent>,
    pub end_stack: Vec<RenderEvent>,
    pub start_ordered_lists: Vec<OrderedListResume>,
    pub end_ordered_lists: Vec<OrderedListResume>,
    pub returned_text_bytes: usize,
    pub code_info: Vec<EventCodeInfo>,
    pub fragments: Vec<EventFragment>,
}
impl ReadableDocument {
    pub fn event_window(
        &self,
        unit_index: usize,
        cursor: EventCursor,
        limit: usize,
    ) -> Result<EventWindow, ReadableWindowError> {
        if !(1024..=65536).contains(&limit) {
            return Err(ReadableWindowError::InvalidLimit);
        }
        let unit = self
            .units
            .get(unit_index)
            .ok_or(ReadableWindowError::InvalidCursor)?;
        if unit.protected_reason.is_some() {
            return Err(ReadableWindowError::ProtectedSource);
        }
        if cursor.event > unit.events.len()
            || cursor.event == unit.events.len() && cursor.offset != 0
        {
            return Err(ReadableWindowError::InvalidCursor);
        }
        let mut stack = vec![];
        let mut list_counts = HashMap::new();
        for event in unit.events.iter().take(cursor.event) {
            advance(&mut stack, &mut list_counts, event)?;
        }
        let start_ordered_lists = list_resume(&stack, &list_counts)?;
        if encoded_len(&stack, STACK_BYTES).is_none() {
            return Err(ReadableWindowError::MetadataTooLarge);
        }
        let initial_stack = stack.clone();
        let start_stack = stack
            .iter()
            .map(|event| copy_event(event, event.text.as_deref()))
            .collect();
        let mut fragments = vec![];
        let mut position = cursor;
        let (mut text_bytes, mut json_bytes) = (0, 0);
        while let Some(event) = unit.events.get(position.event) {
            if fragments.len() == MAX_FRAGMENTS {
                break;
            }
            let text = event.text.as_deref().unwrap_or("");
            let split = event.kind == RenderKind::Text;
            if position.offset > text.len()
                || !text.is_char_boundary(position.offset)
                || position.offset > 0 && (!split || position.offset == text.len())
            {
                return Err(ReadableWindowError::InvalidCursor);
            }
            if limit - text_bytes < 4 && (!text.is_empty() || !fragments.is_empty()) {
                break;
            }
            let mut end = if split {
                position.offset + (limit - text_bytes).min(text.len() - position.offset)
            } else {
                text.len()
            };
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            if !split && end > limit - text_bytes {
                if fragments.is_empty() {
                    return Err(ReadableWindowError::MetadataTooLarge);
                } else {
                    break;
                }
            }
            let candidate = loop {
                let value = if event.text.is_some() {
                    Some(&text[position.offset..end])
                } else {
                    None
                };
                let mut projected = copy_event(event, value);
                if split {
                    projected.offset_utf16 = event.offset_utf16.map(|offset| {
                        offset + text[..position.offset].encode_utf16().count() as u32
                    });
                }
                let candidate = EventFragment {
                    event_index: position.event,
                    text_start_byte: position.offset,
                    text_end_byte: end,
                    event: projected,
                };
                if let Some(bytes) = encoded_len(&candidate, FRAGMENT_BYTES - json_bytes) {
                    break Some((candidate, bytes));
                }
                if !split || end <= position.offset + 1 {
                    break None;
                }
                end = position.offset + (end - position.offset) / 2;
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                if end == position.offset {
                    break None;
                }
            };
            let Some((candidate, bytes)) = candidate else {
                if fragments.is_empty() {
                    return Err(ReadableWindowError::MetadataTooLarge);
                } else {
                    break;
                }
            };
            let mut next_stack = stack.clone();
            let mut next_counts = list_counts.clone();
            advance(&mut next_stack, &mut next_counts, event)?;
            if encoded_len(&next_stack, STACK_BYTES).is_none() {
                if fragments.is_empty() {
                    return Err(ReadableWindowError::MetadataTooLarge);
                } else {
                    break;
                }
            }
            stack = next_stack;
            list_counts = next_counts;
            text_bytes += end - position.offset;
            json_bytes += bytes;
            fragments.push(candidate);
            position = if end == text.len() {
                EventCursor {
                    event: position.event + 1,
                    offset: 0,
                }
            } else {
                EventCursor {
                    event: position.event,
                    offset: end,
                }
            };
        }
        if position.event == unit.events.len() && !stack.is_empty() {
            return Err(ReadableWindowError::InvalidEvidence);
        }
        let code_events: Vec<_> = unit
            .events
            .iter()
            .enumerate()
            .filter(|(_, event)| {
                event.kind == RenderKind::Open && event.tag == Some(RenderTag::CodeBlock)
            })
            .collect();
        if code_events.len() != unit.code_info.len() {
            return Err(ReadableWindowError::InvalidEvidence);
        }
        let mut code_info = vec![];
        let mut code_bytes = 0;
        for ((index, event), info) in code_events.into_iter().zip(&unit.code_info) {
            if index >= cursor.event && index < position.event
                || initial_stack.iter().any(|open| std::ptr::eq(*open, event))
            {
                let bytes = encoded_len(info, STACK_BYTES - code_bytes)
                    .ok_or(ReadableWindowError::MetadataTooLarge)?;
                code_bytes += bytes;
                code_info.push(EventCodeInfo {
                    event_index: index,
                    info: info.clone(),
                });
            }
        }
        let result = EventWindow {
            document_sha256: self.sha256.clone(),
            unit: UnitSummary::new(unit_index, unit),
            cursor,
            next: (position.event < unit.events.len()).then_some(position),
            start_stack,
            end_stack: stack
                .iter()
                .map(|event| copy_event(event, event.text.as_deref()))
                .collect(),
            start_ordered_lists,
            end_ordered_lists: list_resume(&stack, &list_counts)?,
            returned_text_bytes: text_bytes,
            code_info,
            fragments,
        };
        if encoded_len(&result, MAX_EVENT_WINDOW_JSON_BYTES).is_none() {
            return Err(ReadableWindowError::MetadataTooLarge);
        }
        Ok(result)
    }
}
fn event_key(event: &RenderEvent) -> usize {
    std::ptr::from_ref(event) as usize
}
fn advance<'a>(
    stack: &mut Vec<&'a RenderEvent>,
    counts: &mut HashMap<usize, u64>,
    event: &'a RenderEvent,
) -> Result<(), ReadableWindowError> {
    match event.kind {
        RenderKind::Open if event.tag.is_some() && stack.len() < 64 => {
            if event.tag == Some(RenderTag::List) {
                counts.insert(event_key(event), 0);
            }
            if event.tag == Some(RenderTag::Item) {
                let list = stack
                    .iter()
                    .rev()
                    .find(|open| open.tag == Some(RenderTag::List))
                    .ok_or(ReadableWindowError::InvalidEvidence)?;
                let count = counts
                    .get_mut(&event_key(list))
                    .ok_or(ReadableWindowError::InvalidEvidence)?;
                *count = count
                    .checked_add(1)
                    .ok_or(ReadableWindowError::InvalidEvidence)?;
            }
            stack.push(event);
        }
        RenderKind::Open => return Err(ReadableWindowError::InvalidEvidence),
        RenderKind::Close => {
            let open = stack.pop().ok_or(ReadableWindowError::InvalidEvidence)?;
            if open.tag != event.tag {
                return Err(ReadableWindowError::InvalidEvidence);
            }
            if open.tag == Some(RenderTag::List) {
                counts.remove(&event_key(open));
            }
        }
        _ => {}
    }
    Ok(())
}
fn list_resume(
    stack: &[&RenderEvent],
    counts: &HashMap<usize, u64>,
) -> Result<Vec<OrderedListResume>, ReadableWindowError> {
    let mut result = vec![];
    for (depth, open) in stack
        .iter()
        .enumerate()
        .filter(|(_, open)| open.tag == Some(RenderTag::List))
    {
        let Some(start) = open.list_start else {
            continue;
        };
        let active_item = stack
            .iter()
            .skip(depth + 1)
            .take_while(|child| child.tag != Some(RenderTag::List))
            .any(|child| child.tag == Some(RenderTag::Item));
        let count = counts
            .get(&event_key(open))
            .copied()
            .ok_or(ReadableWindowError::InvalidEvidence)?;
        let offset = count
            .checked_sub(u64::from(active_item))
            .ok_or(ReadableWindowError::InvalidEvidence)?;
        result.push(OrderedListResume {
            stack_depth: depth,
            item_start: start
                .checked_add(offset)
                .ok_or(ReadableWindowError::InvalidEvidence)?,
        });
    }
    Ok(result)
}
fn copy_event(event: &RenderEvent, text: Option<&str>) -> RenderEvent {
    RenderEvent {
        kind: event.kind,
        tag: event.tag,
        text: text.map(str::to_owned),
        block_id: event.block_id.clone(),
        offset_utf16: event.offset_utf16,
        section_id: event.section_id.clone(),
        level: event.level,
        list_start: event.list_start,
        destination: event.destination.clone(),
        title: event.title.clone(),
        alignments: event.alignments.clone(),
        checked: event.checked,
        hard_break: event.hard_break,
        source_start_byte: event.source_start_byte,
        source_end_byte: event.source_end_byte,
    }
}
fn error_code(error: &ReadableError) -> &'static str {
    match error {
        ReadableError::Source(code) => code,
        ReadableError::EventLimit => "READABLE_EVENT_LIMIT",
        ReadableError::ExpansionLimit => "READABLE_EXPANSION_LIMIT",
        ReadableError::DepthLimit => "READABLE_DEPTH_LIMIT",
        ReadableError::InvalidRange => "READABLE_RANGE_INVALID",
    }
}
fn encoded_len(value: &impl Serialize, limit: usize) -> Option<usize> {
    struct Counter {
        bytes: usize,
        limit: usize,
    }
    impl Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let next = self
                .bytes
                .checked_add(bytes.len())
                .filter(|next| *next <= self.limit)
                .ok_or_else(|| io::Error::other("bounded JSON size exceeded"))?;
            self.bytes = next;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter { bytes: 0, limit };
    serde_json::to_writer(&mut counter, value).ok()?;
    Some(counter.bytes)
}

#[cfg(test)]
mod tests;
