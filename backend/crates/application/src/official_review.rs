//! Official snapshot decisions. Request fields identify an intent, never confer
//! reviewer eligibility or provider freshness. Comments do not withdraw decisions.
use crate::review_configuration::ReviewArea;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum ReviewDecision {
    ApproveAll,
    RequestChanges,
    Comment,
}
impl ReviewDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ApproveAll => "APPROVE_ALL",
            Self::RequestChanges => "REQUEST_CHANGES",
            Self::Comment => "COMMENT",
        }
    }
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct OfficialReviewRequest {
    pub request_id: Uuid,
    pub expected_fingerprint: String,
    pub domain: ReviewArea,
    pub decision: ReviewDecision,
    pub explanation: String,
}
impl OfficialReviewRequest {
    pub fn normalize(&mut self) -> Result<(), &'static str> {
        self.explanation = self.explanation.trim().to_owned();
        if self.request_id.is_nil()
            || self.expected_fingerprint.len() != 64
            || !self
                .expected_fingerprint
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || self.explanation.is_empty()
            || self.explanation.chars().count() > 10000
            || self.explanation.contains('\0')
        {
            return Err("INVALID_OFFICIAL_REVIEW");
        }
        Ok(())
    }
}
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct OfficialReviewRecorded {
    pub id: Uuid,
    pub request_id: Uuid,
    pub proposal_id: Uuid,
    pub snapshot_id: Uuid,
    pub reviewer_id: Uuid,
    pub domain: ReviewArea,
    pub decision: ReviewDecision,
    pub head_sha: String,
    pub target_sha: String,
    pub policy_version: i64,
    pub configuration_version: i64,
    pub recorded_at: String,
    pub replayed: bool,
}
