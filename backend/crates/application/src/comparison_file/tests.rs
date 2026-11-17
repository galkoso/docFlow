use super::*;
use crate::{
    comparison::{MatchMethod, Status},
    comparison_reader::RepositoryBinding,
};
use specspace_domain::BranchName;
use specspace_git_adapter::{CommitMetadata, CommitReceipt, CommitRequest, FileEntry};
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

fn sha(n: char) -> CommitSha {
    CommitSha::parse(n.to_string().repeat(40)).unwrap()
}
fn side(path: &str, source: &str) -> Side {
    Side {
        path: path.into(),
        blob_oid: blob::oid(source.as_bytes(), 40).unwrap().as_str().into(),
        bytes: source.len() as u64,
        executable: false,
        sha256: Some(format!("{:x}", Sha256::digest(source.as_bytes()))),
        document_id: Some("DOC-ONE".into()),
        catalog_position: Some(0),
    }
}
struct Authority {
    context: Mutex<FileReadContext>,
    calls: AtomicUsize,
    fail_at: AtomicUsize,
    change_at: AtomicUsize,
}
#[async_trait]
impl ComparisonFileAuthorization for Authority {
    async fn resolve_file(
        &self,
        actor: Uuid,
        proposal: Uuid,
        snapshot: Uuid,
        file: &str,
    ) -> Result<FileReadContext, FileReadError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let mut context = self.context.lock().unwrap();
        if call == self.change_at.load(Ordering::SeqCst) {
            context.scope.version += 1;
        }
        if call == self.fail_at.load(Ordering::SeqCst) {
            return Err(FileReadError::NotFound);
        }
        if context.scope.actor != actor
            || context.scope.proposal != proposal
            || context.snapshot != snapshot
            || context.change.id != file
        {
            return Err(FileReadError::NotFound);
        }
        Ok(context.clone())
    }
}
struct Provider {
    context: FileReadContext,
    sources: Mutex<(String, String)>,
    reads: AtomicUsize,
    failure: Mutex<Option<GitError>>,
    gate: tokio::sync::Notify,
    block: std::sync::atomic::AtomicBool,
}
#[async_trait]
impl GitProvider for Provider {
    async fn read_text(
        &self,
        commit: &CommitSha,
        path: &RepositoryPath,
    ) -> Result<String, GitError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        if self.block.load(Ordering::SeqCst) {
            self.gate.notified().await;
        }
        if let Some(error) = self.failure.lock().unwrap().clone() {
            return Err(error);
        }
        let sources = self.sources.lock().unwrap();
        if commit == &self.context.merge_base {
            assert_eq!(
                path.as_str(),
                self.context.change.before.as_ref().unwrap().path
            );
            Ok(sources.0.clone())
        } else {
            assert_eq!(commit, &self.context.head);
            assert_eq!(
                path.as_str(),
                self.context.change.after.as_ref().unwrap().path
            );
            Ok(sources.1.clone())
        }
    }
    async fn main_head(&self) -> Result<CommitSha, GitError> {
        panic!("must not reselect main")
    }
    async fn branch_head(&self, _: &BranchName) -> Result<CommitSha, GitError> {
        panic!("must not reselect branch")
    }
    async fn create_branch(&self, _: &BranchName, _: &CommitSha) -> Result<CommitSha, GitError> {
        panic!("read only")
    }
    async fn commit(&self, _: CommitRequest<'_>) -> Result<CommitSha, GitError> {
        panic!("read only")
    }
    async fn read_bytes(
        &self,
        _: &CommitSha,
        _: &RepositoryPath,
        _: usize,
    ) -> Result<Vec<u8>, GitError> {
        panic!("text only")
    }
    async fn list_files(&self, _: &CommitSha) -> Result<Vec<FileEntry>, GitError> {
        panic!("must not reload full trees")
    }
    async fn commit_metadata(&self, _: &CommitSha) -> Result<CommitMetadata, GitError> {
        panic!("stored evidence")
    }
    async fn commit_receipt(&self, _: &CommitSha) -> Result<CommitReceipt, GitError> {
        panic!("stored evidence")
    }
    async fn merge_base(&self, _: &CommitSha, _: &CommitSha) -> Result<CommitSha, GitError> {
        panic!("stored evidence")
    }
    async fn is_ancestor(&self, _: &CommitSha, _: &CommitSha) -> Result<bool, GitError> {
        panic!("stored evidence")
    }
}
struct Factory {
    provider: Arc<Provider>,
    connects: AtomicUsize,
}
#[async_trait]
impl BoundProviderFactory for Factory {
    async fn provider_bound(
        &self,
        binding: &RepositoryBinding,
    ) -> Result<Arc<dyn GitProvider>, ComparisonReadError> {
        self.connects.fetch_add(1, Ordering::SeqCst);
        assert!(binding == &self.provider.context.scope.connection);
        Ok(self.provider.clone())
    }
}
struct Fixture {
    authority: Arc<Authority>,
    factory: Arc<Factory>,
    reader: Arc<ComparisonFileReader>,
}
impl Fixture {
    fn new(before: &str, after: &str) -> Self {
        let scope = ReadContext {
            actor: Uuid::new_v4(),
            organization: Uuid::new_v4(),
            proposal: Uuid::new_v4(),
            connection: RepositoryBinding {
                repository: Uuid::new_v4(),
                remote_id: 42,
                installation_id: 7,
            },
            branch: BranchName::parse("change/file".to_owned()).unwrap(),
            recorded_head: sha('4'),
            recorded_base: sha('1'),
            version: 3,
            state: "CLOSED".into(),
        };
        let context = FileReadContext {
            scope,
            snapshot: Uuid::new_v4(),
            content_id: "a".repeat(64),
            merge_base: sha('1'),
            head: sha('2'),
            change: Change {
                id: "b".repeat(64),
                kind: Kind::Document,
                status: Status::Renamed,
                matched_by: Some(MatchMethod::DocumentId),
                before: Some(side("docs/old.md", before)),
                after: Some(side("docs/new.md", after)),
                content_changed: before != after,
                mode_changed: false,
                catalog_position_changed: false,
            },
        };
        let provider = Arc::new(Provider {
            context: context.clone(),
            sources: Mutex::new((before.into(), after.into())),
            reads: AtomicUsize::new(0),
            failure: Mutex::new(None),
            gate: tokio::sync::Notify::new(),
            block: std::sync::atomic::AtomicBool::new(false),
        });
        let authority = Arc::new(Authority {
            context: Mutex::new(context),
            calls: AtomicUsize::new(0),
            fail_at: AtomicUsize::new(0),
            change_at: AtomicUsize::new(0),
        });
        let factory = Arc::new(Factory {
            provider,
            connects: AtomicUsize::new(0),
        });
        let reader = Arc::new(
            ComparisonFileReader::new(authority.clone(), factory.clone(), Duration::from_secs(60))
                .unwrap(),
        );
        Self {
            authority,
            factory,
            reader,
        }
    }
    async fn load(&self) -> Result<LoadedFile, FileReadError> {
        let context = self.authority.context.lock().unwrap().clone();
        self.reader
            .load(
                context.scope.actor,
                context.scope.proposal,
                context.snapshot,
                &context.change.id,
            )
            .await
    }
}
#[tokio::test]
async fn renamed_historical_file_reads_only_the_published_pair_and_preserves_exact_source() {
    let f = Fixture::new(
        "---\r\nbroken yaml\r\n---\r\nשלום 👩🏽‍💻\r\n<script>x</script>",
        "---\nnew\n---\nשָׁלוֹם\n",
    );
    let loaded = f.load().await.unwrap();
    assert_eq!(
        loaded.before(),
        Some("---\r\nbroken yaml\r\n---\r\nשלום 👩🏽‍💻\r\n<script>x</script>")
    );
    assert_eq!(loaded.after(), Some("---\nnew\n---\nשָׁלוֹם\n"));
    assert_eq!(loaded.merge_base(), &sha('1'));
    assert_eq!(loaded.head(), &sha('2'));
    assert_eq!(
        loaded.diff.old_sha256,
        loaded
            .change()
            .before
            .as_ref()
            .unwrap()
            .sha256
            .clone()
            .unwrap()
    );
    assert_eq!(f.factory.provider.reads.load(Ordering::SeqCst), 2);
    // Later rendering can explicitly recheck; revocation must discard its result.
    f.authority.fail_at.store(
        f.authority.calls.load(Ordering::SeqCst) + 1,
        Ordering::SeqCst,
    );
    assert_eq!(
        f.reader.recheck(loaded.context.scope.actor, &loaded).await,
        Err(FileReadError::NotFound)
    );
}
#[tokio::test]
async fn additions_deletions_and_present_empty_files_remain_distinct() {
    for (before, after) in [(false, true), (true, false), (true, true)] {
        let f = Fixture::new("", "");
        {
            let mut c = f.authority.context.lock().unwrap();
            if !before {
                c.change.before = None;
                c.change.status = Status::Added;
            }
            if !after {
                c.change.after = None;
                c.change.status = Status::Deleted;
            }
        }
        let loaded = f.load().await.unwrap();
        assert_eq!(loaded.before(), before.then_some(""));
        assert_eq!(loaded.after(), after.then_some(""));
        assert!(loaded.diff.chunks.is_empty());
        assert_eq!(
            f.factory.provider.reads.load(Ordering::SeqCst),
            usize::from(before) + usize::from(after)
        );
    }
}
#[tokio::test]
async fn forged_provider_bytes_or_stored_hashes_never_produce_a_diff() {
    for which in 0..4 {
        let f = Fixture::new("before", "after");
        match which {
            0 => f.factory.provider.sources.lock().unwrap().0 = "forged".into(), // equal byte length
            1 => f.factory.provider.sources.lock().unwrap().1 = "longer forged".into(),
            2 => {
                f.authority
                    .context
                    .lock()
                    .unwrap()
                    .change
                    .before
                    .as_mut()
                    .unwrap()
                    .sha256 = Some("0".repeat(64))
            }
            _ => {
                f.authority
                    .context
                    .lock()
                    .unwrap()
                    .change
                    .after
                    .as_mut()
                    .unwrap()
                    .blob_oid = "0".repeat(40)
            }
        }
        assert!(matches!(
            f.load().await,
            Err(FileReadError::InvalidEvidence)
        ));
    }
}
#[tokio::test]
async fn invalid_or_unsupported_evidence_fails_before_connecting() {
    for which in 0..6 {
        let f = Fixture::new("before", "after");
        {
            let mut c = f.authority.context.lock().unwrap();
            match which {
                0 => c.change.before.as_mut().unwrap().path = "docs/../secret.md".into(),
                1 => c.change.after.as_mut().unwrap().bytes = diff::MAX_FILE_BYTES as u64 + 1,
                2 => c.change.kind = Kind::Asset,
                3 => c.change.after.as_mut().unwrap().sha256 = None,
                4 => {
                    c.change.before = None;
                    c.change.after = None;
                }
                _ => c.content_id = "garbage".into(),
            }
        }
        assert!(f.load().await.is_err());
        assert_eq!(f.factory.connects.load(Ordering::SeqCst), 0);
        assert_eq!(f.factory.provider.reads.load(Ordering::SeqCst), 0);
    }
}
#[tokio::test]
async fn authorization_changes_at_every_async_boundary_prevent_source_publication() {
    // Initial scope, bound connection, old fetch, new fetch, CPU completion.
    for boundary in 1..=5 {
        for change_context in [false, true] {
            let f = Fixture::new("before", "after");
            if change_context {
                // A new context at initial resolve is allowed; subsequent changes are not.
                if boundary == 1 {
                    continue;
                }
                f.authority.change_at.store(boundary, Ordering::SeqCst);
            } else {
                f.authority.fail_at.store(boundary, Ordering::SeqCst);
            }
            assert!(f.load().await.is_err(), "boundary {boundary}");
        }
    }
}
#[tokio::test]
async fn provider_backoff_is_explicit_and_never_automatically_retried() {
    let f = Fixture::new("before", "after");
    *f.factory.provider.failure.lock().unwrap() = Some(GitError::RateLimited {
        retry_after_seconds: Some(13),
    });
    assert!(matches!(
        f.load().await,
        Err(FileReadError::Provider(GitError::RateLimited {
            retry_after_seconds: Some(13)
        }))
    ));
    assert_eq!(f.factory.provider.reads.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn capacity_is_bounded_and_cancellation_releases_io_without_retries() {
    let f = Arc::new(Fixture::new("before", "after"));
    f.factory.provider.block.store(true, Ordering::SeqCst);
    let a = {
        let f = f.clone();
        tokio::spawn(async move { f.load().await })
    };
    let b = {
        let f = f.clone();
        tokio::spawn(async move { f.load().await })
    };
    while f.factory.provider.reads.load(Ordering::SeqCst) < 2 {
        tokio::task::yield_now().await;
    }
    assert!(matches!(f.load().await, Err(FileReadError::Busy)));
    a.abort();
    b.abort();
    assert!(matches!(a.await, Err(error) if error.is_cancelled()));
    assert!(matches!(b.await, Err(error) if error.is_cancelled()));
    f.factory.provider.block.store(false, Ordering::SeqCst);
    assert!(f.load().await.is_ok());
    assert_eq!(f.factory.provider.reads.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn timeout_drops_unfinished_io_and_later_reads_start_with_fresh_authorization() {
    let f = Fixture::new("before", "after");
    let mut reader = ComparisonFileReader::new(
        f.authority.clone(),
        f.factory.clone(),
        Duration::from_secs(30),
    )
    .unwrap();
    // Exercise the production deadline path without waiting thirty seconds.
    reader.timeout = Duration::from_millis(10);
    let context = f.authority.context.lock().unwrap().clone();
    f.factory.provider.block.store(true, Ordering::SeqCst);
    assert!(matches!(
        reader
            .load(
                context.scope.actor,
                context.scope.proposal,
                context.snapshot,
                &context.change.id
            )
            .await,
        Err(FileReadError::TimedOut)
    ));
    assert_eq!(f.factory.provider.reads.load(Ordering::SeqCst), 1);
    f.factory.provider.block.store(false, Ordering::SeqCst);
    reader.timeout = Duration::from_secs(30);
    assert!(
        reader
            .load(
                context.scope.actor,
                context.scope.proposal,
                context.snapshot,
                &context.change.id
            )
            .await
            .is_ok()
    );
    assert_eq!(f.factory.provider.reads.load(Ordering::SeqCst), 3);
}
