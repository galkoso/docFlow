//! Server-owned Git provider configuration. No client-supplied provider URLs.
pub mod app_auth;
pub mod github;
pub mod receipt;
pub use receipt::CommitReceipt;

use async_trait::async_trait;
use specspace_domain::{BranchName, RepositoryPath};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CommitSha(String);

impl CommitSha {
    pub fn parse(value: impl Into<String>) -> Result<Self, GitError> {
        let value = value.into();
        if !(40..=128).contains(&value.len())
            || value.len() % 2 != 0
            || !value.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(GitError::InvalidInput);
        }
        Ok(Self(value.to_ascii_lowercase()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitError {
    InvalidInput,
    BranchMissing,
    FileMissing,
    BranchExists,
    HeadMismatch,
    Unauthorized,
    RateLimited {
        retry_after_seconds: Option<u64>,
    },
    ProviderUnavailable,
    ProviderRejected,
    ResponseTooLarge,
    InvalidResponse,
    /// A mutation may have succeeded. Reconcile the operation before retrying.
    ResultUnknown,
    /// The write may have succeeded; wait before observing/reconciling it.
    ResultUnknownWithRetryAfter {
        retry_after_seconds: u64,
    },
}

impl GitError {
    pub(crate) fn uncertain_readback(self) -> Self {
        match self {
            Self::RateLimited {
                retry_after_seconds: Some(seconds),
            }
            | Self::ResultUnknownWithRetryAfter {
                retry_after_seconds: seconds,
            } => Self::ResultUnknownWithRetryAfter {
                retry_after_seconds: seconds,
            },
            _ => Self::ResultUnknown,
        }
    }
}

impl std::fmt::Display for GitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for GitError {}

pub enum FileChange {
    Put {
        path: RepositoryPath,
        content: Vec<u8>,
    },
    Delete {
        path: RepositoryPath,
    },
}

pub struct FileEntry {
    pub path: RepositoryPath,
    pub blob_sha: CommitSha,
    pub size: u64,
}

pub struct CommitMetadata {
    pub sha: CommitSha,
    pub committed_at: chrono::DateTime<chrono::Utc>,
}

pub struct CommitRequest<'a> {
    pub operation_id: Uuid,
    pub branch: &'a BranchName,
    pub expected_head: &'a CommitSha,
    pub message: &'a str,
    pub actor_id: Uuid,
    pub changes: &'a [FileChange],
}

/// Read/write foundation. PR, merge and checks are added as explicit capabilities
/// when implemented; no default methods pretend those operations succeeded.
#[async_trait]
pub trait GitProvider: Send + Sync {
    async fn main_head(&self) -> Result<CommitSha, GitError>;
    async fn branch_head(&self, branch: &BranchName) -> Result<CommitSha, GitError>;
    async fn create_branch(
        &self,
        branch: &BranchName,
        source: &CommitSha,
    ) -> Result<CommitSha, GitError>;
    async fn commit(&self, request: CommitRequest<'_>) -> Result<CommitSha, GitError>;
    async fn read_text(
        &self,
        commit: &CommitSha,
        path: &RepositoryPath,
    ) -> Result<String, GitError>;
    /// Raw commit-pinned blob, bounded before fetching/decoding (maximum 5MiB).
    async fn read_bytes(
        &self,
        commit: &CommitSha,
        path: &RepositoryPath,
        max_bytes: usize,
    ) -> Result<Vec<u8>, GitError>;
    async fn list_files(&self, commit: &CommitSha) -> Result<Vec<FileEntry>, GitError>;
    async fn commit_metadata(&self, commit: &CommitSha) -> Result<CommitMetadata, GitError>;
    /// Immutable commit evidence for reconciliation. Matching trailers alone
    /// never prove intended file contents, branch reachability or authorization.
    async fn commit_receipt(&self, commit: &CommitSha) -> Result<CommitReceipt, GitError>;
    async fn is_ancestor(
        &self,
        ancestor: &CommitSha,
        descendant: &CommitSha,
    ) -> Result<bool, GitError>;
}
