//! Owned read models. Deserializing metadata never creates sealed review
//! evidence or grants permission to publish, approve or merge a snapshot.
use crate::{
    comparison::{Change, Side},
    review_configuration::ReviewerSelection,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use specspace_git_adapter::CommitSha;
use uuid::Uuid;

/// Complete head-book identity, including documents absent from the diff.
/// Deserializing this read model does not construct a source proof.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewBookDocument {
    pub id: String,
    pub source: Side,
}
impl ReviewBookDocument {
    /// Pure metadata identity; this calculation does not verify source or grant
    /// authority. The publication plan and source reader use the same encoding.
    pub fn identity(
        repository: Uuid,
        head: &CommitSha,
        source: &Side,
    ) -> Result<String, serde_json::Error> {
        let encoded = serde_json::to_vec(&(
            "specspace-review-book-document-v1",
            repository,
            head.as_str(),
            source,
        ))?;
        Ok(format!("{:x}", Sha256::digest(encoded)))
    }
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum ReviewSnapshotUiImpact {
    Yes,
    No,
    Undecided,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReviewSnapshotSummary {
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
    pub ui_impact: ReviewSnapshotUiImpact,
    pub base_outdated: bool,
    pub changed_files: usize,
    pub content_findings_total: usize,
    pub media_checks_required: usize,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReviewSnapshotContext {
    pub author_id: Uuid,
    pub owner_id: Uuid,
    pub team_id: Uuid,
    pub description: String,
    pub ui_impact: ReviewSnapshotUiImpact,
    pub ui_impact_reason: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum ReviewFindingSeverity {
    Warning,
    Blocker,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReviewSnapshotFinding {
    pub code: String,
    pub path: String,
    pub path_truncated: bool,
    pub severity: ReviewFindingSeverity,
    pub source_start_byte: Option<usize>,
    pub source_end_byte: Option<usize>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReviewSnapshotValidation {
    pub findings: Vec<ReviewSnapshotFinding>,
    pub findings_total: usize,
    pub findings_truncated: bool,
    pub blockers_total: usize,
    pub complete: bool,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReviewSnapshotMetadata {
    pub schema_version: u32,
    pub summary: ReviewSnapshotSummary,
    pub context: ReviewSnapshotContext,
    pub assignments: Vec<ReviewerSelection>,
    pub validation: ReviewSnapshotValidation,
}
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReviewSnapshot {
    pub snapshot_id: Uuid,
    pub proposal_id: Uuid,
    pub fingerprint: String,
    pub evidence_digest: String,
    pub media_validation_version: u32,
    pub title: String,
    pub proposal_number: i64,
    pub delivery_status: String,
    pub created_at: String,
    pub created_by: Uuid,
    pub metadata: ReviewSnapshotMetadata,
    pub file_count: i32,
    pub media_count: i32,
    /// Current database metadata only; does not assert fresh Git refs or approval.
    pub current_metadata: bool,
}
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReviewSnapshotFiles {
    pub snapshot_id: Uuid,
    pub offset: i32,
    pub total: i32,
    pub next_offset: Option<i32>,
    pub files: Vec<Change>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReviewMediaReceipt {
    pub path: String,
    pub blob_oid: String,
    pub bytes: u64,
    pub source_sha256: String,
    pub display_sha256: String,
    pub display_width: u32,
    pub display_height: u32,
}
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReviewSnapshotMedia {
    pub snapshot_id: Uuid,
    pub offset: i32,
    pub total: i32,
    pub next_offset: Option<i32>,
    pub receipts: Vec<ReviewMediaReceipt>,
}
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReviewSnapshotHistoryEntry {
    pub snapshot_id: Uuid,
    pub fingerprint: String,
    pub title: String,
    pub branch: String,
    pub head_sha: String,
    pub target_sha: String,
    pub prepared_version: i64,
    pub configuration_version: i64,
    pub policy_version: i64,
    pub file_count: i32,
    pub media_count: i32,
    pub created_at: String,
    pub created_by: Uuid,
}
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReviewSnapshotHistory {
    pub proposal_id: Uuid,
    pub number: i64,
    pub title: String,
    pub branch: String,
    pub target_branch: String,
    pub active_snapshot_id: Option<Uuid>,
    pub current_proposal_version: i64,
    pub current_proposal_state: String,
    pub snapshots: Vec<ReviewSnapshotHistoryEntry>,
    pub next_after: Option<Uuid>,
}
