use async_trait::async_trait;
use serde::Serialize;
use specspace_document_engine::{
    catalog::{self, Catalog},
    find::{self, MatchPage},
    markdown::{self, Document},
};
use specspace_domain::RepositoryPath;
use specspace_git_adapter::{CommitSha, GitError, GitProvider};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::Semaphore;
use uuid::Uuid;

#[derive(Debug, PartialEq, Eq)]
pub enum BookError {
    NotFound,
    AuthorizationUnavailable,
    Provider(GitError),
    InvalidCatalog,
    BookTooLarge,
    ProcessingUnavailable,
    SearchSnapshotNotReady,
    ProposalNotReady,
    InvalidQuery(&'static str),
    RepositoryNotConfigured,
    InvalidRepositoryConfiguration,
    InvalidAsset(&'static str),
}

#[async_trait]
pub trait ReadAuthorization: Send + Sync {
    /// Must query current permissions, rather than a cached browser role.
    async fn can_read(&self, actor: Uuid, repository: Uuid) -> Result<bool, BookError>;
}

#[derive(Debug, Serialize)]
pub struct BookFinding {
    pub path: String,
    pub code: String,
    pub source_start: Option<usize>,
    pub source_end: Option<usize>,
}

#[derive(Debug)]
pub struct BookDocument {
    pub document_id: String,
    pub path: String,
    pub content: Option<Document>,
    pub error_code: Option<String>,
}

#[derive(Debug)]
pub struct BookSnapshot {
    pub commit: CommitSha,
    pub updated_at: String,
    pub documents: Vec<BookDocument>,
    pub findings: Vec<BookFinding>,
}

impl BookSnapshot {
    pub fn highlights(
        &self,
        document_id: &str,
        query: &str,
        windows: &[find::MatchWindow],
    ) -> Result<Vec<find::Highlight>, BookError> {
        if !self.findings.is_empty() || self.documents.iter().any(|doc| doc.content.is_none()) {
            return Err(BookError::SearchSnapshotNotReady);
        }
        let document = self
            .documents
            .iter()
            .find(|doc| doc.document_id == document_id)
            .and_then(|doc| doc.content.as_ref())
            .ok_or(BookError::NotFound)?;
        find::highlights(
            self.commit.as_str(),
            &document.visible_blocks(),
            query,
            windows,
        )
        .map_err(BookError::InvalidQuery)
    }

    pub fn find(&self, query: &str, offset: usize, limit: usize) -> Result<MatchPage, BookError> {
        if !self.findings.is_empty() || self.documents.iter().any(|doc| doc.content.is_none()) {
            return Err(BookError::SearchSnapshotNotReady);
        }
        let blocks: Vec<_> = self
            .documents
            .iter()
            .filter_map(|doc| doc.content.as_ref())
            .flat_map(Document::visible_blocks)
            .collect();
        find::find(self.commit.as_str(), &blocks, query, offset, limit)
            .map_err(BookError::InvalidQuery)
    }
}

pub struct BookService {
    repository_id: Uuid,
    provider: Arc<dyn GitProvider>,
    authorization: Arc<dyn ReadAuthorization>,
    parsing_slots: Arc<Semaphore>,
}

impl BookService {
    pub fn new(
        repository_id: Uuid,
        provider: Arc<dyn GitProvider>,
        authorization: Arc<dyn ReadAuthorization>,
    ) -> Self {
        Self {
            repository_id,
            provider,
            authorization,
            parsing_slots: Arc::new(Semaphore::new(2)),
        }
    }

    pub async fn load_main(&self, actor: Uuid) -> Result<BookSnapshot, BookError> {
        if !self
            .authorization
            .can_read(actor, self.repository_id)
            .await?
        {
            return Err(BookError::NotFound);
        }
        let commit = self
            .provider
            .main_head()
            .await
            .map_err(BookError::Provider)?;
        self.load_commit(actor, commit).await
    }

    pub async fn load_at(&self, actor: Uuid, commit: CommitSha) -> Result<BookSnapshot, BookError> {
        if !self
            .authorization
            .can_read(actor, self.repository_id)
            .await?
        {
            return Err(BookError::NotFound);
        }
        let main = self
            .provider
            .main_head()
            .await
            .map_err(BookError::Provider)?;
        if !self
            .provider
            .is_ancestor(&commit, &main)
            .await
            .map_err(BookError::Provider)?
        {
            return Err(BookError::NotFound);
        }
        self.load_commit(actor, commit).await
    }

    /// Only the reader may use a persisted proof of previously approved main
    /// history. Actual commit metadata/content and current ACL are still read.
    pub(crate) async fn load_recorded_main(
        &self,
        actor: Uuid,
        commit: CommitSha,
    ) -> Result<BookSnapshot, BookError> {
        if !self
            .authorization
            .can_read(actor, self.repository_id)
            .await?
        {
            return Err(BookError::NotFound);
        }
        self.load_commit(actor, commit).await
    }

    /// Used by the isolated proposal reader after resolving current scope.
    pub(crate) async fn load_proposal_at(
        &self,
        actor: Uuid,
        commit: CommitSha,
        head: &CommitSha,
    ) -> Result<BookSnapshot, BookError> {
        if !self
            .authorization
            .can_read(actor, self.repository_id)
            .await?
        {
            return Err(BookError::NotFound);
        }
        if &commit != head
            && !self
                .provider
                .is_ancestor(&commit, head)
                .await
                .map_err(BookError::Provider)?
        {
            return Err(BookError::NotFound);
        }
        self.load_commit(actor, commit).await
    }

    async fn load_commit(&self, actor: Uuid, commit: CommitSha) -> Result<BookSnapshot, BookError> {
        let metadata = self
            .provider
            .commit_metadata(&commit)
            .await
            .map_err(BookError::Provider)?;
        if metadata.sha != commit {
            return Err(BookError::Provider(GitError::InvalidResponse));
        }
        let files = self
            .provider
            .list_files(&commit)
            .await
            .map_err(BookError::Provider)?;
        let text_files: Vec<_> = files
            .iter()
            .filter(|entry| {
                entry.path.as_str().starts_with("docs/") && entry.path.as_str().ends_with(".md")
            })
            .collect();
        let declared_bytes = text_files
            .iter()
            .try_fold(0u64, |total, entry| total.checked_add(entry.size))
            .unwrap_or(u64::MAX);
        if text_files.len() > 1000 || declared_bytes > 20 * 1024 * 1024 {
            return Err(BookError::BookTooLarge);
        }
        let catalog_path = RepositoryPath::parse(".specspace/catalog.json")
            .map_err(|_| BookError::InvalidCatalog)?;
        let source = self
            .provider
            .read_text(&commit, &catalog_path)
            .await
            .map_err(BookError::Provider)?;
        let catalog: Catalog =
            serde_json::from_str(&source).map_err(|_| BookError::InvalidCatalog)?;
        if catalog.schema_version != 1 || catalog.documents.len() > 1000 {
            return Err(BookError::InvalidCatalog);
        }
        let mut loaded = vec![];
        let mut failures = HashMap::new();
        let mut findings = vec![];
        let mut actual_bytes = 0usize;
        for entry in text_files {
            let path = entry.path.as_str();
            let result = if entry.size > 2 * 1024 * 1024 {
                Err("FILE_TOO_LARGE")
            } else {
                let source = self
                    .provider
                    .read_text(&commit, &entry.path)
                    .await
                    .map_err(BookError::Provider)?;
                actual_bytes = actual_bytes.saturating_add(source.len());
                if actual_bytes > 20 * 1024 * 1024 {
                    return Err(BookError::BookTooLarge);
                }
                let slot = self
                    .parsing_slots
                    .clone()
                    .acquire_owned()
                    .await
                    .map_err(|_| BookError::ProcessingUnavailable)?;
                tokio::task::spawn_blocking(move || {
                    let _slot = slot;
                    let mut document = markdown::parse(&source)?;
                    if let Err(code) = specspace_document_engine::render::events(&document) {
                        document.findings.push(markdown::Finding {
                            code,
                            source_start: 0,
                            source_end: source.len(),
                        });
                    }
                    Ok(document)
                })
                .await
                .map_err(|_| BookError::ProcessingUnavailable)?
            };
            match result {
                Ok(document) => {
                    for finding in &document.findings {
                        findings.push(BookFinding {
                            path: path.into(),
                            code: finding.code.into(),
                            source_start: Some(finding.source_start),
                            source_end: Some(finding.source_end),
                        });
                    }
                    loaded.push((path, document));
                }
                Err(code) => {
                    failures.insert(path, code);
                    findings.push(BookFinding {
                        path: path.into(),
                        code: code.into(),
                        source_start: None,
                        source_end: None,
                    });
                }
            }
        }
        if let Err(issues) = catalog::validate(&catalog, &loaded) {
            for issue in issues {
                if issue.code == "CATALOG_FILE_MISSING"
                    && failures.contains_key(issue.path.as_str())
                {
                    continue;
                }
                findings.push(BookFinding {
                    path: issue.path,
                    code: issue.code.into(),
                    source_start: None,
                    source_end: None,
                });
            }
        }
        let mut by_path: HashMap<_, _> = loaded.into_iter().collect();
        let documents = catalog
            .documents
            .into_iter()
            .map(|entry| {
                let content = by_path.remove(entry.path.as_str());
                let error = failures.get(entry.path.as_str()).copied().or_else(|| {
                    if content.is_none() {
                        Some("CATALOG_FILE_MISSING")
                    } else {
                        None
                    }
                });
                BookDocument {
                    document_id: entry.document_id,
                    path: entry.path,
                    content,
                    error_code: error.map(str::to_owned),
                }
            })
            .collect();
        // Revocation during a provider read takes effect before publishing results.
        if !self
            .authorization
            .can_read(actor, self.repository_id)
            .await?
        {
            return Err(BookError::NotFound);
        }
        Ok(BookSnapshot {
            commit,
            updated_at: metadata.committed_at.to_rfc3339(),
            documents,
            findings,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use specspace_domain::BranchName;
    use specspace_git_adapter::{CommitRequest, FileEntry};
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    const SHA: &str = "1111111111111111111111111111111111111111";

    struct TestProvider {
        files: HashMap<String, String>,
        reads: Mutex<Vec<String>>,
        main_reads: AtomicUsize,
    }

    #[async_trait]
    impl GitProvider for TestProvider {
        async fn read_bytes(
            &self,
            _: &CommitSha,
            _: &RepositoryPath,
            _: usize,
        ) -> Result<Vec<u8>, GitError> {
            Err(GitError::ProviderRejected)
        }
        async fn commit_metadata(
            &self,
            commit: &CommitSha,
        ) -> Result<specspace_git_adapter::CommitMetadata, GitError> {
            Ok(specspace_git_adapter::CommitMetadata {
                sha: commit.clone(),
                committed_at: "2026-11-14T10:00:00Z".parse().unwrap(),
            })
        }
        async fn is_ancestor(
            &self,
            ancestor: &CommitSha,
            descendant: &CommitSha,
        ) -> Result<bool, GitError> {
            Ok(ancestor == descendant)
        }
        async fn main_head(&self) -> Result<CommitSha, GitError> {
            self.main_reads.fetch_add(1, Ordering::SeqCst);
            CommitSha::parse(SHA)
        }
        async fn branch_head(&self, _: &BranchName) -> Result<CommitSha, GitError> {
            Err(GitError::ProviderRejected)
        }
        async fn create_branch(
            &self,
            _: &BranchName,
            _: &CommitSha,
        ) -> Result<CommitSha, GitError> {
            Err(GitError::ProviderRejected)
        }
        async fn commit(&self, _: CommitRequest<'_>) -> Result<CommitSha, GitError> {
            Err(GitError::ProviderRejected)
        }
        async fn read_text(
            &self,
            commit: &CommitSha,
            path: &RepositoryPath,
        ) -> Result<String, GitError> {
            self.reads.lock().unwrap().push(commit.as_str().into());
            self.files
                .get(path.as_str())
                .cloned()
                .ok_or(GitError::InvalidResponse)
        }
        async fn list_files(&self, commit: &CommitSha) -> Result<Vec<FileEntry>, GitError> {
            self.reads.lock().unwrap().push(commit.as_str().into());
            Ok(self
                .files
                .iter()
                .map(|(path, source)| FileEntry {
                    path: RepositoryPath::parse(path).unwrap(),
                    blob_sha: CommitSha::parse(SHA).unwrap(),
                    size: source.len() as u64,
                })
                .collect())
        }
    }

    struct TestAuthorization {
        actor: Uuid,
        repository: Uuid,
        calls: AtomicUsize,
        revoke_after: usize,
    }

    #[async_trait]
    impl ReadAuthorization for TestAuthorization {
        async fn can_read(&self, actor: Uuid, repository: Uuid) -> Result<bool, BookError> {
            Ok(actor == self.actor
                && repository == self.repository
                && self.calls.fetch_add(1, Ordering::SeqCst) < self.revoke_after)
        }
    }

    fn fixtures() -> TestProvider {
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../fixtures/repository");
        let paths = [
            ".specspace/catalog.json",
            "docs/overview.md",
            "docs/permissions.md",
            "docs/media.md",
            "docs/recordings.md",
            "docs/player.md",
            "docs/operations.md",
        ];
        TestProvider {
            files: paths
                .iter()
                .map(|path| {
                    (
                        path.to_string(),
                        std::fs::read_to_string(root.join(path)).unwrap(),
                    )
                })
                .collect(),
            reads: Mutex::new(vec![]),
            main_reads: AtomicUsize::new(0),
        }
    }

    fn service(provider: Arc<TestProvider>, revoke_after: usize) -> (BookService, Uuid) {
        let actor = Uuid::new_v4();
        let repository = Uuid::new_v4();
        let authorization = Arc::new(TestAuthorization {
            actor,
            repository,
            calls: AtomicUsize::new(0),
            revoke_after,
        });
        (BookService::new(repository, provider, authorization), actor)
    }

    #[tokio::test]
    async fn book_reads_one_complete_authorized_snapshot_in_catalog_order() {
        let provider = Arc::new(fixtures());
        let (service, actor) = service(provider.clone(), usize::MAX);
        let book = service.load_main(actor).await.unwrap();
        assert_eq!(book.documents.len(), 6);
        assert_eq!(book.updated_at, "2026-11-14T10:00:00+00:00");
        assert_eq!(book.documents[0].document_id, "DOC-OVERVIEW");
        assert!(book.findings.is_empty());
        assert_eq!(book.find("מחיקה", 0, 100).unwrap().total_matches, 4);
        assert_eq!(provider.main_reads.load(Ordering::SeqCst), 1);
        assert!(provider.reads.lock().unwrap().iter().all(|sha| sha == SHA));
    }

    #[tokio::test]
    async fn denied_and_mid_read_revoked_permissions_never_publish_a_book() {
        for revoke_after in [0, 1] {
            let provider = Arc::new(fixtures());
            let (service, actor) = service(provider.clone(), revoke_after);
            assert!(matches!(
                service.load_main(actor).await,
                Err(BookError::NotFound)
            ));
            assert_eq!(
                provider.main_reads.load(Ordering::SeqCst),
                usize::from(revoke_after == 1)
            );
        }
    }

    #[tokio::test]
    async fn invalid_chapter_has_a_placeholder_and_orphan_is_reported() {
        let mut provider = fixtures();
        let orphan = provider.files["docs/player.md"]
            .replace("DOC-PLAYER", "DOC-ORPHAN")
            .replace("SEC-PLAYER", "SEC-ORPHAN");
        provider.files.insert("docs/orphan.md".into(), orphan);
        provider
            .files
            .insert("docs/player.md".into(), "invalid front matter".into());
        let (service, actor) = service(Arc::new(provider), usize::MAX);
        let book = service.load_main(actor).await.unwrap();
        assert_eq!(book.documents.len(), 6);
        assert_eq!(
            book.documents[4].error_code.as_deref(),
            Some("FRONT_MATTER_REQUIRED")
        );
        assert!(book.documents[4].content.is_none());
        assert!(book.documents[0].content.is_some());
        assert!(
            book.findings
                .iter()
                .any(|finding| finding.path == "docs/orphan.md"
                    && finding.code == "UNCATALOGUED_DOCUMENT")
        );
        assert_eq!(
            book.find("מחיקה", 0, 100).unwrap_err(),
            BookError::SearchSnapshotNotReady
        );
    }

    #[tokio::test]
    async fn arbitrary_branch_commits_cannot_be_read_as_main_history() {
        let provider = Arc::new(fixtures());
        let (service, actor) = service(provider.clone(), usize::MAX);
        let unrelated = CommitSha::parse("2222222222222222222222222222222222222222").unwrap();
        assert!(matches!(
            service.load_at(actor, unrelated).await,
            Err(BookError::NotFound)
        ));
        assert!(provider.reads.lock().unwrap().is_empty());
        assert_eq!(
            service
                .load_at(actor, CommitSha::parse(SHA).unwrap())
                .await
                .unwrap()
                .commit
                .as_str(),
            SHA
        );
    }

    struct ReaderAuthorization {
        actor: Uuid,
        repository: Uuid,
        active: AtomicBool,
    }

    #[async_trait]
    impl ReadAuthorization for ReaderAuthorization {
        async fn can_read(&self, actor: Uuid, repository: Uuid) -> Result<bool, BookError> {
            Ok(self.active.load(Ordering::SeqCst)
                && actor == self.actor
                && repository == self.repository)
        }
    }

    struct ReaderSource {
        provider: Arc<TestProvider>,
        online: AtomicBool,
    }

    #[async_trait]
    impl crate::reader::BookProviderFactory for ReaderSource {
        async fn provider(&self, _: Uuid) -> Result<Arc<dyn GitProvider>, BookError> {
            if self.online.load(Ordering::SeqCst) {
                Ok(self.provider.clone())
            } else {
                Err(BookError::Provider(GitError::ProviderUnavailable))
            }
        }
    }

    #[tokio::test]
    async fn cached_books_keep_version_identity_and_never_bypass_current_authorization() {
        let actor = Uuid::new_v4();
        let repository = Uuid::new_v4();
        let authorization = Arc::new(ReaderAuthorization {
            actor,
            repository,
            active: AtomicBool::new(true),
        });
        let source = Arc::new(ReaderSource {
            provider: Arc::new(fixtures()),
            online: AtomicBool::new(true),
        });
        let reader = crate::reader::BookReader::new(authorization.clone(), source.clone());
        let first = reader.main(actor, repository).await.unwrap();
        let reads = source.provider.reads.lock().unwrap().len();
        let again = reader
            .at(actor, repository, first.commit.clone())
            .await
            .unwrap();
        assert!(Arc::ptr_eq(&first, &again));
        assert_eq!(source.provider.reads.lock().unwrap().len(), reads);
        assert!(matches!(
            reader.at(actor, Uuid::new_v4(), first.commit.clone()).await,
            Err(BookError::NotFound)
        ));
        source.online.store(false, Ordering::SeqCst);
        assert!(
            reader
                .at(actor, repository, first.commit.clone())
                .await
                .is_ok()
        );
        assert!(matches!(
            reader.main(actor, repository).await,
            Err(BookError::Provider(GitError::ProviderUnavailable))
        ));
        authorization.active.store(false, Ordering::SeqCst);
        assert!(matches!(
            reader.at(actor, repository, first.commit.clone()).await,
            Err(BookError::NotFound)
        ));
        assert!(matches!(
            reader
                .find(
                    actor,
                    repository,
                    first.commit.clone(),
                    "מחיקה".into(),
                    0,
                    100
                )
                .await,
            Err(BookError::NotFound)
        ));
    }

    struct RecordedHistory {
        repository: Uuid,
        commit: CommitSha,
    }
    #[async_trait]
    impl crate::reader::MainHistoryProof for RecordedHistory {
        async fn contains(&self, repository: Uuid, commit: &CommitSha) -> Result<bool, BookError> {
            Ok(repository == self.repository && commit == &self.commit)
        }
    }

    #[tokio::test]
    async fn recorded_main_survives_a_cold_reader_without_approving_other_commits_or_revoked_users()
    {
        let actor = Uuid::new_v4();
        let repository = Uuid::new_v4();
        let authorization = Arc::new(ReaderAuthorization {
            actor,
            repository,
            active: AtomicBool::new(true),
        });
        let source = Arc::new(ReaderSource {
            provider: Arc::new(fixtures()),
            online: AtomicBool::new(true),
        });
        let recorded = CommitSha::parse("2222222222222222222222222222222222222222").unwrap();
        let reader = crate::reader::BookReader::new(authorization.clone(), source.clone())
            .with_history(Arc::new(RecordedHistory {
                repository,
                commit: recorded.clone(),
            }));
        let book = reader
            .at(actor, repository, recorded.clone())
            .await
            .unwrap();
        assert_eq!(book.commit, recorded);
        assert_eq!(source.provider.main_reads.load(Ordering::SeqCst), 0);
        assert!(!source.provider.reads.lock().unwrap().is_empty());
        assert!(
            source
                .provider
                .reads
                .lock()
                .unwrap()
                .iter()
                .all(|sha| sha == recorded.as_str())
        );
        let unknown = CommitSha::parse("3333333333333333333333333333333333333333").unwrap();
        assert!(matches!(
            reader.at(actor, repository, unknown).await,
            Err(BookError::NotFound)
        ));
        authorization.active.store(false, Ordering::SeqCst);
        assert!(matches!(
            reader.at(actor, repository, recorded).await,
            Err(BookError::NotFound)
        ));
    }
}
