//! Reconciliation proves the planned snapshot, not merely matching trailers.
use crate::saves::{SaveJournalError, SaveLease, SaveManifest};
use sha2::{Digest, Sha256};
use specspace_git_adapter::{CommitSha, FileEntry, FileMode, GitError, GitProvider};
use std::collections::HashMap;
use uuid::Uuid;

#[derive(Debug)]
pub enum VerificationError {
    Provider(GitError),
    InvalidPlan,
    UnexpectedCommit,
    UnexpectedTree,
    ContentMismatch,
    NotReachable,
}
impl From<GitError> for VerificationError {
    fn from(error: GitError) -> Self {
        Self::Provider(error)
    }
}
impl From<SaveJournalError> for VerificationError {
    fn from(_: SaveJournalError) -> Self {
        Self::InvalidPlan
    }
}

pub struct VerifiedSave {
    operation: Uuid,
    manifest_hash: String,
    commit: CommitSha,
    committed_at: String,
}
impl VerifiedSave {
    pub fn operation(&self) -> Uuid {
        self.operation
    }
    pub fn manifest_hash(&self) -> &str {
        &self.manifest_hash
    }
    pub fn commit(&self) -> &CommitSha {
        &self.commit
    }
    pub fn committed_at(&self) -> &str {
        &self.committed_at
    }
}

pub async fn verify(
    provider: &dyn GitProvider,
    lease: &SaveLease,
    manifest: &SaveManifest,
    commit: &CommitSha,
) -> Result<VerifiedSave, VerificationError> {
    manifest.validate(lease.fingerprint())?;
    if manifest.is_noop() {
        return Err(VerificationError::InvalidPlan);
    }
    let context = lease.context();
    let head = context.head()?;
    let branch = context.branch()?;
    let receipt = provider.commit_receipt(commit).await?;
    if receipt.sha != *commit
        || !receipt.matches_operation(lease.operation(), context.actor(), &head)
    {
        return Err(VerificationError::UnexpectedCommit);
    }
    let base = tree(provider.list_files(&head).await?)?;
    let actual = tree(provider.list_files(commit).await?)?;
    if base.len() != actual.len() || base.keys().any(|path| !actual.contains_key(path)) {
        return Err(VerificationError::UnexpectedTree);
    }
    let planned: HashMap<_, _> = manifest
        .documents
        .iter()
        .map(|document| (document.path.as_str(), document))
        .collect();
    if planned.keys().any(|path| !base.contains_key(*path)) {
        return Err(VerificationError::UnexpectedTree);
    }
    for (path, old) in &base {
        let new = &actual[path];
        if let Some(expected) = planned.get(path.as_str()) {
            if old.mode != FileMode::Regular
                || new.mode != old.mode
                || new.size != expected.bytes as u64
            {
                return Err(VerificationError::UnexpectedTree);
            }
            let text = provider.read_text(commit, &new.path).await?;
            if text.len() != expected.bytes
                || format!("{:x}", Sha256::digest(text.as_bytes())) != expected.sha256
                || git_blob_hash(text.as_bytes(), new.blob_sha.as_str().len())?.as_str()
                    != new.blob_sha.as_str()
            {
                return Err(VerificationError::ContentMismatch);
            }
        } else if old.blob_sha != new.blob_sha || old.size != new.size || old.mode != new.mode {
            return Err(VerificationError::UnexpectedTree);
        }
    }
    let observed = provider.branch_head(&branch).await?;
    if observed != *commit && !provider.is_ancestor(commit, &observed).await? {
        return Err(VerificationError::NotReachable);
    }
    Ok(VerifiedSave {
        operation: lease.operation(),
        manifest_hash: manifest.hash(context, lease.operation())?,
        commit: commit.clone(),
        committed_at: receipt.committed_at.to_rfc3339(),
    })
}
fn tree(files: Vec<FileEntry>) -> Result<HashMap<String, FileEntry>, VerificationError> {
    if files.len() > 20000 {
        return Err(VerificationError::UnexpectedTree);
    }
    let mut result = HashMap::new();
    let mut keys = std::collections::HashSet::new();
    for file in files {
        if !keys.insert(file.path.as_str().to_lowercase())
            || result.insert(file.path.as_str().into(), file).is_some()
        {
            return Err(VerificationError::UnexpectedTree);
        }
    }
    Ok(result)
}
pub(crate) fn git_blob_hash(
    bytes: &[u8],
    oid_length: usize,
) -> Result<CommitSha, VerificationError> {
    specspace_git_adapter::blob::oid(bytes, oid_length)
        .map_err(|_| VerificationError::UnexpectedTree)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn blob_identity_matches_git_object_framing_instead_of_plain_file_hashes() {
        assert_eq!(
            git_blob_hash(b"", 40).unwrap().as_str(),
            "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"
        );
        assert_eq!(
            git_blob_hash(b"hello\n", 40).unwrap().as_str(),
            "ce013625030ba8dba906f756967f9e9ca394464a"
        );
        assert!(git_blob_hash(b"hello\n", 128).is_err());
        assert_ne!(
            git_blob_hash(b"hello\n", 64).unwrap().as_str(),
            format!("{:x}", Sha256::digest(b"hello\n"))
        );
    }
}
