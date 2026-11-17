//! Pure comparison preparation: no authority, provider I/O, cache provenance or
//! review success is implied. The caller must prove/pin the merge base and head,
//! authorize the repository/proposal, and recheck access before serving a result.
use crate::{draft::valid_document_id, save::SourceFile};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use specspace_document_engine::{catalog::Catalog, markdown};
use specspace_domain::RepositoryPath;
use specspace_git_adapter::{CommitSha, FileEntry, FileMode, blob};
use std::collections::{BTreeMap, HashMap, HashSet};
use uuid::Uuid;

const CATALOG: &str = ".specspace/catalog.json";
const MAX_FILES: usize = 20_000;
const MAX_DOCUMENTS: usize = 1000;
const MAX_DOCUMENT_BYTES: u64 = 20 * 1024 * 1024;
const MAX_TEXT_BYTES: usize = 2 * 1024 * 1024;
const MAX_FINDINGS: usize = 1000;
#[derive(Debug, PartialEq, Eq)]
pub enum ComparisonError {
    InvalidScope,
    InvalidTree,
    TooLarge,
    MissingSource,
    ContentMismatch,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Finding {
    pub code: String,
    pub path: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Side {
    pub path: String,
    pub blob_oid: String,
    pub bytes: u64,
    pub executable: bool,
    pub sha256: Option<String>,
    pub document_id: Option<String>,
    pub catalog_position: Option<usize>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum Kind {
    Document,
    Asset,
    Catalog,
    Other,
}
fn kind(path: &str) -> Kind {
    if path == CATALOG {
        Kind::Catalog
    } else if path.starts_with("docs/") && path.ends_with(".md") {
        Kind::Document
    } else if path.starts_with("assets/") {
        Kind::Asset
    } else {
        Kind::Other
    }
}
struct File {
    side: Side,
    source: Option<String>,
}
pub struct Snapshot {
    repository: Uuid,
    commit: CommitSha,
    files: BTreeMap<String, File>,
    findings: Vec<Finding>,
    findings_total: usize,
}
impl Snapshot {
    pub fn prepare(
        repository: Uuid,
        commit: CommitSha,
        files: Vec<FileEntry>,
        sources: Vec<SourceFile>,
    ) -> Result<Self, ComparisonError> {
        if repository.is_nil() {
            return Err(ComparisonError::InvalidScope);
        }
        if files.len() > MAX_FILES {
            return Err(ComparisonError::TooLarge);
        }
        if sources.len() > MAX_DOCUMENTS + 1 {
            return Err(ComparisonError::TooLarge);
        }
        let mut actual_document_bytes = 0u64;
        let mut source_map = HashMap::new();
        for source in sources {
            if !matches!(kind(&source.path), Kind::Document | Kind::Catalog) {
                return Err(ComparisonError::InvalidTree);
            }
            if kind(&source.path) == Kind::Document {
                actual_document_bytes =
                    actual_document_bytes.saturating_add(source.markdown.len() as u64);
            }
            if actual_document_bytes > MAX_DOCUMENT_BYTES {
                return Err(ComparisonError::TooLarge);
            }
            if source.markdown.len() > MAX_TEXT_BYTES {
                return Err(ComparisonError::TooLarge);
            }
            if source_map.insert(source.path, source.markdown).is_some() {
                return Err(ComparisonError::InvalidTree);
            }
        }
        let mut snapshot = Self {
            repository,
            commit,
            files: BTreeMap::new(),
            findings: vec![],
            findings_total: 0,
        };
        let mut paths = HashSet::new();
        let mut documents = 0;
        let mut document_bytes = 0u64;
        let mut files = files;
        files.sort_by(|a, b| a.path.as_str().cmp(b.path.as_str()));
        for entry in files {
            let path = entry.path.as_str().to_owned();
            let file_kind = kind(&path);
            if !paths.insert(path.to_lowercase()) {
                return Err(ComparisonError::InvalidTree);
            }
            if file_kind == Kind::Document {
                documents += 1;
                document_bytes = document_bytes.saturating_add(entry.size);
                if documents > MAX_DOCUMENTS || document_bytes > MAX_DOCUMENT_BYTES {
                    return Err(ComparisonError::TooLarge);
                }
            }
            let source = if matches!(file_kind, Kind::Document | Kind::Catalog) {
                if entry.size > MAX_TEXT_BYTES as u64 {
                    return Err(ComparisonError::TooLarge);
                }
                let source = source_map
                    .remove(&path)
                    .ok_or(ComparisonError::MissingSource)?;
                if source.len() as u64 != entry.size
                    || blob::oid(source.as_bytes(), entry.blob_sha.as_str().len())
                        .ok()
                        .as_ref()
                        != Some(&entry.blob_sha)
                {
                    return Err(ComparisonError::ContentMismatch);
                }
                Some(source)
            } else {
                None
            };
            let mut side = Side {
                path: path.clone(),
                blob_oid: entry.blob_sha.as_str().into(),
                bytes: entry.size,
                executable: entry.mode == FileMode::Executable,
                sha256: source
                    .as_ref()
                    .map(|s| format!("{:x}", Sha256::digest(s.as_bytes()))),
                document_id: None,
                catalog_position: None,
            };
            if file_kind == Kind::Document {
                match markdown::front_matter(source.as_ref().unwrap()) {
                    Ok((metadata, _)) => side.document_id = Some(metadata.document_id),
                    Err(code) => snapshot.finding(code, &path),
                }
                if side.executable {
                    snapshot.finding("DOCUMENT_MODE_UNSUPPORTED", &path);
                }
            }
            snapshot.files.insert(path, File { side, source });
        }
        if !source_map.is_empty() {
            return Err(ComparisonError::InvalidTree);
        }
        snapshot.catalog();
        let mut ids = BTreeMap::<String, Vec<String>>::new();
        for file in snapshot.files.values() {
            if let Some(id) = &file.side.document_id {
                ids.entry(id.clone())
                    .or_default()
                    .push(file.side.path.clone());
            }
        }
        for paths in ids.values().filter(|paths| paths.len() > 1) {
            for path in paths {
                snapshot.finding("DUPLICATE_DOCUMENT_ID", path);
            }
        }
        Ok(snapshot)
    }
    pub fn repository(&self) -> Uuid {
        self.repository
    }
    pub fn commit(&self) -> &CommitSha {
        &self.commit
    }
    /// Verified complete tree metadata and optional exact text, without
    /// exposing mutable source maps or serializing whole source snapshots.
    pub fn entries(&self) -> impl Iterator<Item = (&Side, Option<&str>)> {
        self.files
            .values()
            .map(|file| (&file.side, file.source.as_deref()))
    }
    pub fn source(&self, path: &str) -> Option<&str> {
        self.files.get(path)?.source.as_deref()
    }
    pub fn findings(&self) -> &[Finding] {
        &self.findings
    }
    pub fn findings_total(&self) -> usize {
        self.findings_total
    }
    fn finding(&mut self, code: &str, path: &str) {
        self.findings_total += 1;
        if self.findings.len() < MAX_FINDINGS {
            self.findings.push(Finding {
                code: code.into(),
                path: path.into(),
            });
        }
    }
    fn catalog(&mut self) {
        if self
            .files
            .get(CATALOG)
            .is_some_and(|file| file.side.executable)
        {
            self.finding("CATALOG_MODE_UNSUPPORTED", CATALOG);
        }
        let catalog = self
            .files
            .get(CATALOG)
            .and_then(|file| file.source.as_deref())
            .and_then(|s| serde_json::from_str::<Catalog>(s).ok());
        let Some(catalog) =
            catalog.filter(|c| c.schema_version == 1 && c.documents.len() <= MAX_DOCUMENTS)
        else {
            self.finding("CATALOG_INVALID_OR_MISSING", CATALOG);
            return;
        };
        let mut paths = HashSet::new();
        let mut ids = HashSet::new();
        for (position, entry) in catalog.documents.into_iter().enumerate() {
            if RepositoryPath::parse(&entry.path).is_err() || kind(&entry.path) != Kind::Document {
                self.finding("CATALOG_PATH_INVALID", CATALOG);
                continue;
            }
            if !valid_document_id(&entry.document_id)
                || !paths.insert(entry.path.clone())
                || !ids.insert(entry.document_id.clone())
            {
                self.finding("CATALOG_IDENTITY_AMBIGUOUS", CATALOG);
                continue;
            }
            match self
                .files
                .get_mut(&entry.path)
                .filter(|file| kind(&file.side.path) == Kind::Document)
            {
                Some(file)
                    if file.side.document_id.as_deref() == Some(entry.document_id.as_str()) =>
                {
                    file.side.catalog_position = Some(position)
                }
                Some(_) => self.finding("CATALOG_DOCUMENT_ID_MISMATCH", &entry.path),
                None => self.finding("CATALOG_FILE_MISSING", &entry.path),
            }
        }
        let orphans: Vec<_> = self
            .files
            .values()
            .filter(|file| {
                kind(&file.side.path) == Kind::Document && file.side.catalog_position.is_none()
            })
            .map(|file| file.side.path.clone())
            .collect();
        for path in orphans {
            self.finding("DOCUMENT_WITHOUT_CATALOG_POSITION", &path);
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum Status {
    Added,
    Deleted,
    Modified,
    Renamed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum MatchMethod {
    DocumentId,
    Path,
    ExactBlob,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Change {
    /// File-level identity binds both snapshots and paths; not a review approval.
    pub id: String,
    pub kind: Kind,
    pub status: Status,
    pub matched_by: Option<MatchMethod>,
    pub before: Option<Side>,
    pub after: Option<Side>,
    pub content_changed: bool,
    pub mode_changed: bool,
    pub catalog_position_changed: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Inventory {
    pub id: String,
    pub repository_id: Uuid,
    pub before_commit: String,
    pub after_commit: String,
    pub before_file_count: usize,
    pub after_file_count: usize,
    pub changes: Vec<Change>,
}
impl Inventory {
    pub fn between(before: &Snapshot, after: &Snapshot) -> Result<Self, ComparisonError> {
        if before.repository != after.repository {
            return Err(ComparisonError::InvalidScope);
        }
        let mut pairs = BTreeMap::<String, (String, MatchMethod)>::new();
        let mut used = HashSet::new();
        // Unique source document IDs identify rename+edit. A malformed source or
        // duplicate ID cannot supply an identity, even if the catalog claims it.
        let ids = |snapshot: &Snapshot| {
            let mut result = HashMap::<String, Vec<String>>::new();
            for file in snapshot.files.values() {
                if let Some(id) = &file.side.document_id {
                    result
                        .entry(id.clone())
                        .or_default()
                        .push(file.side.path.clone());
                }
            }
            result
        };
        let old_ids = ids(before);
        let new_ids = ids(after);
        for (id, old_paths) in &old_ids {
            if old_paths.len() == 1
                && let Some(new_paths) = new_ids.get(id).filter(|paths| paths.len() == 1)
            {
                pairs.insert(
                    old_paths[0].clone(),
                    (new_paths[0].clone(), MatchMethod::DocumentId),
                );
                used.insert(new_paths[0].clone());
            }
        }
        for path in before.files.keys() {
            if !pairs.contains_key(path) && after.files.contains_key(path) && !used.contains(path) {
                pairs.insert(path.clone(), (path.clone(), MatchMethod::Path));
                used.insert(path.clone());
            }
        }
        // Exact-blob rename is conservative: repeated identical files stay
        // separate additions/deletions rather than inventing a path pairing.
        let mut old_blobs = BTreeMap::<String, Vec<String>>::new();
        let mut new_blobs = BTreeMap::<String, Vec<String>>::new();
        for (path, file) in &before.files {
            if !pairs.contains_key(path) {
                old_blobs
                    .entry(format!(
                        "{:?}:{}:{}",
                        kind(path),
                        file.side.blob_oid,
                        file.side.bytes
                    ))
                    .or_default()
                    .push(path.clone());
            }
        }
        for (path, file) in &after.files {
            if !used.contains(path) {
                new_blobs
                    .entry(format!(
                        "{:?}:{}:{}",
                        kind(path),
                        file.side.blob_oid,
                        file.side.bytes
                    ))
                    .or_default()
                    .push(path.clone());
            }
        }
        for (blob, paths) in old_blobs {
            if paths.len() == 1
                && let Some(new_paths) = new_blobs.get(&blob).filter(|paths| paths.len() == 1)
            {
                pairs.insert(
                    paths[0].clone(),
                    (new_paths[0].clone(), MatchMethod::ExactBlob),
                );
                used.insert(new_paths[0].clone());
            }
        }
        let mut inventory = Self {
            id: hash(&(
                "specspace-diff-inventory-v1",
                before.repository,
                before.commit.as_str(),
                after.commit.as_str(),
            )),
            repository_id: before.repository,
            before_commit: before.commit.as_str().into(),
            after_commit: after.commit.as_str().into(),
            before_file_count: before.files.len(),
            after_file_count: after.files.len(),
            changes: vec![],
        };
        for (path, old) in &before.files {
            let pair = pairs.get(path);
            let new = pair.map(|(path, _)| &after.files[path]);
            inventory.add(
                Some(&old.side),
                new.map(|file| &file.side),
                pair.map(|(_, method)| *method),
            );
        }
        for (path, new) in &after.files {
            if !used.contains(path) {
                inventory.add(None, Some(&new.side), None);
            }
        }
        inventory.changes.sort_by(|a, b| {
            a.before
                .as_ref()
                .or(a.after.as_ref())
                .unwrap()
                .path
                .cmp(&b.before.as_ref().or(b.after.as_ref()).unwrap().path)
        });
        Ok(inventory)
    }
    fn add(
        &mut self,
        before: Option<&Side>,
        after: Option<&Side>,
        matched_by: Option<MatchMethod>,
    ) {
        let content_changed =
            before.map(|s| (&s.blob_oid, s.bytes)) != after.map(|s| (&s.blob_oid, s.bytes));
        let mode_changed = matches!((before,after),(Some(a),Some(b)) if a.executable!=b.executable);
        let catalog_position_changed =
            matches!((before,after),(Some(a),Some(b)) if a.catalog_position!=b.catalog_position);
        let status = match (before, after) {
            (None, Some(_)) => Status::Added,
            (Some(_), None) => Status::Deleted,
            (Some(a), Some(b)) if a.path != b.path => Status::Renamed,
            _ if content_changed || mode_changed || catalog_position_changed => Status::Modified,
            _ => return,
        };
        let selected = after.or(before).unwrap();
        let id = hash(&(
            "specspace-diff-file-v1",
            &self.id,
            before.map(|s| (&s.path, &s.blob_oid, s.executable)),
            after.map(|s| (&s.path, &s.blob_oid, s.executable)),
        ));
        self.changes.push(Change {
            id,
            kind: kind(&selected.path),
            status,
            matched_by,
            before: before.cloned(),
            after: after.cloned(),
            content_changed,
            mode_changed,
            catalog_position_changed,
        });
    }
}
fn hash(value: &impl Serialize) -> String {
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(value).expect("fixed comparison metadata serializes"))
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    type Tree = BTreeMap<String, (Vec<u8>, FileMode)>;
    fn markdown(id: &str, text: &str) -> String {
        format!(
            "---\ndocument_id: {id}\ntitle: Synthetic\nowner_team: fixture\nschema_version: 1\n---\n{text}\n"
        )
    }
    fn tree(documents: &[(&str, &str, &str)]) -> Tree {
        let mut tree = Tree::new();
        let entries: Vec<_> = documents
            .iter()
            .map(|(path, id, text)| {
                tree.insert(
                    (*path).into(),
                    (markdown(id, text).into_bytes(), FileMode::Regular),
                );
                serde_json::json!({"document_id":id,"path":path})
            })
            .collect();
        tree.insert(
            CATALOG.into(),
            (
                serde_json::to_vec(&serde_json::json!({"schema_version":1,"documents":entries}))
                    .unwrap(),
                FileMode::Regular,
            ),
        );
        tree
    }
    fn input(tree: Tree) -> (Vec<FileEntry>, Vec<SourceFile>) {
        let mut files = vec![];
        let mut sources = vec![];
        for (path, (bytes, mode)) in tree {
            files.push(FileEntry {
                path: RepositoryPath::parse(&path).unwrap(),
                blob_sha: blob::oid(&bytes, 40).unwrap(),
                size: bytes.len() as u64,
                mode,
            });
            if matches!(kind(&path), Kind::Document | Kind::Catalog) {
                sources.push(SourceFile {
                    path,
                    markdown: String::from_utf8(bytes).unwrap(),
                });
            }
        }
        (files, sources)
    }
    fn snapshot(repository: Uuid, commit: &str, tree: Tree) -> Snapshot {
        let (files, sources) = input(tree);
        Snapshot::prepare(
            repository,
            CommitSha::parse(commit.repeat(40)).unwrap(),
            files,
            sources,
        )
        .unwrap()
    }
    #[test]
    fn rename_with_content_edits_and_catalog_reordering_follow_unique_verified_document_ids() {
        let repo = Uuid::new_v4();
        let before = snapshot(
            repo,
            "a",
            tree(&[
                ("docs/a.md", "DOC-A", "before"),
                ("docs/b.md", "DOC-B", "same"),
            ]),
        );
        let after = snapshot(
            repo,
            "b",
            tree(&[
                ("docs/b.md", "DOC-B", "same"),
                ("docs/renamed.md", "DOC-A", "after"),
            ]),
        );
        let diff = Inventory::between(&before, &after).unwrap();
        assert!(before.findings().is_empty());
        assert!(after.findings().is_empty());
        let changed = diff
            .changes
            .iter()
            .find(|c| c.before.as_ref().is_some_and(|s| s.path == "docs/a.md"))
            .unwrap();
        assert_eq!(changed.status, Status::Renamed);
        assert_eq!(changed.matched_by, Some(MatchMethod::DocumentId));
        assert!(changed.content_changed);
        assert!(changed.catalog_position_changed);
        assert_eq!(changed.after.as_ref().unwrap().path, "docs/renamed.md");
        let reordered = diff
            .changes
            .iter()
            .find(|c| c.after.as_ref().is_some_and(|s| s.path == "docs/b.md"))
            .unwrap();
        assert!(!reordered.content_changed);
        assert!(reordered.catalog_position_changed);
        assert_eq!(reordered.status, Status::Modified);
        assert!(diff.changes.iter().any(|c| c.kind == Kind::Catalog));
        assert_eq!(
            before.source("docs/a.md"),
            Some(markdown("DOC-A", "before").as_str())
        );
    }
    #[test]
    fn image_and_other_file_changes_are_visible_without_claiming_media_validation_or_fabricating_ambiguous_renames()
     {
        let repo = Uuid::new_v4();
        let mut before = tree(&[]);
        let mut after = before.clone();
        for path in ["assets/one.png", "assets/two.png"] {
            before.insert(path.into(), (vec![255, 0, 1], FileMode::Regular));
        }
        for path in ["assets/three.png", "assets/four.png"] {
            after.insert(path.into(), (vec![255, 0, 1], FileMode::Regular));
        }
        before.insert("README.md".into(), (b"before".to_vec(), FileMode::Regular));
        after.insert(
            "README.md".into(),
            (b"after".to_vec(), FileMode::Executable),
        );
        before.insert(
            "old.txt".into(),
            (b"unique bytes".to_vec(), FileMode::Regular),
        );
        after.insert(
            "new.txt".into(),
            (b"unique bytes".to_vec(), FileMode::Regular),
        );
        let diff =
            Inventory::between(&snapshot(repo, "a", before), &snapshot(repo, "b", after)).unwrap();
        let assets: Vec<_> = diff
            .changes
            .iter()
            .filter(|c| c.kind == Kind::Asset)
            .collect();
        assert_eq!(assets.len(), 4);
        assert!(
            assets
                .iter()
                .all(|c| matches!(c.status, Status::Added | Status::Deleted))
        );
        let readme = diff
            .changes
            .iter()
            .find(|c| c.before.as_ref().is_some_and(|s| s.path == "README.md"))
            .unwrap();
        assert!(readme.content_changed && readme.mode_changed);
        assert!(readme.after.as_ref().unwrap().sha256.is_none());
        let rename = diff
            .changes
            .iter()
            .find(|c| c.status == Status::Renamed)
            .unwrap();
        assert!(!rename.content_changed);
        assert_eq!(rename.matched_by, Some(MatchMethod::ExactBlob));
        assert_eq!(rename.after.as_ref().unwrap().path, "new.txt");
    }
    #[test]
    fn malformed_orphaned_and_ambiguous_document_sources_remain_visible_without_inventing_identity()
    {
        let repo = Uuid::new_v4();
        let mut old = tree(&[
            ("docs/a.md", "DOC-DUP", "old A"),
            ("docs/b.md", "DOC-DUP", "old B"),
        ]);
        old.insert(
            "docs/broken.md".into(),
            (b"---\nunfinished".to_vec(), FileMode::Regular),
        );
        let before = snapshot(repo, "a", old);
        assert!(
            before
                .findings()
                .iter()
                .any(|f| f.code == "DUPLICATE_DOCUMENT_ID")
        );
        assert_eq!(before.source("docs/broken.md"), Some("---\nunfinished"));
        let mut new = tree(&[("docs/c.md", "DOC-DUP", "different content")]);
        new.insert(
            "docs/broken.md".into(),
            (b"still broken".to_vec(), FileMode::Regular),
        );
        let after = snapshot(repo, "b", new);
        let diff = Inventory::between(&before, &after).unwrap();
        assert!(!diff.changes.iter().any(|c| c.status == Status::Renamed));
        let broken = diff
            .changes
            .iter()
            .find(|c| {
                c.before
                    .as_ref()
                    .is_some_and(|s| s.path == "docs/broken.md")
            })
            .unwrap();
        assert!(broken.before.as_ref().unwrap().document_id.is_none());
        assert_eq!(broken.status, Status::Modified);
    }
    #[test]
    fn changed_identity_at_the_same_path_and_bad_catalogs_do_not_hide_raw_content() {
        let repo = Uuid::new_v4();
        let before = snapshot(repo, "a", tree(&[("docs/a.md", "DOC-A", "old")]));
        let mut files = tree(&[("docs/a.md", "DOC-B", "new")]);
        files.get_mut(CATALOG).unwrap().0 = b"{invalid".to_vec();
        let after = snapshot(repo, "b", files);
        assert!(
            after
                .findings()
                .iter()
                .any(|f| f.code == "CATALOG_INVALID_OR_MISSING")
        );
        let diff = Inventory::between(&before, &after).unwrap();
        let document = diff
            .changes
            .iter()
            .find(|c| c.kind == Kind::Document)
            .unwrap();
        assert_eq!(document.status, Status::Modified);
        assert_eq!(
            document.before.as_ref().unwrap().document_id.as_deref(),
            Some("DOC-A")
        );
        assert_eq!(
            document.after.as_ref().unwrap().document_id.as_deref(),
            Some("DOC-B")
        );
    }
    #[test]
    fn identities_are_snapshot_bound_deterministic_and_no_change_is_not_approval() {
        let repo = Uuid::new_v4();
        let files = tree(&[("docs/a.md", "DOC-A", "same")]);
        let before = snapshot(repo, "a", files.clone());
        let after = snapshot(repo, "b", files.clone());
        let no_change = Inventory::between(&before, &after).unwrap();
        assert!(no_change.changes.is_empty());
        let changed = tree(&[("docs/a.md", "DOC-A", "different")]);
        let next = snapshot(repo, "c", changed.clone());
        let diff = Inventory::between(&before, &next).unwrap();
        let again = Inventory::between(&before, &snapshot(repo, "c", changed.clone())).unwrap();
        assert_eq!(
            serde_json::to_value(&diff).unwrap(),
            serde_json::to_value(again).unwrap()
        );
        let later = Inventory::between(&after, &snapshot(repo, "c", changed)).unwrap();
        assert_ne!(diff.id, later.id);
        assert_ne!(diff.changes[0].id, later.changes[0].id);
        assert_eq!(
            Inventory::between(&before, &snapshot(Uuid::new_v4(), "b", files)).unwrap_err(),
            ComparisonError::InvalidScope
        );
    }
    #[test]
    fn forged_bytes_sizes_missing_extra_sources_and_case_folded_duplicate_paths_fail_closed() {
        let repo = Uuid::new_v4();
        let commit = CommitSha::parse("a".repeat(40)).unwrap();
        for mode in 0..5 {
            let (mut files, mut sources) = input(tree(&[("docs/a.md", "DOC-A", "source")]));
            match mode {
                0 => sources[1].markdown.push('x'),
                1 => files[1].size += 1,
                2 => {
                    sources.pop();
                }
                3 => sources.push(SourceFile {
                    path: "docs/extra.md".into(),
                    markdown: "unexpected".into(),
                }),
                _ => files.push(FileEntry {
                    path: RepositoryPath::parse("docs/A.md").unwrap(),
                    blob_sha: files[1].blob_sha.clone(),
                    size: files[1].size,
                    mode: FileMode::Regular,
                }),
            }
            assert!(Snapshot::prepare(repo, commit.clone(), files, sources).is_err());
        }
    }
    #[test]
    fn invalid_catalog_findings_are_bounded_without_losing_the_complete_count_or_document_inventory()
     {
        let repo = Uuid::new_v4();
        let mut files = tree(&[]);
        let entries: Vec<_> = (0..1000)
            .map(|n| (format!("docs/{n}.md"), format!("DOC-{n}")))
            .collect();
        for (path, id) in entries {
            files.insert(
                path,
                (markdown(&id, "text").into_bytes(), FileMode::Executable),
            );
        }
        let snapshot = snapshot(repo, "a", files);
        assert_eq!(snapshot.findings().len(), MAX_FINDINGS);
        assert!(snapshot.findings_total() > MAX_FINDINGS);
        assert_eq!(snapshot.files.len(), 1001);
        assert!(snapshot.source("docs/999.md").is_some());
    }
    #[test]
    fn source_quotas_and_catalog_paths_are_checked_before_inventory_publication() {
        let repo = Uuid::new_v4();
        let mut files = tree(&[("docs/a.md", "DOC-A", "source")]);
        files.get_mut(CATALOG).unwrap().0=serde_json::to_vec(&serde_json::json!({"schema_version":1,"documents":[
            {"document_id":"DOC-A","path":"docs/missing.md"},{"document_id":"DOC-OTHER","path":"docs/a.md"},
            {"document_id":"DOC-BAD","path":"../../secret"}
        ]})).unwrap();
        let snapshot = snapshot(repo, "a", files);
        assert!(
            snapshot
                .findings()
                .iter()
                .any(|f| f.code == "CATALOG_FILE_MISSING" && f.path == "docs/missing.md")
        );
        assert!(
            snapshot
                .findings()
                .iter()
                .any(|f| f.code == "CATALOG_DOCUMENT_ID_MISMATCH" && f.path == "docs/a.md")
        );
        assert!(
            snapshot
                .findings()
                .iter()
                .any(|f| f.code == "CATALOG_PATH_INVALID" && f.path == CATALOG)
        );
        let sources = (0..11)
            .map(|n| SourceFile {
                path: format!("docs/{n}.md"),
                markdown: "x".repeat(MAX_TEXT_BYTES),
            })
            .collect();
        assert!(matches!(
            Snapshot::prepare(
                repo,
                CommitSha::parse("a".repeat(40)).unwrap(),
                vec![],
                sources
            ),
            Err(ComparisonError::TooLarge)
        ));
    }
}
