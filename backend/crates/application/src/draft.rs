//! Recovery buffers are uncommitted user input, including temporarily invalid
//! Markdown. Validation here bounds storage; it never declares content ready for
//! review or normalizes source bytes before a user chooses a Git save.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use specspace_git_adapter::CommitSha;
use uuid::Uuid;

pub const MAX_DRAFT_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_RECOVERY_BYTES: i64 = 20 * 1024 * 1024;
pub const MAX_RECOVERY_DOCUMENTS: i64 = 1000;

pub fn valid_document_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 120
        && value
            .bytes()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'-')
}

#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DraftContext {
    pub proposal_id: Uuid,
    pub current_head_sha: Option<String>,
    pub proposal_version: i64,
    pub proposal_state: String,
    pub can_edit: bool,
    pub can_store_recovery: bool,
    pub retention_days: Option<u16>,
}
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DraftBuffer {
    pub document_id: String,
    pub base_head_sha: String,
    pub version: i64,
    pub updated_at: String,
    pub expires_at: String,
    pub content_utf8: Option<String>,
    pub base_is_current: bool,
}
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DraftSnapshot {
    pub context: DraftContext,
    pub buffers: Vec<DraftBuffer>,
}
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DraftSaved {
    pub context: DraftContext,
    pub buffer: DraftBuffer,
    pub replayed: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct DraftRetention(u16);
impl DraftRetention {
    pub fn new(days: u16) -> Result<Self, DraftError> {
        if !(1..=365).contains(&days) {
            return Err(DraftError::Invalid("INVALID_RETENTION"));
        }
        Ok(Self(days))
    }
    pub fn days(self) -> u16 {
        self.0
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum DraftError {
    NotFound,
    Forbidden,
    Invalid(&'static str),
    VersionConflict { current_version: Option<i64> },
    SaveIdentityMismatch,
    QuotaExceeded,
    PolicyNotConfigured,
    Unavailable,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SaveDraft {
    pub save_id: Uuid,
    pub document_id: String,
    pub base_head_sha: String,
    /// Zero creates a buffer; existing live buffers require their returned version.
    pub expected_draft_version: i64,
    pub content_utf8: String,
}

pub struct ValidatedDraft {
    request: SaveDraft,
    fingerprint: String,
}
impl ValidatedDraft {
    pub fn new(mut request: SaveDraft) -> Result<Self, DraftError> {
        if request.save_id.is_nil() {
            return Err(DraftError::Invalid("INVALID_SAVE_ID"));
        }
        if !valid_document_id(&request.document_id) {
            return Err(DraftError::Invalid("INVALID_DOCUMENT_ID"));
        }
        if request.expected_draft_version < 0 {
            return Err(DraftError::Invalid("INVALID_DRAFT_VERSION"));
        }
        request.base_head_sha = CommitSha::parse(&request.base_head_sha)
            .map_err(|_| DraftError::Invalid("INVALID_BASE_SHA"))?
            .as_str()
            .into();
        if request.content_utf8.len() > MAX_DRAFT_BYTES {
            return Err(DraftError::Invalid("FILE_TOO_LARGE"));
        }
        if request.content_utf8.contains('\0') {
            return Err(DraftError::Invalid("INVALID_CONTENT"));
        }
        let fingerprint = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&request).map_err(|_| DraftError::Unavailable)?)
        );
        Ok(Self {
            request,
            fingerprint,
        })
    }
    pub fn request(&self) -> &SaveDraft {
        &self.request
    }
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> SaveDraft {
        SaveDraft {
            save_id: Uuid::new_v4(),
            document_id: "DOC-MEDIA".into(),
            base_head_sha: "A".repeat(40),
            expected_draft_version: 0,
            content_utf8: "---\r\ninvalid unfinished ** שָׁלוֹם\r\n".into(),
        }
    }
    #[test]
    fn recovery_preserves_incomplete_source_and_binds_the_whole_save_intent() {
        let original = request();
        let saved = ValidatedDraft::new(original.clone()).unwrap();
        assert_eq!(saved.request().content_utf8, original.content_utf8);
        assert_eq!(saved.request().base_head_sha, "a".repeat(40));
        assert_eq!(
            saved.fingerprint(),
            ValidatedDraft::new(original.clone()).unwrap().fingerprint()
        );
        for mutation in [0, 1, 2, 3] {
            let mut changed = original.clone();
            match mutation {
                0 => changed.content_utf8.push('x'),
                1 => changed.expected_draft_version = 1,
                2 => changed.base_head_sha = "b".repeat(40),
                _ => changed.document_id = "DOC-OTHER".into(),
            }
            assert_ne!(
                saved.fingerprint(),
                ValidatedDraft::new(changed).unwrap().fingerprint()
            );
        }
    }
    #[test]
    fn recovery_rejects_unbounded_or_unstorable_input() {
        for mutation in [0, 1, 2, 3, 4, 5] {
            let mut bad = request();
            match mutation {
                0 => bad.content_utf8 = "x".repeat(MAX_DRAFT_BYTES + 1),
                1 => bad.content_utf8.push('\0'),
                2 => bad.document_id = "../secret".into(),
                3 => bad.base_head_sha = "main".into(),
                4 => bad.expected_draft_version = -1,
                _ => bad.save_id = Uuid::nil(),
            }
            assert!(ValidatedDraft::new(bad).is_err());
        }
        assert!(DraftRetention::new(0).is_err());
        assert!(DraftRetention::new(366).is_err());
        assert_eq!(DraftRetention::new(30).unwrap().days(), 30);
    }
}
