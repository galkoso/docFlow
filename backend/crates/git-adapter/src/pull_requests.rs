//! Explicit PR capability. A returned observation is not a review approval or a
//! merge receipt. Callers journal writes and reconcile uncertainty before retry.
use crate::{CommitSha, GitError};
use async_trait::async_trait;
use specspace_domain::BranchName;

pub const MAX_PR_BODY_BYTES: usize = 64 * 1024;
pub const MAX_PR_PAGES: u16 = 100;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PullRequestNumber(u64);
impl PullRequestNumber {
    pub fn new(number: u64) -> Result<Self, PullRequestError> {
        if number == 0 || number > i64::MAX as u64 {
            return Err(PullRequestError::InvalidInput);
        }
        Ok(Self(number))
    }
    pub fn get(self) -> u64 {
        self.0
    }
}
#[derive(Clone, PartialEq, Eq)]
pub struct PullRequestScope {
    repository_id: u64,
    branch: BranchName,
}
impl PullRequestScope {
    pub fn new(repository_id: u64, branch: BranchName) -> Result<Self, PullRequestError> {
        if repository_id == 0 {
            return Err(PullRequestError::InvalidInput);
        }
        Ok(Self {
            repository_id,
            branch,
        })
    }
    pub fn repository_id(&self) -> u64 {
        self.repository_id
    }
    pub fn branch(&self) -> &BranchName {
        &self.branch
    }
}
/// Exact bounded metadata; no client URL, base override, or PR state mutation.
#[derive(Clone, PartialEq, Eq)]
pub struct PullRequestContent {
    title: String,
    body: String,
}
impl PullRequestContent {
    pub fn new(title: String, body: String) -> Result<Self, PullRequestError> {
        if !(3..=120).contains(&title.chars().count())
            || title.trim().is_empty()
            || title.chars().any(char::is_control)
            || body.len() > MAX_PR_BODY_BYTES
            || body.contains('\0')
        {
            return Err(PullRequestError::InvalidInput);
        }
        Ok(Self { title, body })
    }
    pub fn title(&self) -> &str {
        &self.title
    }
    pub fn body(&self) -> &str {
        &self.body
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PullRequestState {
    Open,
    Closed,
    Merged,
}
/// Provider observation. Deliberately no Debug/Serialize/Deserialize: bodies may
/// contain private proposal context and do not become browser-owned authority.
pub struct PullRequest {
    pub id: u64,
    pub node_id: String,
    pub number: PullRequestNumber,
    pub repository_id: u64,
    pub branch: BranchName,
    pub head: CommitSha,
    pub target: CommitSha,
    pub state: PullRequestState,
    pub draft: bool,
    pub title: String,
    pub body: String,
    pub url: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PullRequestPage {
    page: u16,
    limit: u8,
}
impl PullRequestPage {
    pub fn new(page: u16, limit: u8) -> Result<Self, PullRequestError> {
        if !(1..=MAX_PR_PAGES).contains(&page) || !(1..=100).contains(&limit) {
            return Err(PullRequestError::InvalidInput);
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
pub struct PullRequestListing {
    pub items: Vec<PullRequest>,
    pub next: Option<PullRequestPage>,
}
#[derive(Debug, PartialEq, Eq)]
pub enum PullRequestError {
    InvalidInput,
    NotFound,
    NotOpen,
    InvalidResponse,
    Provider(GitError),
}
#[async_trait]
pub trait PullRequestProvider: Send + Sync {
    async fn pull_request(
        &self,
        scope: &PullRequestScope,
        number: PullRequestNumber,
    ) -> Result<PullRequest, PullRequestError>;
    /// All states. Exhaustion must be proven before callers infer absence.
    async fn pull_requests(
        &self,
        scope: &PullRequestScope,
        page: PullRequestPage,
    ) -> Result<PullRequestListing, PullRequestError>;
    /// Creates a draft. Readiness and merge require separate explicit capabilities.
    async fn create_pull_request(
        &self,
        scope: &PullRequestScope,
        content: &PullRequestContent,
    ) -> Result<PullRequest, PullRequestError>;
    /// Updates title/body only; never retargets, reopens or merges a PR.
    async fn update_pull_request(
        &self,
        scope: &PullRequestScope,
        number: PullRequestNumber,
        content: &PullRequestContent,
    ) -> Result<PullRequest, PullRequestError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identifiers_page_bounds_and_metadata_limits_are_explicit() {
        assert!(PullRequestNumber::new(0).is_err());
        assert!(PullRequestNumber::new(u64::MAX).is_err());
        assert_eq!(PullRequestNumber::new(42).unwrap().get(), 42);
        for (page, size) in [(0, 1), (101, 1), (1, 0), (1, 101)] {
            assert!(PullRequestPage::new(page, size).is_err());
        }
        assert!(PullRequestContent::new("abc\n".into(), "body".into()).is_err());
        assert!(PullRequestContent::new("אבג".repeat(41), "body".into()).is_err());
        assert!(
            PullRequestContent::new("Synthetic".into(), "x".repeat(MAX_PR_BODY_BYTES + 1)).is_err()
        );
        let text =
            PullRequestContent::new("כותרת".into(), "body\n<!-- identity -->".into()).unwrap();
        assert_eq!(text.title(), "כותרת");
        assert_eq!(text.body(), "body\n<!-- identity -->");
    }
}
