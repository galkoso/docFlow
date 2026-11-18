//! Explicit operator admission, not a provider result or mutation receipt.
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PrRecoveryRequest {
    pub request_id: Uuid,
    pub expected_fingerprint: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum PrRecoveryMode {
    RetryUnstarted,
    ObserveStarted,
}
impl PrRecoveryMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RetryUnstarted => "RETRY_UNSTARTED",
            Self::ObserveStarted => "OBSERVE_STARTED",
        }
    }
}
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PrRecoveryAccepted {
    pub proposal_id: Uuid,
    pub snapshot_id: Uuid,
    pub request_id: Uuid,
    pub mode: PrRecoveryMode,
    pub replayed: bool,
}
