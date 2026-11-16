use super::{SaveContext, SaveJournalError};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use specspace_application::save::{MAX_SAVE_BYTES, MAX_SAVE_FILES, PreparedSave};
use specspace_domain::RepositoryPath;
use std::collections::HashSet;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedDocument {
    pub document_id: String,
    pub path: String,
    pub sha256: String,
    pub bytes: usize,
    pub normalization_changed: bool,
    pub normalization_only: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveManifest {
    pub version: u32,
    pub request_hash: String,
    pub documents: Vec<SavedDocument>,
}
impl SaveManifest {
    pub(super) fn from_prepared(plan: &PreparedSave) -> Self {
        Self {
            version: 1,
            request_hash: plan.fingerprint().into(),
            documents: plan
                .documents()
                .iter()
                .map(|document| SavedDocument {
                    document_id: document.document_id.clone(),
                    path: document.path.as_str().into(),
                    sha256: document.sha256.clone(),
                    bytes: document.markdown.len(),
                    normalization_changed: document.normalization_changed,
                    normalization_only: document.normalization_only,
                })
                .collect(),
        }
    }
    pub fn is_noop(&self) -> bool {
        self.documents.is_empty()
    }
    pub(crate) fn validate(&self, request_hash: &str) -> Result<(), SaveJournalError> {
        if self.version != 1
            || !digest(request_hash)
            || self.request_hash != request_hash
            || self.documents.len() > MAX_SAVE_FILES
        {
            return Err(SaveJournalError::InvalidPlan);
        }
        let mut paths = HashSet::new();
        let mut ids = HashSet::new();
        let mut total = 0usize;
        for document in &self.documents {
            if !specspace_application::draft::valid_document_id(&document.document_id)
                || !ids.insert(&document.document_id)
                || RepositoryPath::parse(&document.path).is_err()
                || !document.path.starts_with("docs/")
                || !document.path.ends_with(".md")
                || !paths.insert(document.path.to_lowercase())
                || !digest(&document.sha256)
                || document.bytes > 2 * 1024 * 1024
            {
                return Err(SaveJournalError::InvalidPlan);
            }
            total = total.saturating_add(document.bytes);
        }
        if total > MAX_SAVE_BYTES {
            return Err(SaveJournalError::InvalidPlan);
        }
        Ok(())
    }
    pub(crate) fn hash(
        &self,
        context: &SaveContext,
        operation: uuid::Uuid,
    ) -> Result<String, SaveJournalError> {
        let bytes = serde_json::to_vec(&("specspace-save-manifest-v1", operation, context, self))
            .map_err(|_| SaveJournalError::InvalidPlan)?;
        Ok(format!("{:x}", Sha256::digest(bytes)))
    }
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
