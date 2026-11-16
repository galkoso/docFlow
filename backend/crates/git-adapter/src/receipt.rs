//! Commit identity is evidence, not permission or content verification.
use crate::CommitSha;
use chrono::{DateTime, Utc};
use uuid::Uuid;

pub const MAX_COMMIT_MESSAGE_BYTES: usize = 64 * 1024;

pub struct CommitReceipt {
    pub sha: CommitSha,
    pub tree: CommitSha,
    pub parents: Vec<CommitSha>,
    pub committed_at: DateTime<Utc>,
    pub message: String,
}

/// Reserved fields are written by the adapter, never taken from user text.
pub fn has_reserved_trailer(message: &str) -> bool {
    message.lines().any(|line| {
        let line = line.trim_start().to_ascii_lowercase();
        line.starts_with("specspace-operation:") || line.starts_with("specspace-actor:")
    })
}

impl CommitReceipt {
    /// A SpecSpace save has one exact parent and one final authoritative trailer
    /// block. Extra, forged or ambiguous fields anywhere else are rejected.
    pub fn matches_operation(&self, operation: Uuid, actor: Uuid, parent: &CommitSha) -> bool {
        if operation.is_nil()
            || actor.is_nil()
            || self.parents.len() != 1
            || self.parents.first() != Some(parent)
            || self.sha == *parent
            || self.message.len() > MAX_COMMIT_MESSAGE_BYTES
            || self.message.contains('\0')
        {
            return false;
        }
        let normalized = self.message.replace("\r\n", "\n");
        let Some((body, trailers)) = normalized.trim_end_matches('\n').rsplit_once("\n\n") else {
            return false;
        };
        if body.trim().is_empty() || has_reserved_trailer(body) {
            return false;
        }
        let mut lines = trailers.lines();
        let operation_id = lines
            .next()
            .and_then(|line| line.strip_prefix("SpecSpace-Operation: "))
            .and_then(|id| Uuid::parse_str(id).ok());
        let actor_id = lines
            .next()
            .and_then(|line| line.strip_prefix("SpecSpace-Actor: "))
            .and_then(|id| Uuid::parse_str(id).ok());
        operation_id == Some(operation) && actor_id == Some(actor) && lines.next().is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_identity_requires_one_parent_and_unambiguous_final_trailers() {
        let parent = CommitSha::parse("1".repeat(40)).unwrap();
        let operation = Uuid::new_v4();
        let actor = Uuid::new_v4();
        let trailer = format!("SpecSpace-Operation: {operation}\nSpecSpace-Actor: {actor}");
        let message = format!("Save document\n\nHuman explanation\n\n{trailer}");
        let mut receipt = CommitReceipt {
            sha: CommitSha::parse("2".repeat(40)).unwrap(),
            tree: CommitSha::parse("3".repeat(40)).unwrap(),
            parents: vec![parent.clone()],
            committed_at: "2026-11-16T00:00:00Z".parse().unwrap(),
            message: message.clone(),
        };
        assert!(receipt.matches_operation(operation, actor, &parent));
        receipt.message = format!("{}\r\n", message.replace('\n', "\r\n"));
        assert!(receipt.matches_operation(operation, actor, &parent));
        assert!(!receipt.matches_operation(Uuid::new_v4(), actor, &parent));
        assert!(!receipt.matches_operation(operation, Uuid::new_v4(), &parent));
        for bad in [
            format!("Save\n\n{trailer}\nextra"),
            format!("Save\n\n{trailer}\nSpecSpace-Actor: {actor}"),
            format!("Save\n\n{trailer}\n\nfooter"),
            format!("Save\nSpecSpace-Actor: {actor}\n\n{trailer}"),
            format!("Save\n\tsPeCsPaCe-OpErAtIoN: {operation}\n\n{trailer}"),
            format!("Save\0\n\n{trailer}"),
            format!("{}\n\n{trailer}", "x".repeat(MAX_COMMIT_MESSAGE_BYTES)),
            format!("\n\n{trailer}"),
        ] {
            receipt.message = bad;
            assert!(!receipt.matches_operation(operation, actor, &parent));
        }
        receipt.message = message;
        receipt
            .parents
            .push(CommitSha::parse("4".repeat(40)).unwrap());
        assert!(!receipt.matches_operation(operation, actor, &parent));
        receipt.parents.clear();
        assert!(!receipt.matches_operation(operation, actor, &parent));
        receipt.parents.push(parent.clone());
        receipt.sha = parent.clone();
        assert!(!receipt.matches_operation(operation, actor, &parent));
    }
}
