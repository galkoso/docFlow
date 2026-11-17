//! Conservative readable pairing. Output covers every unit on each side,
//! including unchanged/protected source. Matching evidence is never approval.
use super::{ReadableDocument, ReadableError, ReadableUnit, UnitKind, hash, prepare};
use serde::Serialize;
use std::collections::HashMap;

pub enum ReadableSide {
    Absent,
    Ready(ReadableDocument),
    Unavailable {
        sha256: String,
        source_bytes: usize,
        error: ReadableError,
    },
}
impl ReadableSide {
    fn from_source(source: Option<&str>) -> Self {
        match source {
            None => Self::Absent,
            Some(source) => match prepare(source) {
                Ok(document) => Self::Ready(document),
                Err(error) => Self::Unavailable {
                    sha256: hash(source.as_bytes()),
                    source_bytes: source.len(),
                    error,
                },
            },
        }
    }
    pub fn document(&self) -> Option<&ReadableDocument> {
        if let Self::Ready(document) = self {
            Some(document)
        } else {
            None
        }
    }
    pub fn sha256(&self) -> Option<&str> {
        match self {
            Self::Absent => None,
            Self::Ready(document) => Some(&document.sha256),
            Self::Unavailable { sha256, .. } => Some(sha256),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum UnitMatch {
    Structural,
    HeadingId,
    ExactSource,
    RenderedContent,
    SectionPosition,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum UnitStatus {
    Unchanged,
    Modified,
    Added,
    Deleted,
}
#[derive(Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UnitChange {
    /// Versioned derived identity binds both complete source hashes and units.
    /// A serving/review layer must additionally bind file/snapshot and scope.
    pub id: String,
    pub before_index: Option<usize>,
    pub after_index: Option<usize>,
    pub status: UnitStatus,
    pub matched_by: Option<UnitMatch>,
    pub raw_changed: bool,
    /// None means render equivalence is unknown (e.g. protected source).
    pub render_changed: Option<bool>,
    /// Absolute ordinal difference, not a claim about which move was intended.
    pub position_changed: bool,
}
/// Intentionally not a whole-response DTO. Callers must window rows and content.
pub struct ReadablePair {
    pub before: ReadableSide,
    pub after: ReadableSide,
    /// False requires raw fallback; an unavailable projection is never no-change.
    pub ready: bool,
    pub raw_changed: bool,
    pub changes: Vec<UnitChange>,
}
impl ReadablePair {
    pub fn prepare(before: Option<&str>, after: Option<&str>) -> Self {
        let before = ReadableSide::from_source(before);
        let after = ReadableSide::from_source(after);
        let ready = !matches!(before, ReadableSide::Unavailable { .. })
            && !matches!(after, ReadableSide::Unavailable { .. });
        let raw_changed = before.sha256() != after.sha256();
        let changes = if ready {
            align(&before, &after)
        } else {
            vec![]
        };
        Self {
            before,
            after,
            ready,
            raw_changed,
            changes,
        }
    }
}
type Groups<'a> = HashMap<(UnitKind, &'a str), Vec<usize>>;
fn groups(units: &[ReadableUnit], render: bool) -> Groups<'_> {
    let mut result = Groups::new();
    for (index, unit) in units.iter().enumerate() {
        if matches!(unit.kind, UnitKind::Metadata | UnitKind::Trailing)
            || unit.kind == UnitKind::Heading && (render || unit.heading_id.is_some())
        {
            continue;
        }
        let value = if render {
            unit.semantic_sha256.as_deref()
        } else {
            Some(unit.raw_sha256.as_str())
        };
        if let Some(value) = value {
            result.entry((unit.kind, value)).or_default().push(index);
        }
    }
    result
}
fn align(before: &ReadableSide, after: &ReadableSide) -> Vec<UnitChange> {
    let old = before
        .document()
        .map_or(&[][..], |doc| doc.units.as_slice());
    let new = after.document().map_or(&[][..], |doc| doc.units.as_slice());
    // Identical complete sources need no inferred identity, even when they
    // contain repeated text or invalid/duplicate section markers.
    if before.sha256().is_some() && before.sha256() == after.sha256() {
        return old
            .iter()
            .enumerate()
            .map(|(index, unit)| {
                change(
                    before,
                    after,
                    Some((index, unit)),
                    Some((index, &new[index])),
                    Some(UnitMatch::ExactSource),
                )
            })
            .collect();
    }
    let mut matched = vec![None; old.len()];
    let mut used = vec![false; new.len()];
    // Metadata and trailing source each have at most one structural slot.
    for kind in [UnitKind::Metadata, UnitKind::Trailing] {
        if let (Some(a), Some(b)) = (
            old.iter().position(|u| u.kind == kind),
            new.iter().position(|u| u.kind == kind),
        ) {
            matched[a] = Some((b, UnitMatch::Structural));
            used[b] = true;
        }
    }
    let headings: HashMap<_, _> = new
        .iter()
        .enumerate()
        .filter_map(|(i, u)| u.heading_id.as_deref().map(|id| (id, i)))
        .collect();
    for (a, unit) in old.iter().enumerate() {
        if let Some(b) = unit
            .heading_id
            .as_deref()
            .and_then(|id| headings.get(id))
            .copied()
        {
            matched[a] = Some((b, UnitMatch::HeadingId));
            used[b] = true;
        }
    }
    let old_raw = groups(old, false);
    let new_raw = groups(new, false);
    let old_render = groups(old, true);
    let new_render = groups(new, true);
    for (left, right, method) in [
        (&old_raw, &new_raw, UnitMatch::ExactSource),
        (&old_render, &new_render, UnitMatch::RenderedContent),
    ] {
        for (key, left) in left {
            let Some(right) = right.get(key) else {
                continue;
            };
            if let ([a], [b]) = (left.as_slice(), right.as_slice())
                && matched[*a].is_none()
                && !used[*b]
            {
                matched[*a] = Some((*b, method));
                used[*b] = true;
            }
        }
    }
    let left = positional(old, &old_raw, &old_render, |i| matched[i].is_none());
    let right = positional(new, &new_raw, &new_render, |i| !used[i]);
    for (key, left) in left {
        let Some(right) = right.get(&key).filter(|right| right.len() == left.len()) else {
            continue;
        };
        for (a, b) in left.into_iter().zip(right.iter().copied()) {
            matched[a] = Some((b, UnitMatch::SectionPosition));
            used[b] = true;
        }
    }
    let mut rows = vec![];
    for (a, pairing) in matched.into_iter().enumerate() {
        let (b, method) = pairing.map_or((None, None), |(b, method)| (Some(b), Some(method)));
        rows.push(change(
            before,
            after,
            old.get(a).map(|u| (a, u)),
            b.map(|b| (b, &new[b])),
            method,
        ));
    }
    for (b, unit) in new.iter().enumerate().filter(|(b, _)| !used[*b]) {
        rows.push(change(before, after, None, Some((b, unit)), None));
    }
    rows.sort_by_key(|row| {
        (
            row.after_index.or(row.before_index),
            row.after_index.is_none(),
            row.before_index,
        )
    });
    rows
}
type Positions<'a> = HashMap<(Option<&'a str>, UnitKind), Vec<usize>>;
fn positional<'a>(
    units: &'a [ReadableUnit],
    raw: &Groups<'_>,
    rendered: &Groups<'_>,
    eligible: impl Fn(usize) -> bool,
) -> Positions<'a> {
    let mut result = Positions::new();
    // Some(None) is the pre-heading body. An unidentified heading disables
    // positional inference until another unique heading establishes a scope.
    let mut section = Some(None);
    for (index, unit) in units.iter().enumerate() {
        if unit.kind == UnitKind::Heading {
            section = unit.heading_id.as_deref().map(Some);
        }
        let Some(section) = section else {
            continue;
        };
        if !eligible(index)
            || matches!(
                unit.kind,
                UnitKind::Heading | UnitKind::Metadata | UnitKind::Trailing
            )
        {
            continue;
        }
        if raw
            .get(&(unit.kind, unit.raw_sha256.as_str()))
            .is_none_or(|group| group.len() != 1)
            || unit.semantic_sha256.as_deref().is_some_and(|sha| {
                rendered
                    .get(&(unit.kind, sha))
                    .is_none_or(|group| group.len() != 1)
            })
        {
            continue;
        }
        result.entry((section, unit.kind)).or_default().push(index);
    }
    result
}
fn change(
    before: &ReadableSide,
    after: &ReadableSide,
    old: Option<(usize, &ReadableUnit)>,
    new: Option<(usize, &ReadableUnit)>,
    method: Option<UnitMatch>,
) -> UnitChange {
    let a = old.map(|(_, unit)| unit);
    let b = new.map(|(_, unit)| unit);
    let raw_changed = a.map(|u| &u.raw_sha256) != b.map(|u| &u.raw_sha256);
    let status = match (a, b) {
        (None, Some(_)) => UnitStatus::Added,
        (Some(_), None) => UnitStatus::Deleted,
        _ if raw_changed => UnitStatus::Modified,
        _ => UnitStatus::Unchanged,
    };
    let render_changed = match (a, b) {
        (Some(a), Some(b)) => a
            .semantic_sha256
            .as_ref()
            .zip(b.semantic_sha256.as_ref())
            .map(|(a, b)| a != b),
        (Some(unit), None) | (None, Some(unit)) => unit.semantic_sha256.as_ref().map(|_| true),
        _ => None,
    };
    let before_index = old.map(|(index, _)| index);
    let after_index = new.map(|(index, _)| index);
    let identity = serde_json::to_vec(&(
        "specspace-readable-unit-v1",
        before.sha256(),
        after.sha256(),
        before_index,
        after_index,
        a.map(|u| (&u.raw_sha256, u.source_start_byte, u.source_end_byte)),
        b.map(|u| (&u.raw_sha256, u.source_start_byte, u.source_end_byte)),
    ))
    .expect("fixed unit identity serializes");
    UnitChange {
        id: hash(&identity),
        before_index,
        after_index,
        status,
        matched_by: method,
        raw_changed,
        render_changed,
        position_changed: before_index.zip(after_index).is_some_and(|(a, b)| a != b),
    }
}

#[cfg(test)]
mod tests;
