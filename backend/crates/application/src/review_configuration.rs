//! Bounded metadata intent; current scope, roles, versions and operation replay
//! are rechecked by persistence. This action does not submit or approve content.
use crate::proposal::UiImpact;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use specspace_git_adapter::CommitSha;
use std::collections::HashSet;
use uuid::Uuid;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum ReviewArea {
    Product,
    Development,
    Design,
}
impl ReviewArea {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Product => "PRODUCT",
            Self::Development => "DEVELOPMENT",
            Self::Design => "DESIGN",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReviewerSelection {
    pub reviewer_id: Uuid,
    pub domain: ReviewArea,
    pub required: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReviewSettings {
    pub ui_impact: UiImpact,
    pub ui_impact_reason: Option<String>,
    pub assignments: Vec<ReviewerSelection>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReviewConfigurationRequest {
    pub request_id: Uuid,
    pub expected_head_sha: String,
    pub expected_proposal_version: i64,
    pub expected_configuration_version: i64,
    pub settings: ReviewSettings,
}
impl ReviewConfigurationRequest {
    pub fn normalize(&mut self) -> Result<(), &'static str> {
        if self.request_id.is_nil()
            || self.expected_proposal_version < 1
            || self.expected_configuration_version < 1
            || self.settings.assignments.len() > 200
        {
            return Err("INVALID_REVIEW_CONFIGURATION");
        }
        self.expected_head_sha = CommitSha::parse(&self.expected_head_sha)
            .map_err(|_| "INVALID_REVIEW_CONFIGURATION")?
            .as_str()
            .into();
        if let Some(reason) = self.settings.ui_impact_reason.take() {
            if reason.contains('\0') || reason.chars().count() > 10000 {
                return Err("INVALID_UI_IMPACT_REASON");
            }
            self.settings.ui_impact_reason =
                (!reason.trim().is_empty()).then(|| reason.trim().to_owned());
        }
        if self.settings.ui_impact == UiImpact::No && self.settings.ui_impact_reason.is_none() {
            return Err("UI_IMPACT_REASON_REQUIRED");
        }
        let mut seen = HashSet::new();
        if self.settings.assignments.iter().any(|selection| {
            selection.reviewer_id.is_nil()
                || !seen.insert((selection.reviewer_id, selection.domain))
        }) {
            return Err("INVALID_REVIEW_ASSIGNMENTS");
        }
        self.settings
            .assignments
            .sort_by_key(|selection| (selection.domain.as_str(), selection.reviewer_id));
        Ok(())
    }
    pub fn fingerprint(&self, organization: Uuid, proposal: Uuid, actor: Uuid) -> String {
        format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&(
                    "specspace-review-configuration-v1",
                    organization,
                    proposal,
                    actor,
                    &self.expected_head_sha,
                    self.expected_proposal_version,
                    self.expected_configuration_version,
                    &self.settings
                ))
                .expect("bounded metadata serializes")
            )
        )
    }
}
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReviewerStatus {
    pub reviewer_id: Uuid,
    pub domain: ReviewArea,
    pub required: bool,
    pub display_name: String,
    pub active: bool,
    pub qualified: bool,
}
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReviewerCandidate {
    pub reviewer_id: Uuid,
    pub display_name: String,
}
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReviewerCandidates {
    pub proposal_id: Uuid,
    pub configuration_version: i64,
    pub domain: ReviewArea,
    pub reviewers: Vec<ReviewerCandidate>,
    pub next_after: Option<Uuid>,
}
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReviewConfiguration {
    pub proposal_id: Uuid,
    pub proposal_version: i64,
    pub configuration_version: i64,
    pub policy_version: i64,
    pub head_sha: Option<String>,
    pub state: String,
    pub can_edit: bool,
    pub settings: ReviewSettings,
    pub reviewers: Vec<ReviewerStatus>,
}
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ConfigurationUpdated {
    pub proposal_id: Uuid,
    pub request_id: Uuid,
    pub applied_proposal_version: i64,
    pub applied_configuration_version: i64,
    pub changed: bool,
    pub replayed: bool,
    pub current: ReviewConfiguration,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> ReviewConfigurationRequest {
        ReviewConfigurationRequest {
            request_id: Uuid::new_v4(),
            expected_head_sha: "A".repeat(40),
            expected_proposal_version: 7,
            expected_configuration_version: 3,
            settings: ReviewSettings {
                ui_impact: UiImpact::No,
                ui_impact_reason: Some("  No UI change  ".into()),
                assignments: vec![
                    ReviewerSelection {
                        reviewer_id: Uuid::from_u128(2),
                        domain: ReviewArea::Product,
                        required: true,
                    },
                    ReviewerSelection {
                        reviewer_id: Uuid::from_u128(1),
                        domain: ReviewArea::Development,
                        required: false,
                    },
                ],
            },
        }
    }

    #[test]
    fn canonical_retry_identity_ignores_order_whitespace_and_request_key() {
        let mut a = request();
        let mut b = a.clone();
        b.request_id = Uuid::new_v4();
        b.expected_head_sha = b.expected_head_sha.to_lowercase();
        b.settings.ui_impact_reason = Some("No UI change".into());
        b.settings.assignments.reverse();
        a.normalize().unwrap();
        b.normalize().unwrap();
        let scope = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let hash = a.fingerprint(scope.0, scope.1, scope.2);
        assert_eq!(hash, b.fingerprint(scope.0, scope.1, scope.2));
        b.expected_configuration_version += 1;
        assert_ne!(hash, b.fingerprint(scope.0, scope.1, scope.2));
        assert_ne!(hash, a.fingerprint(Uuid::new_v4(), scope.1, scope.2));
        assert_ne!(hash, a.fingerprint(scope.0, Uuid::new_v4(), scope.2));
        assert_ne!(hash, a.fingerprint(scope.0, scope.1, Uuid::new_v4()));
        b = a.clone();
        b.settings.assignments[0].required = !b.settings.assignments[0].required;
        assert_ne!(hash, b.fingerprint(scope.0, scope.1, scope.2));
    }

    #[test]
    fn bounded_metadata_rejects_duplicate_assignments_and_invalid_reasons() {
        let mut duplicate = request();
        duplicate
            .settings
            .assignments
            .push(duplicate.settings.assignments[0].clone());
        assert_eq!(duplicate.normalize(), Err("INVALID_REVIEW_ASSIGNMENTS"));
        for reason in [" ".into(), "\0".into(), "א".repeat(10001)] {
            let mut invalid = request();
            invalid.settings.ui_impact_reason = Some(reason);
            assert!(invalid.normalize().is_err());
        }
        let mut boundary = request();
        boundary.settings.ui_impact_reason = Some("א".repeat(10000));
        boundary.normalize().unwrap();
        let mut empty = request();
        empty.settings.ui_impact = UiImpact::Undecided;
        empty.settings.ui_impact_reason = None;
        empty.settings.assignments.clear();
        // Incomplete metadata is editable; formal readiness belongs to submission.
        empty.normalize().unwrap();
    }
}
