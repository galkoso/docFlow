//! Source delivery and explicit window acknowledgement; neither is an approval.
use crate::{
    comparison_file::discussion::AnchorSide, review_configuration::ReviewArea,
    review_units::UnitSource,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReviewSourceRequest {
    pub expected_fingerprint: String,
    pub domain: ReviewArea,
    pub unit_id: String,
    pub side: AnchorSide,
    pub offset: usize,
}
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReviewSourceWindow {
    pub actor_id: Uuid,
    pub proposal_id: Uuid,
    pub snapshot_id: Uuid,
    pub delivery_id: Uuid,
    pub fingerprint: String,
    pub domain: ReviewArea,
    pub unit_id: String,
    pub side: AnchorSide,
    pub source: UnitSource,
    pub head_sha: String,
    pub merge_base_sha: String,
    pub start_byte: usize,
    pub end_byte: usize,
    pub offset: usize,
    pub next_offset: Option<usize>,
    pub text: String,
    pub window_sha256: String,
    pub expires_at: String,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReviewVisitRequest {
    pub request_id: Uuid,
    pub delivery_id: Uuid,
    pub expected_fingerprint: String,
    pub domain: ReviewArea,
}
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReviewVisitRecorded {
    pub id: Uuid,
    pub request_id: Uuid,
    pub delivery_id: Uuid,
    pub actor_id: Uuid,
    pub proposal_id: Uuid,
    pub snapshot_id: Uuid,
    pub fingerprint: String,
    pub domain: ReviewArea,
    pub unit_id: String,
    pub side: AnchorSide,
    pub start_byte: usize,
    pub end_byte: usize,
    pub recorded_at: String,
    pub replayed: bool,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReviewCoverageRequest {
    pub expected_fingerprint: String,
    pub domain: ReviewArea,
    pub offset: i32,
    pub limit: i32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum ReviewSideCoverage {
    Absent,
    Unsupported,
    Unviewed,
    Partial,
    Viewed,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReviewUnitCoverage {
    pub unit_id: String,
    pub before: ReviewSideCoverage,
    pub after: ReviewSideCoverage,
    pub viewed: bool,
}
/// Current caller's exact snapshot/domain read evidence, never approval authority.
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReviewCoverage {
    pub actor_id: Uuid,
    pub proposal_id: Uuid,
    pub snapshot_id: Uuid,
    pub fingerprint: String,
    pub head_sha: String,
    pub domain: ReviewArea,
    pub current_for_metadata: bool,
    /// False for legacy snapshots without a complete sealed requirement set.
    pub available: bool,
    pub total_units: i32,
    pub viewed_units: i32,
    /// Includes partial and unsupported items, without treating them as viewed.
    pub unviewed_units: i32,
    pub unsupported_units: i32,
    pub offset: i32,
    pub next_offset: Option<i32>,
    pub units: Vec<ReviewUnitCoverage>,
}
