//! Immutable intent and whole-book plan for an existing-document Git draft save.
//! A plan grants no authority and performs no I/O. The journal/worker must bind
//! current permissions, proposal version, pinned snapshot provenance, image-byte
//! verification and expected-head mutation before any Git write.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use specspace_document_engine::{
    catalog::Catalog,
    save_validation::{self, SaveValidation},
};
use specspace_domain::RepositoryPath;
use specspace_git_adapter::{CommitSha, FileChange, receipt::has_reserved_trailer};
use std::{
    collections::{HashMap, HashSet},
    io::{self, BufWriter, Write},
};
use uuid::Uuid;

pub const MAX_SAVE_BYTES: usize = 10 * 1024 * 1024;
pub const MAX_SAVE_FILES: usize = 50;
const MAX_FILE_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DocumentSave {
    pub document_id: String,
    pub expected_document_sha256: String,
    pub markdown: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SaveRequest {
    pub expected_head_sha: String,
    pub expected_proposal_version: i64,
    pub message: String,
    pub documents: Vec<DocumentSave>,
}

#[derive(Debug)]
pub enum SaveError {
    Invalid(&'static str),
    SnapshotMismatch,
    DocumentChanged(String),
    Validation(SaveValidation),
}
pub struct SourceFile {
    pub path: String,
    pub markdown: String,
}
pub struct SaveSnapshot {
    pub repository_id: Uuid,
    pub commit: CommitSha,
    pub catalog: Catalog,
    /// All docs/*.md in the pinned Git tree, including uncatalogued files.
    pub files: Vec<SourceFile>,
    /// Exact asset paths from that tree. Existence does not verify media bytes.
    pub asset_paths: HashSet<String>,
}
pub struct PreparedDocument {
    pub document_id: String,
    pub path: RepositoryPath,
    pub markdown: String,
    pub sha256: String,
    pub normalization_changed: bool,
    pub normalization_only: bool,
    pub removed_section_ids: Option<Vec<String>>,
}
pub struct PreparedSave {
    actor: Uuid,
    proposal: Uuid,
    repository: Uuid,
    fingerprint: String,
    expected_head: CommitSha,
    expected_proposal_version: i64,
    documents: Vec<PreparedDocument>,
    /// Required assets must undergo pinned hash/MIME/pixel verification by the
    /// caller. Warnings here do not satisfy stricter review/merge policy.
    validation: SaveValidation,
}
impl PreparedSave {
    pub fn actor(&self) -> Uuid {
        self.actor
    }
    pub fn proposal(&self) -> Uuid {
        self.proposal
    }
    pub fn repository(&self) -> Uuid {
        self.repository
    }
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
    pub fn expected_head(&self) -> &CommitSha {
        &self.expected_head
    }
    pub fn expected_proposal_version(&self) -> i64 {
        self.expected_proposal_version
    }
    pub fn documents(&self) -> &[PreparedDocument] {
        &self.documents
    }
    pub fn validation(&self) -> &SaveValidation {
        &self.validation
    }
    pub fn is_noop(&self) -> bool {
        self.documents.is_empty()
    }
    pub fn file_changes(&self) -> Vec<FileChange> {
        self.documents
            .iter()
            .map(|document| FileChange::Put {
                path: document.path.clone(),
                content: document.markdown.as_bytes().to_vec(),
            })
            .collect()
    }
}

pub struct ValidatedSave {
    actor: Uuid,
    proposal: Uuid,
    repository: Uuid,
    head: CommitSha,
    request: SaveRequest,
    fingerprint: String,
}
impl ValidatedSave {
    pub fn new(
        actor: Uuid,
        proposal: Uuid,
        repository: Uuid,
        mut request: SaveRequest,
    ) -> Result<Self, SaveError> {
        if actor.is_nil() || proposal.is_nil() || repository.is_nil() {
            return Err(SaveError::Invalid("INVALID_SAVE_SCOPE"));
        }
        let head = CommitSha::parse(&request.expected_head_sha)
            .map_err(|_| SaveError::Invalid("INVALID_HEAD_SHA"))?;
        request.expected_head_sha = head.as_str().into();
        if request.expected_proposal_version <= 0 {
            return Err(SaveError::Invalid("INVALID_PROPOSAL_VERSION"));
        }
        if request.message.trim().is_empty()
            || request.message.chars().count() > 10000
            || request.message.contains('\0')
            || has_reserved_trailer(&request.message)
        {
            return Err(SaveError::Invalid("INVALID_COMMIT_MESSAGE"));
        }
        if request.documents.is_empty() || request.documents.len() > MAX_SAVE_FILES {
            return Err(SaveError::Invalid("SAVE_FILE_LIMIT"));
        }
        let mut ids = HashSet::new();
        let mut total = 0usize;
        for document in &mut request.documents {
            if !crate::draft::valid_document_id(&document.document_id)
                || !ids.insert(document.document_id.clone())
            {
                return Err(SaveError::Invalid("INVALID_DOCUMENT_ID"));
            }
            if document.expected_document_sha256.len() != 64
                || !document
                    .expected_document_sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit())
            {
                return Err(SaveError::Invalid("INVALID_DOCUMENT_SHA256"));
            }
            document.expected_document_sha256.make_ascii_lowercase();
            if document.markdown.len() > MAX_FILE_BYTES {
                return Err(SaveError::Invalid("FILE_TOO_LARGE"));
            }
            if document.markdown.contains('\0') {
                return Err(SaveError::Invalid("INVALID_CONTENT"));
            }
            total = total.saturating_add(document.markdown.len());
            if total > MAX_SAVE_BYTES {
                return Err(SaveError::Invalid("SAVE_BYTE_LIMIT"));
            }
        }
        // JSON escaping can make the wire representation much larger than the
        // bounded Markdown. Hash identical protocol bytes through a small buffer
        // instead of allocating a second complete encoded request.
        let mut writer = BufWriter::with_capacity(64 * 1024, IntentHash(Sha256::new()));
        serde_json::to_writer(
            &mut writer,
            &("specspace-save-v1", actor, proposal, repository, &request),
        )
        .map_err(|_| SaveError::Invalid("INVALID_SAVE_INTENT"))?;
        let fingerprint = format!(
            "{:x}",
            writer
                .into_inner()
                .map_err(|_| SaveError::Invalid("INVALID_SAVE_INTENT"))?
                .0
                .finalize()
        );
        Ok(Self {
            actor,
            proposal,
            repository,
            head,
            request,
            fingerprint,
        })
    }
    pub fn actor(&self) -> Uuid {
        self.actor
    }
    pub fn proposal(&self) -> Uuid {
        self.proposal
    }
    pub fn repository(&self) -> Uuid {
        self.repository
    }
    pub fn head(&self) -> &CommitSha {
        &self.head
    }
    pub fn request(&self) -> &SaveRequest {
        &self.request
    }
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    pub fn prepare(&self, snapshot: SaveSnapshot) -> Result<PreparedSave, SaveError> {
        if snapshot.repository_id != self.repository || snapshot.commit != self.head {
            return Err(SaveError::SnapshotMismatch);
        }
        if snapshot.files.len() > 1000
            || snapshot.catalog.documents.len() > 1000
            || snapshot
                .files
                .iter()
                .try_fold(0usize, |total, file| total.checked_add(file.markdown.len()))
                .is_none_or(|total| total > 20 * 1024 * 1024)
        {
            return Err(SaveError::Invalid("BOOK_TOO_LARGE"));
        }
        let mut paths = HashSet::new();
        let mut by_id = HashMap::new();
        if snapshot.catalog.schema_version != 1 {
            return Err(SaveError::Invalid("INVALID_CATALOG"));
        }
        for entry in &snapshot.catalog.documents {
            if !crate::draft::valid_document_id(&entry.document_id)
                || RepositoryPath::parse(&entry.path).is_err()
                || !entry.path.starts_with("docs/")
                || !entry.path.ends_with(".md")
                || !paths.insert(entry.path.to_lowercase())
                || by_id
                    .insert(entry.document_id.as_str(), entry.path.as_str())
                    .is_some()
            {
                return Err(SaveError::Invalid("INVALID_CATALOG"));
            }
        }
        let mut source_paths = HashSet::new();
        let mut by_path = HashMap::new();
        let mut files: Vec<_> = snapshot
            .files
            .into_iter()
            .map(|file| (file.path, file.markdown))
            .collect();
        for (index, (path, source)) in files.iter().enumerate() {
            if source.len() > MAX_FILE_BYTES {
                return Err(SaveError::Invalid("FILE_TOO_LARGE"));
            }
            if RepositoryPath::parse(path).is_err()
                || !path.starts_with("docs/")
                || !path.ends_with(".md")
                || !source_paths.insert(path.to_lowercase())
            {
                return Err(SaveError::Invalid("INVALID_SOURCE_MANIFEST"));
            }
            by_path.insert(path.clone(), index);
        }
        let mut changes = vec![];
        let mut total = 0usize;
        for change in &self.request.documents {
            let path = by_id
                .get(change.document_id.as_str())
                .ok_or(SaveError::Invalid("DOCUMENT_NOT_IN_CATALOG"))?;
            let index = by_path
                .get(*path)
                .copied()
                .ok_or(SaveError::Invalid("CATALOG_FILE_MISSING"))?;
            let original = &files[index].1;
            if hash(original.as_bytes()) != change.expected_document_sha256 {
                return Err(SaveError::DocumentChanged(change.document_id.clone()));
            }
            // A byte-identical request never triggers normalization of old files.
            if change.markdown == *original {
                continue;
            }
            let normalized =
                save_validation::normalize(&change.markdown).map_err(SaveError::Invalid)?;
            if normalized == *original {
                continue;
            }
            total = total.saturating_add(normalized.len());
            if total > MAX_SAVE_BYTES {
                return Err(SaveError::Invalid("SAVE_BYTE_LIMIT"));
            }
            let old_normalized = save_validation::normalize(original).ok();
            let old_sections = old_normalized
                .as_deref()
                .and_then(|source| save_validation::section_ids(source).ok());
            let prepared = PreparedDocument {
                document_id: change.document_id.clone(),
                path: RepositoryPath::parse(*path)
                    .map_err(|_| SaveError::Invalid("PATH_FORBIDDEN"))?,
                sha256: hash(normalized.as_bytes()),
                normalization_changed: normalized != change.markdown,
                normalization_only: old_normalized.as_ref() == Some(&normalized),
                markdown: normalized.clone(),
                removed_section_ids: old_sections,
            };
            files[index].1 = normalized;
            changes.push(prepared);
        }
        let validation =
            save_validation::validate(&snapshot.catalog, &files, &snapshot.asset_paths);
        if !validation.ready() {
            return Err(SaveError::Validation(validation));
        }
        for change in &mut changes {
            if let Some(old) = change.removed_section_ids.take() {
                let current: HashSet<_> = validation
                    .section_ids
                    .get(&change.document_id)
                    .into_iter()
                    .flatten()
                    .map(String::as_str)
                    .collect();
                let mut removed: Vec<_> = old
                    .into_iter()
                    .filter(|id| !current.contains(id.as_str()))
                    .collect();
                removed.sort();
                change.removed_section_ids = Some(removed);
            }
        }
        Ok(PreparedSave {
            actor: self.actor,
            proposal: self.proposal,
            repository: self.repository,
            fingerprint: self.fingerprint.clone(),
            expected_head: self.head.clone(),
            expected_proposal_version: self.request.expected_proposal_version,
            documents: changes,
            validation,
        })
    }
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

struct IntentHash(Sha256);
impl Write for IntentHash {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
