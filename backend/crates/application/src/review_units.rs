//! Mandatory source requirements for one verified comparison. These descriptors
//! are neither visits nor approvals. The enclosing sealed snapshot supplies
//! reviewer/domain/policy authority. Matching unit IDs never transfer a visit or
//! approval between snapshots.
use crate::{
    comparison::{Change, Kind, Side},
    comparison_reader::LoadedComparison,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use specspace_document_engine::review_sections::{self, SectionError, Sections, SourceRange};
use std::collections::{HashMap, HashSet};

pub const MAX_REQUIRED_UNITS: usize = 40_000;
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileRequirement {
    NonDocument,
    NonSection,
    Layout,
    Unstructured,
    SectionLimit,
    ManifestLimit,
}
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Requirement {
    Section {
        document_id: String,
        section_id: String,
    },
    File {
        reason: FileRequirement,
    },
}
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnitSource {
    pub source: Side,
    pub range: Option<SourceRange>,
}
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequiredReviewUnit {
    pub id: String,
    pub file_id: String,
    pub requirement: Requirement,
    pub before: Option<UnitSource>,
    pub after: Option<UnitSource>,
}
pub(crate) fn prepare(loaded: &LoadedComparison) -> Result<Vec<RequiredReviewUnit>, &'static str> {
    prepare_with_limit(loaded, MAX_REQUIRED_UNITS)
}
pub(crate) fn prepare_with_limit(
    loaded: &LoadedComparison,
    limit: usize,
) -> Result<Vec<RequiredReviewUnit>, &'static str> {
    let changes = &loaded.inventory().changes;
    if changes.is_empty() || changes.len() > limit || limit > MAX_REQUIRED_UNITS {
        return Err("REVIEW_UNIT_SCOPE_INVALID");
    }
    let mut remaining = limit - changes.len();
    let mut units = Vec::new();
    for change in changes {
        let before = change
            .before
            .as_ref()
            .and_then(|s| loaded.before().source(&s.path));
        let after = change
            .after
            .as_ref()
            .and_then(|s| loaded.after().source(&s.path));
        if matches!(change.kind, Kind::Document | Kind::Catalog)
            && (change.before.is_some() && before.is_none()
                || change.after.is_some() && after.is_none())
        {
            return Err("REVIEW_UNIT_SOURCE_MISSING");
        }
        let mut file = file_units(change, before, after)?;
        if file.len() - 1 > remaining {
            file = vec![whole_file(
                change,
                before,
                after,
                FileRequirement::ManifestLimit,
            )?];
        }
        remaining -= file.len() - 1;
        units.extend(file);
    }
    Ok(units)
}

/// Re-derive complete requirement evidence from actual, verified source. A
/// sealed manifest fallback is deliberately whole-file, never an arbitrary span.
pub(crate) fn verify_source_unit(
    required: &RequiredReviewUnit,
    change: &Change,
    before: Option<&str>,
    after: Option<&str>,
) -> Result<(), &'static str> {
    let expected = if matches!(
        required.requirement,
        Requirement::File {
            reason: FileRequirement::ManifestLimit
        }
    ) {
        vec![whole_file(
            change,
            before,
            after,
            FileRequirement::ManifestLimit,
        )?]
    } else {
        file_units(change, before, after)?
    };
    if expected.iter().any(|unit| unit == required) {
        Ok(())
    } else {
        Err("REVIEW_UNIT_SOURCE_MISMATCH")
    }
}

fn file_units(
    change: &Change,
    before: Option<&str>,
    after: Option<&str>,
) -> Result<Vec<RequiredReviewUnit>, &'static str> {
    if change.kind != Kind::Document {
        return Ok(vec![whole_file(
            change,
            before,
            after,
            FileRequirement::NonDocument,
        )?]);
    }
    let old_id = change
        .before
        .as_ref()
        .and_then(|s| s.document_id.as_deref());
    let new_id = change.after.as_ref().and_then(|s| s.document_id.as_deref());
    let id = new_id.or(old_id);
    if id.is_none() || old_id.is_some() && new_id.is_some() && old_id != new_id {
        return Ok(vec![whole_file(
            change,
            before,
            after,
            FileRequirement::Unstructured,
        )?]);
    }
    let id = id.unwrap();
    let old = before.map(|s| review_sections::prepare(s, id)).transpose();
    let new = after.map(|s| review_sections::prepare(s, id)).transpose();
    let (old, new) = match (old, new) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(SectionError::SectionLimit), _) | (_, Err(SectionError::SectionLimit)) => {
            return Ok(vec![whole_file(
                change,
                before,
                after,
                FileRequirement::SectionLimit,
            )?]);
        }
        _ => {
            return Ok(vec![whole_file(
                change,
                before,
                after,
                FileRequirement::Unstructured,
            )?]);
        }
    };
    fn map(value: &Option<Sections>) -> HashMap<&str, &SourceRange> {
        value
            .as_ref()
            .map(|v| {
                v.sections
                    .iter()
                    .map(|s| (s.id.as_str(), &s.range))
                    .collect::<HashMap<_, _>>()
            })
            .unwrap_or_default()
    }
    let old_map = map(&old);
    let new_map = map(&new);
    fn order<'a>(value: &'a Option<Sections>, other: &HashMap<&str, &SourceRange>) -> Vec<&'a str> {
        value
            .as_ref()
            .map(|v| {
                v.sections
                    .iter()
                    .filter(|s| other.contains_key(s.id.as_str()))
                    .map(|s| s.id.as_str())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    }
    let moved = order(&old, &new_map) != order(&new, &old_map);
    let preamble_changed =
        old.as_ref().map(|s| &s.preamble.sha256) != new.as_ref().map(|s| &s.preamble.sha256);
    let metadata_changed = change.mode_changed
        || change.catalog_position_changed
        || change.before.as_ref().map(|s| &s.path) != change.after.as_ref().map(|s| &s.path);
    let mut result = Vec::new();
    if moved {
        result.push(whole_file(change, before, after, FileRequirement::Layout)?);
    } else if preamble_changed || metadata_changed {
        result.push(unit(
            change,
            Requirement::File {
                reason: FileRequirement::NonSection,
            },
            old.as_ref().map(|v| v.preamble.clone()),
            new.as_ref().map(|v| v.preamble.clone()),
        )?);
    }
    let mut seen = HashSet::new();
    for section in old.iter().chain(new.iter()).flat_map(|v| &v.sections) {
        if !seen.insert(&section.id) {
            continue;
        }
        let before = old_map.get(section.id.as_str()).copied();
        let after = new_map.get(section.id.as_str()).copied();
        if before.map(|r| &r.sha256) != after.map(|r| &r.sha256) {
            result.push(unit(
                change,
                Requirement::Section {
                    document_id: id.into(),
                    section_id: section.id.clone(),
                },
                before.cloned(),
                after.cloned(),
            )?);
        }
    }
    // A changed file must always have a requirement, including an unforeseen
    // parser/alignment equivalence. Never turn unknown evidence into no work.
    if result.is_empty() {
        result.push(whole_file(
            change,
            before,
            after,
            FileRequirement::NonSection,
        )?);
    }
    Ok(result)
}
fn whole_file(
    change: &Change,
    before: Option<&str>,
    after: Option<&str>,
    reason: FileRequirement,
) -> Result<RequiredReviewUnit, &'static str> {
    unit(
        change,
        Requirement::File { reason },
        before.map(review_sections::whole),
        after.map(review_sections::whole),
    )
}
fn unit(
    change: &Change,
    requirement: Requirement,
    before: Option<SourceRange>,
    after: Option<SourceRange>,
) -> Result<RequiredReviewUnit, &'static str> {
    let section = matches!(requirement, Requirement::Section { .. });
    let before = change
        .before
        .as_ref()
        .filter(|_| !section || before.is_some())
        .map(|source| UnitSource {
            source: source.clone(),
            range: before,
        });
    let after = change
        .after
        .as_ref()
        .filter(|_| !section || after.is_some())
        .map(|source| UnitSource {
            source: source.clone(),
            range: after,
        });
    let identity = serde_json::to_vec(&(
        "specspace-required-review-unit-v1",
        &change.id,
        &requirement,
        &before,
        &after,
    ))
    .map_err(|_| "REVIEW_UNIT_IDENTITY_FAILED")?;
    Ok(RequiredReviewUnit {
        id: format!("{:x}", Sha256::digest(identity)),
        file_id: change.id.clone(),
        requirement,
        before,
        after,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::comparison::{MatchMethod, Status};
    const PREFIX: &str =
        "---\ndocument_id: DOC-ONE\ntitle: Original\nowner_team: core\nschema_version: 1\n---\n";
    const A: &str = "<!-- ss:section:SEC-A -->\n# First\n\nOriginal A\n";
    const B: &str = "<!-- ss:section:SEC-B -->\n## Second\n\nOriginal B\n";
    fn side(path: &str, source: &str) -> Side {
        Side {
            path: path.into(),
            blob_oid: specspace_git_adapter::blob::oid(source.as_bytes(), 40)
                .unwrap()
                .as_str()
                .into(),
            bytes: source.len() as u64,
            executable: false,
            sha256: Some(format!("{:x}", Sha256::digest(source))),
            document_id: specspace_document_engine::markdown::front_matter(source)
                .ok()
                .map(|(m, _)| m.document_id),
            catalog_position: Some(0),
        }
    }
    fn change(before: &str, after: &str) -> Change {
        Change {
            id: "a".repeat(64),
            kind: Kind::Document,
            status: Status::Modified,
            matched_by: Some(MatchMethod::DocumentId),
            before: Some(side("docs/a.md", before)),
            after: Some(side("docs/a.md", after)),
            content_changed: before != after,
            mode_changed: false,
            catalog_position_changed: false,
        }
    }
    #[test]
    fn changed_sections_include_full_raw_context_but_unchanged_sections_are_not_requirements() {
        let before = format!("{PREFIX}{A}{B}");
        let after = before.replace(
            "Original A",
            "New A\n<script>raw()</script>\n![image](assets/a.png)",
        );
        let units = file_units(&change(&before, &after), Some(&before), Some(&after)).unwrap();
        assert_eq!(units.len(), 1);
        assert_eq!(
            units[0].requirement,
            Requirement::Section {
                document_id: "DOC-ONE".into(),
                section_id: "SEC-A".into()
            }
        );
        let range = units[0].after.as_ref().unwrap().range.as_ref().unwrap();
        assert!(after[range.start_byte..range.end_byte].contains("<script>raw()</script>"));
        let encoded = serde_json::to_string(&units).unwrap();
        assert!(!encoded.contains("Original A"));
        assert!(!encoded.contains("raw()"));
        let next = after.replace("Original B", "New B");
        let next_units = file_units(&change(&before, &next), Some(&before), Some(&next)).unwrap();
        assert_ne!(units[0].id, next_units[0].id);
        assert_eq!(next_units.len(), 2);
    }
    #[test]
    fn added_and_deleted_sections_keep_absent_sides_distinct_from_existing_files() {
        let before = format!("{PREFIX}{A}");
        let after = format!("{PREFIX}{B}");
        let units = file_units(&change(&before, &after), Some(&before), Some(&after)).unwrap();
        assert_eq!(units.len(), 2);
        assert!(units[0].before.is_some() && units[0].after.is_none());
        assert!(units[1].before.is_none() && units[1].after.is_some());
    }
    #[test]
    fn front_matter_paths_and_section_order_remain_mandatory_review_work() {
        let before = format!("{PREFIX}{A}{B}");
        let after = before.replace("title: Original", "title: Updated");
        let units = file_units(&change(&before, &after), Some(&before), Some(&after)).unwrap();
        assert_eq!(units.len(), 1);
        assert_eq!(
            units[0].requirement,
            Requirement::File {
                reason: FileRequirement::NonSection
            }
        );
        assert_eq!(
            units[0]
                .after
                .as_ref()
                .unwrap()
                .range
                .as_ref()
                .unwrap()
                .end_byte,
            PREFIX.replace("Original", "Updated").len()
        );
        let reordered = format!("{PREFIX}{B}{A}");
        let units = file_units(
            &change(&before, &reordered),
            Some(&before),
            Some(&reordered),
        )
        .unwrap();
        assert_eq!(units.len(), 1);
        assert_eq!(
            units[0].requirement,
            Requirement::File {
                reason: FileRequirement::Layout
            }
        );
        assert_eq!(
            units[0]
                .after
                .as_ref()
                .unwrap()
                .range
                .as_ref()
                .unwrap()
                .end_byte,
            reordered.len()
        );
        let mut renamed = change(&before, &before);
        renamed.after.as_mut().unwrap().path = "docs/new.md".into();
        let units = file_units(&renamed, Some(&before), Some(&before)).unwrap();
        assert_eq!(units.len(), 1);
        assert_ne!(
            units[0].before.as_ref().unwrap().source.path,
            units[0].after.as_ref().unwrap().source.path
        );
    }
    #[test]
    fn unstructured_and_excessive_sections_have_explicit_whole_file_requirements() {
        let after = format!("{PREFIX}{A}");
        let before = "unstructured old source";
        let units = file_units(&change(before, &after), Some(before), Some(&after)).unwrap();
        assert_eq!(
            units[0].requirement,
            Requirement::File {
                reason: FileRequirement::Unstructured
            }
        );
        assert_eq!(
            units[0]
                .before
                .as_ref()
                .unwrap()
                .range
                .as_ref()
                .unwrap()
                .end_byte,
            before.len()
        );
        let mut many = PREFIX.to_owned();
        for n in 0..=review_sections::MAX_SECTIONS {
            many.push_str(&format!("<!-- ss:section:SEC-{n} -->\n# Heading\n"));
        }
        let units = file_units(&change(&after, &many), Some(&after), Some(&many)).unwrap();
        assert_eq!(units.len(), 1);
        assert_eq!(
            units[0].requirement,
            Requirement::File {
                reason: FileRequirement::SectionLimit
            }
        );
    }
    #[test]
    fn non_document_and_source_absence_never_become_zero_coverage() {
        let mut asset = change("before", "after");
        asset.kind = Kind::Asset;
        asset.before.as_mut().unwrap().path = "assets/before.png".into();
        asset.after.as_mut().unwrap().path = "assets/after.png".into();
        let units = file_units(&asset, None, None).unwrap();
        assert_eq!(units.len(), 1);
        assert_eq!(
            units[0].requirement,
            Requirement::File {
                reason: FileRequirement::NonDocument
            }
        );
        assert!(units[0].before.as_ref().unwrap().range.is_none());
        assert!(units[0].after.as_ref().unwrap().range.is_none());
    }
}
