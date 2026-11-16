//! Git object identity includes its type/length header, unlike a content digest.
use crate::{CommitSha, GitError};
use sha2::{Digest, Sha256};
pub fn oid(bytes: &[u8], oid_length: usize) -> Result<CommitSha, GitError> {
    let prefix = format!("blob {}\0", bytes.len());
    let hash = match oid_length {
        40 => {
            let mut hash = sha1::Sha1::new();
            hash.update(prefix.as_bytes());
            hash.update(bytes);
            format!("{:x}", hash.finalize())
        }
        64 => {
            let mut hash = Sha256::new();
            hash.update(prefix.as_bytes());
            hash.update(bytes);
            format!("{:x}", hash.finalize())
        }
        _ => return Err(GitError::InvalidResponse),
    };
    CommitSha::parse(hash).map_err(|_| GitError::InvalidResponse)
}
