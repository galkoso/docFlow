//! Stored readiness facts. This response never grants approval or merge authority.
use serde::Serialize;
use specspace_domain::review_check::{self, Conclusion, Gate};
use uuid::Uuid;

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum ReadinessConclusion {
    Pending,
    Failure,
    Success,
}
impl From<Conclusion> for ReadinessConclusion {
    fn from(value: Conclusion) -> Self {
        match value {
            Conclusion::Pending => Self::Pending,
            Conclusion::Failure => Self::Failure,
            Conclusion::Success => Self::Success,
        }
    }
}
#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum ReadinessGate {
    Evidence,
    ProposalState,
    Snapshot,
    Approvals,
    RequiredReviewer,
    BaseUpdate,
    ContentValidation,
    MediaValidation,
    SectionCoverage,
    BlockingThreads,
    BlockingQuestions,
}
impl From<Gate> for ReadinessGate {
    fn from(value: Gate) -> Self {
        match value {
            Gate::Evidence => Self::Evidence,
            Gate::ProposalState => Self::ProposalState,
            Gate::Snapshot => Self::Snapshot,
            Gate::Approvals => Self::Approvals,
            Gate::RequiredReviewer => Self::RequiredReviewer,
            Gate::BaseUpdate => Self::BaseUpdate,
            Gate::ContentValidation => Self::ContentValidation,
            Gate::MediaValidation => Self::MediaValidation,
            Gate::SectionCoverage => Self::SectionCoverage,
            Gate::BlockingThreads => Self::BlockingThreads,
            Gate::BlockingQuestions => Self::BlockingQuestions,
        }
    }
}
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReadinessIssue {
    pub gate: ReadinessGate,
    pub conclusion: ReadinessConclusion,
    pub code: String,
}
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReadinessEvaluation {
    pub conclusion: ReadinessConclusion,
    pub issues: Vec<ReadinessIssue>,
}
impl From<review_check::Evaluation> for ReadinessEvaluation {
    fn from(value: review_check::Evaluation) -> Self {
        Self {
            conclusion: value.conclusion.into(),
            issues: value
                .issues
                .into_iter()
                .map(|issue| ReadinessIssue {
                    gate: issue.gate.into(),
                    conclusion: issue.conclusion.into(),
                    code: issue.code.into(),
                })
                .collect(),
        }
    }
}
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReviewReadiness {
    pub actor_id: Uuid,
    pub proposal_id: Uuid,
    pub snapshot_id: Uuid,
    pub fingerprint: String,
    pub head_sha: String,
    pub target_sha: String,
    pub policy_version: i64,
    pub configuration_version: i64,
    /// Compares stored proposal/repository metadata only.
    pub current_for_metadata: bool,
    /// False for this stored report. Provider checks and merge must independently
    /// re-read refs and authority; this response cannot authorize either action.
    pub provider_refs_verified: bool,
    pub read_at: String,
    pub evaluation: ReadinessEvaluation,
}
