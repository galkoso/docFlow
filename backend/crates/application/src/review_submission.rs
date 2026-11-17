//! Small browser expectations and a server-captured authority digest. This does
//! not publish a review, create a PR, or imply successful provider work.
use crate::review_context::{ReviewExpectation, ReviewScope};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use specspace_domain::policy::{Membership, ReviewDomain, Role, UiImpact};
use specspace_git_adapter::CommitSha;
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReviewSubmissionRequest {
    pub expected_head_sha: String,
    pub expected_target_sha: String,
    pub expected_proposal_version: i64,
    pub expected_configuration_version: i64,
    pub expected_policy_version: i64,
}
pub struct ValidatedReviewSubmission {
    actor: Uuid,
    proposal: Uuid,
    request: ReviewSubmissionRequest,
    expectation: ReviewExpectation,
}
impl ValidatedReviewSubmission {
    pub fn new(
        actor: Uuid,
        proposal: Uuid,
        mut request: ReviewSubmissionRequest,
    ) -> Result<Self, &'static str> {
        if actor.is_nil()
            || proposal.is_nil()
            || request.expected_proposal_version < 1
            || request.expected_configuration_version < 1
            || request.expected_policy_version < 1
        {
            return Err("INVALID_REVIEW_SUBMISSION");
        }
        let head = CommitSha::parse(&request.expected_head_sha)
            .map_err(|_| "INVALID_REVIEW_SUBMISSION")?;
        let target = CommitSha::parse(&request.expected_target_sha)
            .map_err(|_| "INVALID_REVIEW_SUBMISSION")?;
        request.expected_head_sha = head.as_str().into();
        request.expected_target_sha = target.as_str().into();
        let expectation = ReviewExpectation {
            head,
            target,
            proposal_version: request.expected_proposal_version,
            configuration_version: request.expected_configuration_version as u64,
            policy_version: request.expected_policy_version as u64,
        };
        Ok(Self {
            actor,
            proposal,
            request,
            expectation,
        })
    }
    pub fn actor(&self) -> Uuid {
        self.actor
    }
    pub fn proposal(&self) -> Uuid {
        self.proposal
    }
    pub fn request(&self) -> &ReviewSubmissionRequest {
        &self.request
    }
    pub fn expectation(&self) -> &ReviewExpectation {
        &self.expectation
    }
    pub fn fingerprint(&self, organization: Uuid) -> String {
        hash(&(
            "specspace-review-submission-v1",
            organization,
            self.actor,
            self.proposal,
            &self.request,
        ))
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReviewSubmissionAccepted {
    pub proposal_id: Uuid,
    pub operation_id: Uuid,
    pub job_id: Uuid,
}
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReviewSubmissionStatus {
    pub proposal_id: Uuid,
    pub operation_id: Uuid,
    pub job_id: Uuid,
    pub state: String,
    pub attempts: i32,
    pub phase: String,
    pub error_code: Option<String>,
    pub publication: Option<ReviewPublication>,
}
/// Durable snapshot outcome; PR synchronization and approvals are separate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReviewPublication {
    pub proposal_id: Uuid,
    pub operation_id: Uuid,
    pub job_id: Uuid,
    pub snapshot_id: Uuid,
    pub fingerprint: String,
    pub evidence_digest: String,
    pub proposal_version: i64,
    pub activated: bool,
    pub replayed: bool,
}
impl ReviewScope {
    /// Captures authority and metadata, rather than shared content identity.
    /// The job retains only this digest, not another copy of proposal text.
    pub fn authority_digest(&self) -> String {
        let read = &self.comparison;
        let review = &self.review;
        let mut assignments: Vec<_> = review
            .assignments
            .iter()
            .map(|item| (item.reviewer, domain(item.domain), item.required))
            .collect();
        assignments.sort_by_key(|item| (item.1, item.0));
        let mut members: Vec<_> = review.reviewer_memberships.iter().map(member).collect();
        members.sort_by_key(|item| item.1);
        hash(&(
            "specspace-review-authority-v1",
            (
                read.actor,
                read.organization,
                read.proposal,
                read.connection.repository,
                read.connection.remote_id,
                read.connection.installation_id,
                read.branch.as_str(),
                read.recorded_head.as_str(),
                read.recorded_base.as_str(),
                read.version,
                &read.state,
            ),
            (
                member(&review.actor),
                review.assigned_editor,
                review.repository_access,
            ),
            (
                review.proposal,
                review.repository,
                review.author,
                review.owner,
                review.team,
                &review.description,
            ),
            (
                review.expected_head.as_str(),
                review.expected_target.as_str(),
                review.expected_version,
                review.policy_version,
                review.configuration_version,
            ),
            (
                match review.ui_impact {
                    UiImpact::Yes => "YES",
                    UiImpact::No => "NO",
                    UiImpact::Undecided => "UNDECIDED",
                },
                &review.ui_impact_reason,
            ),
            assignments,
            members,
        ))
    }
}
fn hash(value: &impl Serialize) -> String {
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(value).expect("bounded review metadata serializes"))
    )
}
fn domain(value: ReviewDomain) -> &'static str {
    match value {
        ReviewDomain::Product => "PRODUCT",
        ReviewDomain::Development => "DEVELOPMENT",
        ReviewDomain::Design => "DESIGN",
    }
}
fn member(value: &Membership) -> (Uuid, Uuid, bool, &'static str, Vec<&'static str>) {
    let mut domains: Vec<_> = value.review_domains.iter().copied().map(domain).collect();
    domains.sort();
    domains.dedup();
    (
        value.organization_id,
        value.user_id,
        value.active,
        match value.role {
            Role::Reader => "READER",
            Role::Contributor => "CONTRIBUTOR",
            Role::Reviewer => "REVIEWER",
            Role::Maintainer => "MAINTAINER",
            Role::Admin => "ADMIN",
        },
        domains,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn request_identity_normalizes_sha_case_but_binds_every_scope_and_version() {
        let actor = Uuid::new_v4();
        let proposal = Uuid::new_v4();
        let org = Uuid::new_v4();
        let request = ReviewSubmissionRequest {
            expected_head_sha: "A".repeat(40),
            expected_target_sha: "B".repeat(40),
            expected_proposal_version: 2,
            expected_configuration_version: 2,
            expected_policy_version: 1,
        };
        let input = ValidatedReviewSubmission::new(actor, proposal, request.clone()).unwrap();
        let hash = input.fingerprint(org);
        let mut lower = request.clone();
        lower.expected_head_sha = lower.expected_head_sha.to_lowercase();
        lower.expected_target_sha = lower.expected_target_sha.to_lowercase();
        assert_eq!(
            hash,
            ValidatedReviewSubmission::new(actor, proposal, lower)
                .unwrap()
                .fingerprint(org)
        );
        for field in 0..5 {
            let mut changed = request.clone();
            match field {
                0 => changed.expected_head_sha = "1".repeat(40),
                1 => changed.expected_target_sha = "2".repeat(40),
                2 => changed.expected_proposal_version += 1,
                3 => changed.expected_configuration_version += 1,
                _ => changed.expected_policy_version += 1,
            }
            assert_ne!(
                hash,
                ValidatedReviewSubmission::new(actor, proposal, changed)
                    .unwrap()
                    .fingerprint(org)
            );
        }
        assert_ne!(hash, input.fingerprint(Uuid::new_v4()));
        assert_ne!(
            hash,
            ValidatedReviewSubmission::new(Uuid::new_v4(), proposal, request.clone())
                .unwrap()
                .fingerprint(org)
        );
        assert_ne!(
            hash,
            ValidatedReviewSubmission::new(actor, Uuid::new_v4(), request.clone())
                .unwrap()
                .fingerprint(org)
        );
        let mut invalid = request;
        invalid.expected_policy_version = 0;
        assert!(ValidatedReviewSubmission::new(actor, proposal, invalid).is_err());
    }
}
