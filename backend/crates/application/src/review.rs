//! Pure preparation of a shared review intent from verified Git snapshots.
//! This is neither authorization, a provider check, media verification, a PR,
//! a published review snapshot nor an approval. A fenced submission worker must
//! recheck fresh context/refs, verify required media and publish atomically.
use crate::{
    comparison::{Change, Kind},
    comparison_reader::LoadedComparison,
    review_snapshot::ReviewBookDocument,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use specspace_document_engine::{
    catalog::Catalog,
    media,
    save_validation::{self, SaveValidation},
};
use specspace_domain::policy::{self, Action, Membership, ResourceAccess, ReviewDomain, UiImpact};
use specspace_git_adapter::CommitSha;
use std::{
    collections::{BTreeMap, HashSet},
    io::{self, BufWriter, Write},
};
use uuid::Uuid;

#[derive(Clone, PartialEq, Eq)]
pub struct ReviewAssignment {
    pub reviewer: Uuid,
    pub domain: ReviewDomain,
    pub required: bool,
}
/// Resolved by a current server-side authorization/configuration read. No
/// Deserialize implementation: browser selection is not authority for a scope.
#[derive(Clone, PartialEq, Eq)]
pub struct ReviewContext {
    pub actor: Membership,
    pub assigned_editor: bool,
    pub repository_access: bool,
    pub proposal: Uuid,
    pub repository: Uuid,
    pub author: Uuid,
    pub owner: Uuid,
    pub team: Uuid,
    pub description: String,
    pub expected_head: CommitSha,
    pub expected_target: CommitSha,
    pub expected_version: i64,
    pub policy_version: u64,
    pub configuration_version: u64,
    pub ui_impact: UiImpact,
    pub ui_impact_reason: Option<String>,
    pub assignments: Vec<ReviewAssignment>,
    pub reviewer_memberships: Vec<Membership>,
}
#[derive(Debug)]
pub enum ReviewError {
    Invalid(&'static str),
    Validation(SaveValidation),
}
impl ReviewError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid(code) => code,
            Self::Validation(_) => "REVIEW_VALIDATION_FAILED",
        }
    }
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct AssignedReviewer {
    pub reviewer_id: Uuid,
    pub domain: &'static str,
    pub required: bool,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct RequiredMedia {
    pub path: String,
    pub blob_oid: String,
    pub bytes: u64,
}
#[derive(Debug, Serialize)]
pub struct ReviewSummary {
    pub fingerprint: String,
    pub proposal_id: Uuid,
    pub repository_id: Uuid,
    pub branch: String,
    pub head_sha: String,
    pub target_sha: String,
    pub merge_base_sha: String,
    pub recorded_base_sha: String,
    pub comparison_content_id: String,
    pub expected_proposal_version: i64,
    pub policy_version: u64,
    pub configuration_version: u64,
    pub ui_impact: &'static str,
    pub base_outdated: bool,
    pub changed_files: usize,
    pub content_findings_total: usize,
    pub media_checks_required: usize,
}
/// Complete immutable manifest metadata, with source bodies kept out of DTOs.
/// A summary is not a claim that all approval/merge gates have passed.
pub struct PreparedReview {
    context: ReviewContext,
    summary: ReviewSummary,
    files: Vec<Change>,
    book_documents: Vec<ReviewBookDocument>,
    assignments: Vec<AssignedReviewer>,
    media: Vec<RequiredMedia>,
    validation: SaveValidation,
}
impl PreparedReview {
    pub fn context(&self) -> &ReviewContext {
        &self.context
    }
    pub fn summary(&self) -> &ReviewSummary {
        &self.summary
    }
    pub fn files(&self) -> &[Change] {
        &self.files
    }
    pub fn book_documents(&self) -> &[ReviewBookDocument] {
        &self.book_documents
    }
    pub fn assignments(&self) -> &[AssignedReviewer] {
        &self.assignments
    }
    pub fn required_media(&self) -> &[RequiredMedia] {
        &self.media
    }
    pub fn validation(&self) -> &SaveValidation {
        &self.validation
    }
    pub fn prepare(context: ReviewContext, loaded: &LoadedComparison) -> Result<Self, ReviewError> {
        if context.actor.user_id != loaded.actor()
            || context.actor.organization_id != loaded.organization()
            || context.proposal != loaded.proposal()
            || context.repository != loaded.repository()
            || !policy::authorized(
                &context.actor,
                &ResourceAccess {
                    organization_id: loaded.organization(),
                    repository_access: context.repository_access,
                    assigned_editor: context.assigned_editor,
                    export_allowed: false,
                },
                Action::Edit,
            )
        {
            return Err(ReviewError::Invalid("REVIEW_NOT_AUTHORIZED"));
        }
        if [context.author, context.owner, context.team]
            .iter()
            .any(Uuid::is_nil)
            || context.expected_version < 1
            || context.policy_version == 0
            || context.configuration_version == 0
            || !valid_text(&context.description, 10, 10000)
            || context
                .ui_impact_reason
                .as_ref()
                .is_some_and(|reason| !valid_text(reason, 1, 10000))
            || context.ui_impact == UiImpact::No && context.ui_impact_reason.is_none()
        {
            return Err(ReviewError::Invalid("INVALID_REVIEW_CONTEXT"));
        }
        if context.expected_version != loaded.version()
            || &context.expected_head != loaded.recorded_head()
            || &context.expected_head != loaded.head()
            || &context.expected_target != loaded.target()
            || loaded.historical_head()
        {
            return Err(ReviewError::Invalid("REVIEW_CONTEXT_CHANGED"));
        }
        if !matches!(
            loaded.state(),
            "DRAFT" | "IN_REVIEW" | "CHANGES_REQUESTED" | "APPROVED"
        ) {
            return Err(ReviewError::Invalid("PROPOSAL_NOT_REVIEWABLE"));
        }
        if loaded.external_branch_change() {
            return Err(ReviewError::Invalid("REVIEW_BRANCH_CHANGED"));
        }
        if loaded.target_outdated() {
            return Err(ReviewError::Invalid("REVIEW_TARGET_CHANGED"));
        }
        if loaded.inventory().changes.is_empty() {
            return Err(ReviewError::Invalid("REVIEW_NO_CHANGES"));
        }
        if loaded.after().findings_total() != 0 {
            return Err(ReviewError::Invalid("REVIEW_SOURCE_INVALID"));
        }
        let assignments = reviewers(&context)?;
        let catalog: Catalog = serde_json::from_str(
            loaded
                .after()
                .source(".specspace/catalog.json")
                .ok_or(ReviewError::Invalid("REVIEW_CATALOG_MISSING"))?,
        )
        .map_err(|_| ReviewError::Invalid("REVIEW_CATALOG_INVALID"))?;
        let mut files = vec![];
        let mut assets = HashSet::new();
        let mut after = BTreeMap::new();
        for (side, source) in loaded.after().entries() {
            if side.path.starts_with("docs/") && side.path.ends_with(".md") {
                files.push((
                    side.path.clone(),
                    source
                        .ok_or(ReviewError::Invalid("REVIEW_SOURCE_MISSING"))?
                        .to_owned(),
                ));
            }
            if side.path.starts_with("assets/") {
                assets.insert(side.path.clone());
            }
            after.insert(side.path.as_str(), side);
        }
        let validation = save_validation::validate(&catalog, &files, &assets);
        if !validation.ready() {
            return Err(ReviewError::Validation(validation));
        }
        let mut book_documents = Vec::with_capacity(catalog.documents.len());
        for entry in &catalog.documents {
            let side = after
                .get(entry.path.as_str())
                .ok_or(ReviewError::Invalid("REVIEW_SOURCE_MISSING"))?;
            if side.document_id.as_deref() != Some(entry.document_id.as_str())
                || side.catalog_position != Some(book_documents.len())
                || side.sha256.is_none()
                || side.executable
            {
                return Err(ReviewError::Invalid("REVIEW_DOCUMENT_IDENTITY_INVALID"));
            }
            book_documents.push(ReviewBookDocument {
                id: ReviewBookDocument::identity(loaded.repository(), loaded.head(), side)
                    .map_err(|_| ReviewError::Invalid("REVIEW_FINGERPRINT_FAILED"))?,
                source: (*side).clone(),
            });
        }
        let mut media_paths = validation.required_assets.clone();
        for change in &loaded.inventory().changes {
            if change.kind == Kind::Other {
                return Err(ReviewError::Invalid("REVIEW_CHANGED_PATH_UNSUPPORTED"));
            }
            if let Some(side) = &change.after {
                if side.executable {
                    return Err(ReviewError::Invalid("REVIEW_EXECUTABLE_UNSUPPORTED"));
                }
                if change.kind == Kind::Asset {
                    media_paths.insert(side.path.clone());
                }
            }
        }
        let mut media = vec![];
        for path in media_paths {
            let side = after
                .get(path.as_str())
                .ok_or(ReviewError::Invalid("REVIEW_ASSET_MISSING"))?;
            if !media::valid_path(&path)
                || side.executable
                || side.bytes > media::MAX_ASSET_BYTES as u64
            {
                return Err(ReviewError::Invalid("REVIEW_MEDIA_UNSUPPORTED"));
            }
            media.push(RequiredMedia {
                path,
                blob_oid: side.blob_oid.clone(),
                bytes: side.bytes,
            });
        }
        let impact = match context.ui_impact {
            UiImpact::Yes => "YES",
            UiImpact::No => "NO",
            UiImpact::Undecided => "UNDECIDED",
        };
        let mut writer = BufWriter::new(HashWriter(Sha256::new()));
        serde_json::to_writer(
            &mut writer,
            &(
                "specspace-review-plan-v2",
                loaded.organization(),
                loaded.repository(),
                loaded.binding().remote_id,
                loaded.binding().installation_id,
                loaded.proposal(),
                loaded.branch().as_str(),
                (
                    loaded.head().as_str(),
                    loaded.target().as_str(),
                    loaded.merge_base().as_str(),
                    loaded.recorded_base().as_str(),
                ),
                (
                    context.author,
                    context.owner,
                    context.team,
                    &context.description,
                    context.policy_version,
                    context.configuration_version,
                    impact,
                    &context.ui_impact_reason,
                ),
                &assignments,
                &loaded.inventory().changes,
                &media,
                &book_documents,
            ),
        )
        .map_err(|_| ReviewError::Invalid("REVIEW_FINGERPRINT_FAILED"))?;
        writer
            .flush()
            .map_err(|_| ReviewError::Invalid("REVIEW_FINGERPRINT_FAILED"))?;
        let fingerprint = format!(
            "{:x}",
            writer
                .into_inner()
                .map_err(|_| ReviewError::Invalid("REVIEW_FINGERPRINT_FAILED"))?
                .0
                .finalize()
        );
        let summary = ReviewSummary {
            fingerprint,
            proposal_id: loaded.proposal(),
            repository_id: loaded.repository(),
            branch: loaded.branch().as_str().into(),
            head_sha: loaded.head().as_str().into(),
            target_sha: loaded.target().as_str().into(),
            merge_base_sha: loaded.merge_base().as_str().into(),
            recorded_base_sha: loaded.recorded_base().as_str().into(),
            comparison_content_id: loaded.id().into(),
            expected_proposal_version: context.expected_version,
            policy_version: context.policy_version,
            configuration_version: context.configuration_version,
            ui_impact: impact,
            base_outdated: loaded.base_outdated(),
            changed_files: loaded.inventory().changes.len(),
            content_findings_total: validation.findings_total,
            media_checks_required: media.len(),
        };
        Ok(Self {
            context,
            summary,
            files: loaded.inventory().changes.clone(),
            book_documents,
            assignments,
            media,
            validation,
        })
    }
}
fn reviewers(context: &ReviewContext) -> Result<Vec<AssignedReviewer>, ReviewError> {
    if context.assignments.is_empty()
        || context.assignments.len() > 200
        || context.reviewer_memberships.len() > 200
    {
        return Err(ReviewError::Invalid("REVIEW_ASSIGNMENTS_REQUIRED"));
    }
    let mut users = HashSet::new();
    if context.reviewer_memberships.iter().any(|member| {
        member.organization_id != context.actor.organization_id
            || member.user_id.is_nil()
            || !users.insert(member.user_id)
    }) {
        return Err(ReviewError::Invalid("REVIEW_MEMBERSHIP_INVALID"));
    }
    let mut pairs = HashSet::new();
    let mut result = vec![];
    for assignment in &context.assignments {
        if assignment.reviewer == context.author
            || !pairs.insert((assignment.reviewer, assignment.domain))
        {
            return Err(ReviewError::Invalid("REVIEW_ASSIGNMENT_INVALID"));
        }
        let member = context
            .reviewer_memberships
            .iter()
            .find(|member| member.user_id == assignment.reviewer)
            .ok_or(ReviewError::Invalid("REVIEWER_NOT_ELIGIBLE"))?;
        if !member.review_domains.contains(&assignment.domain)
            || !policy::authorized(
                member,
                &ResourceAccess {
                    organization_id: context.actor.organization_id,
                    repository_access: true,
                    assigned_editor: false,
                    export_allowed: false,
                },
                Action::Review,
            )
        {
            return Err(ReviewError::Invalid("REVIEWER_NOT_ELIGIBLE"));
        }
        result.push(AssignedReviewer {
            reviewer_id: assignment.reviewer,
            domain: area(assignment.domain),
            required: assignment.required,
        });
    }
    let independent = result.iter().any(|product| {
        product.domain == "PRODUCT"
            && result.iter().any(|development| {
                development.domain == "DEVELOPMENT"
                    && product.reviewer_id != development.reviewer_id
            })
    });
    if !independent {
        return Err(ReviewError::Invalid("INDEPENDENT_REVIEWERS_REQUIRED"));
    }
    if context.ui_impact == UiImpact::Yes
        && !result.iter().any(|reviewer| reviewer.domain == "DESIGN")
    {
        return Err(ReviewError::Invalid("DESIGN_REVIEWER_REQUIRED"));
    }
    result.sort_by_key(|reviewer| (reviewer.domain, reviewer.reviewer_id));
    Ok(result)
}
fn area(domain: ReviewDomain) -> &'static str {
    match domain {
        ReviewDomain::Product => "PRODUCT",
        ReviewDomain::Development => "DEVELOPMENT",
        ReviewDomain::Design => "DESIGN",
    }
}
fn valid_text(value: &str, min: usize, max: usize) -> bool {
    !value.contains('\0')
        && (min..=max).contains(&value.trim().chars().count())
        && value.chars().count() <= max
}
struct HashWriter(Sha256);
impl Write for HashWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
