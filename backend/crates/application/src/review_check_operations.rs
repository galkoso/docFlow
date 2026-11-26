//! Administrator journal observation and explicit recovery. These receipts do
//! not establish live provider state, policy approval or merge permission.
use serde::{Deserialize, Serialize};
use uuid::Uuid;
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReviewCheckOperation {
    pub viewer_id: Uuid,
    pub organization_id: Uuid,
    pub proposal_id: Uuid,
    pub operation_id: Uuid,
    pub snapshot_id: Uuid,
    pub request_hash: String,
    pub purpose: String,
    pub desired_result: String,
    pub job_state: String,
    pub operation_state: String,
    pub attempts: i32,
    pub max_attempts: i32,
    pub write_phase: String,
    pub known_run: bool,
    pub published: bool,
    pub error_code: Option<String>,
    pub provider_verified: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReviewCheckRecoveryRequest {
    pub request_id: Uuid,
    pub expected_request_hash: String,
}
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReviewCheckRecoveryAccepted {
    pub proposal_id: Uuid,
    pub operation_id: Uuid,
    pub request_id: Uuid,
    pub mode: String,
    pub max_attempts: i32,
    pub replayed: bool,
}

#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReviewCheckOperationPage {
    pub viewer_id: Uuid,
    pub organization_id: Uuid,
    pub proposal_id: Uuid,
    pub after: Option<Uuid>,
    pub limit: i32,
    pub items: Vec<ReviewCheckOperation>,
    pub next_cursor: Option<Uuid>,
}
