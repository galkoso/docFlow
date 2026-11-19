//! Browser intent only. Paths, refs, source digests, quotes, roles and trusted
//! anchor evidence cannot be supplied through these request types.
use crate::comparison_file::discussion::{AnchorSide, TextSelection};
use serde::{Deserialize, Serialize};
use specspace_domain::discussion::CommentBody;
use uuid::Uuid;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OpenThreadRequest {
    pub request_id: Uuid,
    pub origin: ThreadOrigin,
    pub blocking: bool,
    pub body: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ThreadOrigin {
    Proposal {
        expected_proposal_version: i64,
    },
    Snapshot {
        snapshot_id: Uuid,
        expected_fingerprint: String,
        anchor: SnapshotAnchor,
    },
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SnapshotAnchor {
    Proposal {},
    File {
        file_id: String,
        side: AnchorSide,
        selection: TextSelection,
    },
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ThreadMutationRequest {
    pub request_id: Uuid,
    pub expected_thread_version: i64,
    pub mutation: ThreadMutation,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ThreadMutation {
    Reply {
        body: String,
        reply_to_id: Option<Uuid>,
    },
    Edit {
        comment_id: Uuid,
        expected_comment_version: i64,
        body: String,
    },
    Tombstone {
        comment_id: Uuid,
        expected_comment_version: i64,
    },
    Resolve {},
    Reopen {},
    SetBlocking {
        blocking: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscussionRecorded {
    pub id: Uuid,
    pub request_id: Uuid,
    pub actor_id: Uuid,
    pub proposal_id: Uuid,
    pub thread_id: Uuid,
    pub thread_version: i64,
    pub comment_id: Option<Uuid>,
    pub comment_version: Option<i64>,
    pub recorded_at: String,
    pub replayed: bool,
}

fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl OpenThreadRequest {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.request_id.is_nil() || CommentBody::new(&self.body).is_err() {
            return Err("INVALID_DISCUSSION");
        }
        match &self.origin {
            ThreadOrigin::Proposal {
                expected_proposal_version,
            } if *expected_proposal_version < 1 => return Err("INVALID_DISCUSSION"),
            ThreadOrigin::Snapshot {
                snapshot_id,
                expected_fingerprint,
                anchor,
            } => {
                if snapshot_id.is_nil() || !digest(expected_fingerprint) {
                    return Err("INVALID_DISCUSSION");
                }
                if let SnapshotAnchor::File {
                    file_id, selection, ..
                } = anchor
                    && (!digest(file_id) || selection.validate().is_err())
                {
                    return Err("INVALID_DISCUSSION");
                }
            }
            _ => {}
        }
        Ok(())
    }
}

impl ThreadMutationRequest {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.request_id.is_nil()
            || self.expected_thread_version < 1
            || self.expected_thread_version == i64::MAX
        {
            return Err("INVALID_DISCUSSION");
        }
        match &self.mutation {
            ThreadMutation::Reply { body, reply_to_id } => {
                if CommentBody::new(body).is_err() || reply_to_id.is_some_and(|id| id.is_nil()) {
                    return Err("INVALID_DISCUSSION");
                }
            }
            ThreadMutation::Edit {
                comment_id,
                expected_comment_version,
                body,
            } => {
                if CommentBody::new(body).is_err()
                    || !valid_comment(*comment_id, *expected_comment_version)
                {
                    return Err("INVALID_DISCUSSION");
                }
            }
            ThreadMutation::Tombstone {
                comment_id,
                expected_comment_version,
            } if !valid_comment(*comment_id, *expected_comment_version) => {
                return Err("INVALID_DISCUSSION");
            }
            _ => {}
        }
        Ok(())
    }
}

fn valid_comment(id: Uuid, version: i64) -> bool {
    !id.is_nil() && version > 0 && version < i64::MAX
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_shapes_reject_browser_authority_and_source_evidence() {
        let valid = json!({"request_id":Uuid::new_v4(),"origin":{"kind":"proposal","expected_proposal_version":1},"blocking":false,"body":"שלום 🦀"});
        assert!(
            serde_json::from_value::<OpenThreadRequest>(valid.clone())
                .unwrap()
                .validate()
                .is_ok()
        );
        for field in ["actor_id", "role", "head_sha", "path", "quote"] {
            let mut forged = valid.clone();
            forged[field] = json!("forged");
            assert!(serde_json::from_value::<OpenThreadRequest>(forged).is_err());
        }
        let forged_snapshot = json!({"request_id":Uuid::new_v4(),"origin":{"kind":"snapshot","snapshot_id":Uuid::new_v4(),"expected_fingerprint":"a".repeat(64),"anchor":{"kind":"proposal","source_sha256":"b".repeat(64)}},"blocking":false,"body":"test"});
        assert!(serde_json::from_value::<OpenThreadRequest>(forged_snapshot).is_err());
        let forged_resolution = json!({"request_id":Uuid::new_v4(),"expected_thread_version":1,"mutation":{"kind":"resolve","independent_requirement":false}});
        assert!(serde_json::from_value::<ThreadMutationRequest>(forged_resolution).is_err());
    }
}
