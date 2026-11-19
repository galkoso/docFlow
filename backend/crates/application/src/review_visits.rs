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
