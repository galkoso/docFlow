//! Explicit head/App-bound site-policy check capability. Callers must journal
//! mutations and reconcile unknown results; an adapter receipt is no merge grant.
use crate::{CommitSha, GitError};
use async_trait::async_trait;
use uuid::Uuid;

pub const MAX_CHECK_PAGES: u16 = 100;
pub const CHECK_NAME: &str = specspace_domain::review_check::CHECK_NAME;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CheckRunId(u64);
impl CheckRunId {
    pub fn new(id: u64) -> Result<Self, CheckError> {
        if id == 0 || id > i64::MAX as u64 {
            return Err(CheckError::InvalidInput);
        }
        Ok(Self(id))
    }
    pub fn get(self) -> u64 {
        self.0
    }
}
/// Server-resolved numeric repository/App identity and immutable head.
#[derive(Clone, PartialEq, Eq)]
pub struct CheckScope {
    repository_id: u64,
    app_id: u64,
    head: CommitSha,
}
impl CheckScope {
    pub fn new(repository_id: u64, app_id: u64, head: CommitSha) -> Result<Self, CheckError> {
        if repository_id == 0
            || app_id == 0
            || repository_id > i64::MAX as u64
            || app_id > i64::MAX as u64
        {
            return Err(CheckError::InvalidInput);
        }
        Ok(Self {
            repository_id,
            app_id,
            head,
        })
    }
    pub fn repository_id(&self) -> u64 {
        self.repository_id
    }
    pub fn app_id(&self) -> u64 {
        self.app_id
    }
    pub fn head(&self) -> &CommitSha {
        &self.head
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckResult {
    Pending,
    Failure,
    Success,
}
impl CheckResult {
    pub fn title(self) -> &'static str {
        match self {
            Self::Pending => "Review policy pending",
            Self::Failure => "Review policy failed",
            Self::Success => "Review policy satisfied",
        }
    }
    pub fn summary(self) -> &'static str {
        match self {
            Self::Pending => {
                "Required site-policy evidence is incomplete. This check does not authorize merge."
            }
            Self::Failure => {
                "Required site-policy conditions are not satisfied. This check does not authorize merge."
            }
            Self::Success => {
                "Site-policy evidence passed for this head. Merge requires fresh authority, refs and external required checks."
            }
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckState {
    Pending,
    Failure,
    Success,
    Other,
}
impl From<CheckResult> for CheckState {
    fn from(value: CheckResult) -> Self {
        match value {
            CheckResult::Pending => Self::Pending,
            CheckResult::Failure => Self::Failure,
            CheckResult::Success => Self::Success,
        }
    }
}
/// No serialization/deserialization: this is provider evidence, not a request.
pub struct CheckRun {
    pub id: CheckRunId,
    pub scope: CheckScope,
    pub operation: Uuid,
    pub state: CheckState,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CheckPage {
    page: u16,
    limit: u8,
}
impl CheckPage {
    pub fn new(page: u16, limit: u8) -> Result<Self, CheckError> {
        if !(1..=MAX_CHECK_PAGES).contains(&page) || !(1..=100).contains(&limit) {
            return Err(CheckError::InvalidInput);
        }
        Ok(Self { page, limit })
    }
    pub fn page(self) -> u16 {
        self.page
    }
    pub fn limit(self) -> u8 {
        self.limit
    }
}
pub struct CheckListing {
    pub items: Vec<CheckRun>,
    pub next: Option<CheckPage>,
    pub total_count: u64,
}
#[derive(Debug, PartialEq, Eq)]
pub enum CheckError {
    InvalidInput,
    InvalidResponse,
    NotFound,
    IdentityMismatch,
    CannotRestart,
    Provider(GitError),
}
pub fn external_id(operation: Uuid) -> Result<String, CheckError> {
    if operation.is_nil() {
        return Err(CheckError::InvalidInput);
    }
    Ok(format!("specspace-review-policy:{operation}"))
}
#[async_trait]
pub trait ReviewCheckProvider: Send + Sync {
    async fn check_run(&self, scope: &CheckScope, id: CheckRunId) -> Result<CheckRun, CheckError>;
    /// All matching runs, including earlier evaluations. Callers must exhaust
    /// pages, reject duplicate identities and reconcile concurrent changes before
    /// inferring absence. A provider external_id is not an idempotency key.
    async fn check_runs(
        &self,
        scope: &CheckScope,
        page: CheckPage,
    ) -> Result<CheckListing, CheckError>;
    /// Always starts pending. Never retries a POST automatically.
    async fn create_check(
        &self,
        scope: &CheckScope,
        operation: Uuid,
    ) -> Result<CheckRun, CheckError>;
    /// Preflight verifies the exact run/head/App/operation before PATCH. The
    /// caller serializes updates and fences authority; GitHub offers no CAS here.
    /// Completed runs cannot be reset to Pending; invalidate with Failure or
    /// journal a new pending run so an earlier success cannot silently survive.
    async fn update_check(
        &self,
        scope: &CheckScope,
        id: CheckRunId,
        operation: Uuid,
        result: CheckResult,
    ) -> Result<CheckRun, CheckError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scopes_pages_and_operation_ids_are_bounded() {
        for n in [0, u64::MAX] {
            assert!(CheckRunId::new(n).is_err());
        }
        let sha = CommitSha::parse("a".repeat(40)).unwrap();
        for (repo, app) in [(0, 1), (1, 0), (u64::MAX, 1), (1, u64::MAX)] {
            assert!(CheckScope::new(repo, app, sha.clone()).is_err());
        }
        for (page, limit) in [(0, 1), (101, 1), (1, 0), (1, 101)] {
            assert!(CheckPage::new(page, limit).is_err());
        }
        assert!(external_id(Uuid::nil()).is_err());
        assert_eq!(
            external_id(Uuid::from_u128(1)).unwrap(),
            "specspace-review-policy:00000000-0000-0000-0000-000000000001"
        );
        assert_eq!(CHECK_NAME, "specspace/review-policy");
    }
}
