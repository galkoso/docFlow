//! Display data only. Deserializing stored anchor data never constructs the
//! source-processing or application layer's opaque verified anchor capability.
use crate::{comparison::Side, comparison_file::discussion::AnchorSide};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum DiscussionTextSelection {
    Document {
        document_id: String,
    },
    Section {
        document_id: String,
        section_id: String,
    },
    Lines,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DiscussionTextAnchor {
    pub selection: DiscussionTextSelection,
    pub source_sha256: String,
    pub range_sha256: String,
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: usize,
    pub end_line: usize,
    pub quote: String,
    pub quote_truncated: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum DiscussionAnchor {
    Proposal {},
    File {
        file_id: String,
        side: AnchorSide,
        source: Side,
        text: Box<DiscussionTextAnchor>,
    },
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DiscussionOrigin {
    pub repository_id: Uuid,
    pub branch: String,
    pub snapshot_id: Option<Uuid>,
    pub fingerprint: Option<String>,
    pub head_sha: String,
    pub target_sha: String,
    pub merge_base_sha: Option<String>,
    pub proposal_version: i64,
    pub anchor: DiscussionAnchor,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DiscussionActor {
    pub id: Uuid,
    pub name: String,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DiscussionResolution {
    pub actor: DiscussionActor,
    pub recorded_at: String,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DiscussionPermissions {
    pub can_comment: bool,
    pub can_resolve: bool,
    pub can_reopen: bool,
    pub can_toggle_blocking: bool,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DiscussionThread {
    pub id: Uuid,
    pub proposal_id: Uuid,
    pub creator: DiscussionActor,
    pub created_at: String,
    pub origin: DiscussionOrigin,
    pub version: i64,
    pub blocking: bool,
    pub resolved: bool,
    pub independent_requirement: bool,
    pub resolution: Option<DiscussionResolution>,
    pub last_activity_at: String,
    pub comment_count: i64,
    /// Compared to recorded database refs, never a live provider observation.
    pub outdated_context: bool,
    pub repository_binding_changed: bool,
    pub permissions: DiscussionPermissions,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DiscussionThreads {
    pub proposal_id: Uuid,
    pub actor_id: Uuid,
    pub proposal_version: i64,
    pub recorded_head_sha: Option<String>,
    pub recorded_target_sha: Option<String>,
    pub can_comment: bool,
    pub can_open_blocking_thread: bool,
    /// Includes outdated threads and tombstoned messages.
    pub unresolved_blockers: i64,
    pub read_at: String,
    pub threads: Vec<DiscussionThread>,
    pub next_after: Option<Uuid>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DiscussionComment {
    pub id: Uuid,
    pub author: DiscussionActor,
    pub version: i64,
    pub created_at: String,
    pub edited_at: Option<String>,
    pub tombstoned_at: Option<String>,
    pub reply_to_id: Option<Uuid>,
    pub body: Option<String>,
    pub can_edit: bool,
    pub can_tombstone: bool,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DiscussionComments {
    pub proposal_id: Uuid,
    pub actor_id: Uuid,
    pub thread: DiscussionThread,
    pub comments: Vec<DiscussionComment>,
    pub next_after: Option<Uuid>,
    pub read_at: String,
}
