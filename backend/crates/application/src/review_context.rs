//! Fresh server-resolved scope for review submission and publication rechecks.
//! Expectations are compare-and-swap values, never authority for membership or
//! repository selection. This module performs no publication or provider write.
use crate::{
    comparison_reader::{ComparisonReadError, ReadContext},
    review::ReviewContext,
};
use async_trait::async_trait;
use specspace_git_adapter::CommitSha;
use uuid::Uuid;

#[derive(Clone, PartialEq, Eq)]
pub struct ReviewExpectation {
    pub head: CommitSha,
    /// Must additionally match the provider's current main during preparation.
    pub target: CommitSha,
    pub proposal_version: i64,
    pub configuration_version: u64,
    pub policy_version: u64,
}
#[derive(Clone, PartialEq, Eq)]
pub struct ReviewScope {
    pub comparison: ReadContext,
    pub review: ReviewContext,
}
#[derive(Debug, PartialEq, Eq)]
pub enum ReviewContextError {
    Read(ComparisonReadError),
    Forbidden,
    StateChanged,
    Stale,
    Busy,
    InvalidConfiguration,
    OwnerNotEligible,
    Unavailable,
}
#[async_trait]
pub trait ReviewAuthorization: Send + Sync {
    async fn resolve(
        &self,
        actor: Uuid,
        proposal: Uuid,
        expectation: &ReviewExpectation,
    ) -> Result<ReviewScope, ReviewContextError>;
}
