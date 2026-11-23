//! Official snapshot decisions. Request fields identify an intent, never confer
//! reviewer eligibility or provider freshness. Comments do not withdraw decisions.
use crate::review_configuration::ReviewArea;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum ReviewDecision {
    Approve,
    ApproveAll,
    RequestChanges,
    Comment,
}
impl ReviewDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Approve => "APPROVE",
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
            || self.decision != ReviewDecision::Approve && self.explanation.is_empty()
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

#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct OfficialReviewEntry {
    pub id: Uuid,
    pub reviewer_id: Uuid,
    pub reviewer_name: String,
    pub domain: ReviewArea,
    pub decision: ReviewDecision,
    pub explanation: String,
    pub recorded_at: String,
    pub effective: bool,
    pub reviewer_eligible: bool,
    /// Database context, current eligibility and positive-decision coverage only;
    /// provider refs are not read. An eligible effective decision may be invalid.
    pub valid_for_stored_context: bool,
}
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct OfficialReviewHistory {
    pub proposal_id: Uuid,
    pub snapshot_id: Uuid,
    pub fingerprint: String,
    pub head_sha: String,
    pub target_sha: String,
    pub policy_version: i64,
    pub configuration_version: i64,
    pub current_for_metadata: bool,
    /// Current caller assignments; each POST independently rechecks eligibility.
    pub domains: Vec<ReviewArea>,
    pub approve_all_available: bool,
    /// Caller domains ready for regular approval after all mandatory individual
    /// approvals. Submission independently rechecks coverage and provider refs.
    pub approve_domains: Vec<ReviewArea>,
    /// True when any eligible domain is unread/unknown, or no domain is eligible.
    pub unviewed_content_warning: bool,
    /// Eligible domains with incomplete or unavailable acknowledged coverage.
    /// These read facts do not confer section approval or permission to merge.
    pub unviewed_domains: Vec<ReviewArea>,
    pub read_at: String,
    pub entries: Vec<OfficialReviewEntry>,
    pub next_after: Option<Uuid>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_regular_approval_allows_an_empty_optional_explanation() {
        for decision in [
            ReviewDecision::Approve,
            ReviewDecision::ApproveAll,
            ReviewDecision::RequestChanges,
            ReviewDecision::Comment,
        ] {
            let mut request = OfficialReviewRequest {
                request_id: Uuid::new_v4(),
                expected_fingerprint: "a".repeat(64),
                domain: ReviewArea::Product,
                decision,
                explanation: " \n ".into(),
            };
            assert_eq!(
                request.normalize().is_ok(),
                decision == ReviewDecision::Approve
            );
        }
    }
}
