//! Authorized, bounded loading for a comparison. Proposal content never seeds
//! the main-book cache/index. Provider connections bind frozen numeric settings.
use crate::{
    book::BookError,
    comparison::{ComparisonError, Inventory, Snapshot},
    save::SourceFile,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use specspace_domain::BranchName;
use specspace_git_adapter::{CommitSha, FileEntry, GitError, GitProvider};
use std::{collections::HashSet, sync::Arc, time::Duration};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use uuid::Uuid;

#[derive(Clone, PartialEq, Eq)]
pub struct RepositoryBinding {
    pub repository: Uuid,
    pub remote_id: u64,
    pub installation_id: u64,
}
/// Server-resolved context only. Never deserialize it from an API request.
#[derive(Clone, PartialEq, Eq)]
pub struct ReadContext {
    pub actor: Uuid,
    pub organization: Uuid,
    pub proposal: Uuid,
    pub connection: RepositoryBinding,
    pub branch: BranchName,
    pub recorded_head: CommitSha,
    pub recorded_base: CommitSha,
    pub version: i64,
    pub state: String,
}
#[derive(Debug, PartialEq, Eq)]
pub enum ComparisonReadError {
    NotFound,
    ProposalNotReady,
    ContextChanged,
    InvalidConfiguration,
    InvalidSelection,
    Busy,
    TimedOut,
    Source(BookError),
    Provider(GitError),
    Snapshot(ComparisonError),
    ProcessingUnavailable,
}
#[async_trait]
pub trait ComparisonAuthorization: Send + Sync {
    async fn resolve(
        &self,
        actor: Uuid,
        proposal: Uuid,
    ) -> Result<ReadContext, ComparisonReadError>;
}
#[async_trait]
pub trait BoundProviderFactory: Send + Sync {
    /// Must connect to these exact settings, even if the repository is edited
    /// and restored while connecting. A second unbound lookup is insufficient.
    async fn provider_bound(
        &self,
        binding: &RepositoryBinding,
    ) -> Result<Arc<dyn GitProvider>, ComparisonReadError>;
}
#[derive(Default)]
pub struct Selection {
    pub head: Option<CommitSha>,
    pub target: Option<CommitSha>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ComparisonSummary {
    pub content_id: String,
    pub proposal_id: Uuid,
    pub repository_id: Uuid,
    pub proposal_version: i64,
    pub branch: String,
    pub proposal_state: String,
    pub head_sha: String,
    pub target_sha: String,
    pub merge_base_sha: String,
    pub observed_main_sha: String,
    pub observed_branch_sha: Option<String>,
    pub recorded_head_sha: String,
    pub recorded_base_sha: String,
    pub historical_head: bool,
    pub target_outdated: bool,
    pub base_outdated: bool,
    pub external_branch_change: bool,
    pub before_file_count: usize,
    pub after_file_count: usize,
    pub changes_total: usize,
    pub before_findings: Vec<crate::comparison::Finding>,
    pub before_findings_total: usize,
    pub after_findings: Vec<crate::comparison::Finding>,
    pub after_findings_total: usize,
}
pub struct LoadedComparison {
    id: String,
    context: ReadContext,
    head: CommitSha,
    target: CommitSha,
    current_target: CommitSha,
    merge_base: CommitSha,
    branch_tip: Option<CommitSha>,
    before: Arc<Snapshot>,
    after: Arc<Snapshot>,
    inventory: Inventory,
}
impl LoadedComparison {
    pub fn matches_context(&self, context: &ReadContext) -> bool {
        self.context == *context
    }
    pub fn summary(&self) -> ComparisonSummary {
        ComparisonSummary {
            content_id: self.id.clone(),
            proposal_id: self.proposal(),
            repository_id: self.repository(),
            proposal_version: self.version(),
            branch: self.branch().as_str().into(),
            proposal_state: self.state().into(),
            head_sha: self.head.as_str().into(),
            target_sha: self.target.as_str().into(),
            merge_base_sha: self.merge_base.as_str().into(),
            observed_main_sha: self.current_target.as_str().into(),
            observed_branch_sha: self.branch_tip.as_ref().map(|s| s.as_str().into()),
            recorded_head_sha: self.recorded_head().as_str().into(),
            recorded_base_sha: self.recorded_base().as_str().into(),
            historical_head: self.historical_head(),
            target_outdated: self.target_outdated(),
            base_outdated: self.base_outdated(),
            external_branch_change: self.external_branch_change(),
            before_file_count: self.inventory.before_file_count,
            after_file_count: self.inventory.after_file_count,
            changes_total: self.inventory.changes.len(),
            before_findings: self.before.findings().to_vec(),
            before_findings_total: self.before.findings_total(),
            after_findings: self.after.findings().to_vec(),
            after_findings_total: self.after.findings_total(),
        }
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn actor(&self) -> Uuid {
        self.context.actor
    }
    pub fn organization(&self) -> Uuid {
        self.context.organization
    }
    pub fn binding(&self) -> &RepositoryBinding {
        &self.context.connection
    }
    pub fn proposal(&self) -> Uuid {
        self.context.proposal
    }
    pub fn repository(&self) -> Uuid {
        self.context.connection.repository
    }
    pub fn version(&self) -> i64 {
        self.context.version
    }
    pub fn branch(&self) -> &BranchName {
        &self.context.branch
    }
    pub fn state(&self) -> &str {
        &self.context.state
    }
    pub fn head(&self) -> &CommitSha {
        &self.head
    }
    pub fn recorded_head(&self) -> &CommitSha {
        &self.context.recorded_head
    }
    pub fn recorded_base(&self) -> &CommitSha {
        &self.context.recorded_base
    }
    pub fn target(&self) -> &CommitSha {
        &self.target
    }
    pub fn current_target(&self) -> &CommitSha {
        &self.current_target
    }
    pub fn merge_base(&self) -> &CommitSha {
        &self.merge_base
    }
    pub fn branch_tip(&self) -> Option<&CommitSha> {
        self.branch_tip.as_ref()
    }
    pub fn historical_head(&self) -> bool {
        self.head != self.context.recorded_head
    }
    pub fn target_outdated(&self) -> bool {
        self.target != self.current_target
    }
    pub fn base_outdated(&self) -> bool {
        self.context.recorded_base != self.current_target
    }
    pub fn external_branch_change(&self) -> bool {
        self.branch_tip.as_ref() != Some(&self.context.recorded_head)
    }
    pub fn before(&self) -> &Snapshot {
        &self.before
    }
    pub fn after(&self) -> &Snapshot {
        &self.after
    }
    pub fn inventory(&self) -> &Inventory {
        &self.inventory
    }
}
pub struct ComparisonReader {
    authorization: Arc<dyn ComparisonAuthorization>,
    source: Arc<dyn BoundProviderFactory>,
    loads: Arc<Semaphore>,
    timeout: Duration,
}
impl ComparisonReader {
    pub fn new(
        authorization: Arc<dyn ComparisonAuthorization>,
        source: Arc<dyn BoundProviderFactory>,
        timeout: Duration,
    ) -> Result<Self, ComparisonReadError> {
        if !(Duration::from_secs(30)..=Duration::from_secs(900)).contains(&timeout) {
            return Err(ComparisonReadError::InvalidConfiguration);
        }
        Ok(Self {
            authorization,
            source,
            loads: Arc::new(Semaphore::new(1)),
            timeout,
        })
    }
    pub async fn recheck(
        &self,
        actor: Uuid,
        comparison: &LoadedComparison,
    ) -> Result<(), ComparisonReadError> {
        self.check(actor, &comparison.context).await
    }
    async fn check(&self, actor: Uuid, context: &ReadContext) -> Result<(), ComparisonReadError> {
        let current = self.authorization.resolve(actor, context.proposal).await?;
        if current.actor != actor || current.proposal != context.proposal {
            return Err(ComparisonReadError::NotFound);
        }
        if current != *context {
            return Err(ComparisonReadError::ContextChanged);
        }
        Ok(())
    }
    pub async fn load(
        &self,
        actor: Uuid,
        proposal: Uuid,
        selection: Selection,
    ) -> Result<LoadedComparison, ComparisonReadError> {
        let permit = self
            .loads
            .clone()
            .try_acquire_owned()
            .map_err(|_| ComparisonReadError::Busy)?;
        tokio::time::timeout(
            self.timeout,
            self.load_inner(actor, proposal, selection, permit),
        )
        .await
        .map_err(|_| ComparisonReadError::TimedOut)?
    }
    async fn load_inner(
        &self,
        actor: Uuid,
        proposal: Uuid,
        selection: Selection,
        permit: OwnedSemaphorePermit,
    ) -> Result<LoadedComparison, ComparisonReadError> {
        let context = self.authorization.resolve(actor, proposal).await?;
        if context.actor != actor || context.proposal != proposal {
            return Err(ComparisonReadError::NotFound);
        }
        if context.state == "PROVISIONING" {
            return Err(ComparisonReadError::ProposalNotReady);
        }
        if !matches!(
            context.state.as_str(),
            "DRAFT"
                | "IN_REVIEW"
                | "CHANGES_REQUESTED"
                | "APPROVED"
                | "MERGING"
                | "MERGED"
                | "CLOSED"
        ) {
            return Err(ComparisonReadError::InvalidConfiguration);
        }
        if actor.is_nil()
            || proposal.is_nil()
            || context.organization.is_nil()
            || context.connection.repository.is_nil()
            || context.connection.remote_id == 0
            || context.connection.installation_id == 0
            || context.version < 1
            || !context.branch.as_str().starts_with("change/")
        {
            return Err(ComparisonReadError::InvalidConfiguration);
        }
        let provider = self.source.provider_bound(&context.connection).await?;
        self.check(actor, &context).await?;
        let current_target = provider
            .main_head()
            .await
            .map_err(ComparisonReadError::Provider)?;
        let head = selection
            .head
            .unwrap_or_else(|| context.recorded_head.clone());
        let target = selection.target.unwrap_or_else(|| current_target.clone());
        // Historical selections must belong to this proposal/main lineage.
        // No unproved browser SHA becomes an approved-book target.
        if head != context.recorded_head
            && !provider
                .is_ancestor(&head, &context.recorded_head)
                .await
                .map_err(ComparisonReadError::Provider)?
        {
            return Err(ComparisonReadError::InvalidSelection);
        }
        if target != current_target
            && !provider
                .is_ancestor(&target, &current_target)
                .await
                .map_err(ComparisonReadError::Provider)?
        {
            return Err(ComparisonReadError::InvalidSelection);
        }
        let merge_base = provider
            .merge_base(&target, &head)
            .await
            .map_err(ComparisonReadError::Provider)?;
        self.check(actor, &context).await?;
        let before = self
            .fetch(actor, &context, provider.as_ref(), &merge_base)
            .await?;
        let after = if head == merge_base {
            None
        } else {
            Some(
                self.fetch(actor, &context, provider.as_ref(), &head)
                    .await?,
            )
        };
        self.check(actor, &context).await?;
        let context_copy = context.clone();
        let head_copy = head.clone();
        let base_copy = merge_base.clone();
        let (before, after, inventory, _permit) = tokio::task::spawn_blocking(move || {
            // Own the concurrency permit through CPU work even if the caller
            // times out/disconnects and drops the JoinHandle.
            let before = Arc::new(
                Snapshot::prepare(
                    context_copy.connection.repository,
                    base_copy,
                    before.0,
                    before.1,
                )
                .map_err(ComparisonReadError::Snapshot)?,
            );
            let after = match after {
                Some((files, sources)) => Arc::new(
                    Snapshot::prepare(
                        context_copy.connection.repository,
                        head_copy,
                        files,
                        sources,
                    )
                    .map_err(ComparisonReadError::Snapshot)?,
                ),
                None => before.clone(),
            };
            let inventory =
                Inventory::between(&before, &after).map_err(ComparisonReadError::Snapshot)?;
            Ok::<_, ComparisonReadError>((before, after, inventory, permit))
        })
        .await
        .map_err(|_| ComparisonReadError::ProcessingUnavailable)??;
        self.check(actor, &context).await?;
        let current_target = provider
            .main_head()
            .await
            .map_err(ComparisonReadError::Provider)?;
        if target != current_target
            && !provider
                .is_ancestor(&target, &current_target)
                .await
                .map_err(ComparisonReadError::Provider)?
        {
            return Err(ComparisonReadError::ContextChanged);
        }
        let branch_tip = match provider.branch_head(&context.branch).await {
            Ok(sha) => Some(sha),
            Err(GitError::BranchMissing) => None,
            Err(error) => return Err(ComparisonReadError::Provider(error)),
        };
        self.check(actor, &context).await?;
        let id = format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&(
                    "specspace-comparison-view-v1",
                    context.organization,
                    proposal,
                    context.connection.repository,
                    context.connection.remote_id,
                    context.connection.installation_id,
                    context.version,
                    context.branch.as_str(),
                    context.recorded_head.as_str(),
                    context.recorded_base.as_str(),
                    &context.state,
                    head.as_str(),
                    target.as_str(),
                    merge_base.as_str()
                ))
                .map_err(|_| ComparisonReadError::ProcessingUnavailable)?
            )
        );
        Ok(LoadedComparison {
            id,
            context,
            head,
            target,
            current_target,
            merge_base,
            branch_tip,
            before,
            after,
            inventory,
        })
    }
    async fn fetch(
        &self,
        actor: Uuid,
        context: &ReadContext,
        provider: &dyn GitProvider,
        commit: &CommitSha,
    ) -> Result<(Vec<FileEntry>, Vec<SourceFile>), ComparisonReadError> {
        let mut files = provider
            .list_files(commit)
            .await
            .map_err(ComparisonReadError::Provider)?;
        if files.len() > 20_000 {
            return Err(ComparisonReadError::Snapshot(ComparisonError::TooLarge));
        }
        files.sort_by(|a, b| a.path.as_str().cmp(b.path.as_str()));
        let mut paths = HashSet::new();
        let mut documents = 0;
        let mut declared = 0u64;
        for file in &files {
            let path = file.path.as_str();
            if !paths.insert(path.to_lowercase()) {
                return Err(ComparisonReadError::Snapshot(ComparisonError::InvalidTree));
            }
            if path.starts_with("docs/") && path.ends_with(".md") {
                documents += 1;
                declared = declared.saturating_add(file.size);
                if documents > 1000 || declared > 20 * 1024 * 1024 {
                    return Err(ComparisonReadError::Snapshot(ComparisonError::TooLarge));
                }
            }
            if (path == ".specspace/catalog.json"
                || path.starts_with("docs/") && path.ends_with(".md"))
                && file.size > 2 * 1024 * 1024
            {
                return Err(ComparisonReadError::Snapshot(ComparisonError::TooLarge));
            }
        }
        self.check(actor, context).await?;
        let mut sources = vec![];
        let mut actual = 0usize;
        for file in &files {
            let path = file.path.as_str();
            if path != ".specspace/catalog.json"
                && !(path.starts_with("docs/") && path.ends_with(".md"))
            {
                continue;
            }
            if sources.len() % 8 == 0 {
                self.check(actor, context).await?;
            }
            let markdown = provider
                .read_text(commit, &file.path)
                .await
                .map_err(ComparisonReadError::Provider)?;
            actual = actual.saturating_add(markdown.len());
            if markdown.len() > 2 * 1024 * 1024 || actual > 22 * 1024 * 1024 {
                return Err(ComparisonReadError::Snapshot(ComparisonError::TooLarge));
            }
            if markdown.len() as u64 != file.size {
                return Err(ComparisonReadError::Snapshot(
                    ComparisonError::ContentMismatch,
                ));
            }
            sources.push(SourceFile {
                path: path.into(),
                markdown,
            });
        }
        self.check(actor, context).await?;
        Ok((files, sources))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use specspace_domain::RepositoryPath;
    use specspace_git_adapter::{CommitMetadata, CommitReceipt, CommitRequest, FileMode, blob};
    use std::{
        collections::{BTreeMap, VecDeque},
        sync::{
            Mutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
    };
    use tokio::sync::Notify;
    const OLD: &str = "1111111111111111111111111111111111111111";
    const HEAD: &str = "2222222222222222222222222222222222222222";
    const MAIN: &str = "3333333333333333333333333333333333333333";
    const NEXT: &str = "4444444444444444444444444444444444444444";
    const LATER: &str = "5555555555555555555555555555555555555555";
    fn sha(value: &str) -> CommitSha {
        CommitSha::parse(value).unwrap()
    }
    struct Auth {
        context: Mutex<ReadContext>,
        allowed: AtomicBool,
    }
    #[async_trait]
    impl ComparisonAuthorization for Auth {
        async fn resolve(
            &self,
            actor: Uuid,
            proposal: Uuid,
        ) -> Result<ReadContext, ComparisonReadError> {
            let context = self.context.lock().unwrap().clone();
            if !self.allowed.load(Ordering::SeqCst)
                || actor != context.actor
                || proposal != context.proposal
            {
                return Err(ComparisonReadError::NotFound);
            }
            Ok(context)
        }
    }
    type Hook = Mutex<Option<Box<dyn FnOnce() + Send>>>;
    struct Provider {
        target: Mutex<String>,
        branch: Mutex<Option<String>>,
        graph: BTreeMap<String, Vec<String>>,
        trees: BTreeMap<String, BTreeMap<String, Vec<u8>>>,
        tree_reads: Mutex<Vec<String>>,
        text_reads: Mutex<Vec<(String, String)>>,
        binary_reads: Mutex<Vec<(String, String, usize)>>,
        binary_override: Mutex<Option<Result<Vec<u8>, GitError>>>,
        after_bytes: Hook,
        binary_blocked: AtomicBool,
        binary_started: Notify,
        binary_allowed: AtomicBool,
        after_read: Hook,
        blocked: AtomicBool,
        started: Notify,
        main_reads: AtomicUsize,
        oversized: AtomicBool,
        forged: AtomicBool,
    }
    impl Provider {
        fn ancestors(&self, start: &str) -> Vec<String> {
            let mut seen = HashSet::new();
            let mut result = vec![];
            let mut queue = VecDeque::from([start.to_owned()]);
            while let Some(sha) = queue.pop_front() {
                if seen.insert(sha.clone()) {
                    if let Some(parents) = self.graph.get(&sha) {
                        queue.extend(parents.iter().cloned());
                    }
                    result.push(sha);
                }
            }
            result
        }
    }
    #[async_trait]
    impl GitProvider for Provider {
        async fn main_head(&self) -> Result<CommitSha, GitError> {
            self.main_reads.fetch_add(1, Ordering::SeqCst);
            Ok(sha(&self.target.lock().unwrap()))
        }
        async fn branch_head(&self, branch: &BranchName) -> Result<CommitSha, GitError> {
            assert_eq!(branch.as_str(), "change/diff");
            self.branch
                .lock()
                .unwrap()
                .as_ref()
                .map(|s| sha(s))
                .ok_or(GitError::BranchMissing)
        }
        async fn create_branch(
            &self,
            _: &BranchName,
            _: &CommitSha,
        ) -> Result<CommitSha, GitError> {
            panic!("comparison cannot create branches")
        }
        async fn commit(&self, _: CommitRequest<'_>) -> Result<CommitSha, GitError> {
            panic!("comparison cannot write Git")
        }
        async fn read_text(
            &self,
            commit: &CommitSha,
            path: &RepositoryPath,
        ) -> Result<String, GitError> {
            self.started.notify_one();
            if self.blocked.load(Ordering::SeqCst) {
                std::future::pending::<()>().await;
            }
            self.text_reads
                .lock()
                .unwrap()
                .push((commit.as_str().into(), path.as_str().into()));
            let result =
                String::from_utf8(self.trees[commit.as_str()][path.as_str()].clone()).unwrap();
            if let Some(hook) = self.after_read.lock().unwrap().take() {
                hook();
            }
            if self.forged.load(Ordering::SeqCst) && path.as_str().ends_with(".md") {
                return Ok(result.replace("before", "forged"));
            }
            Ok(result)
        }
        async fn read_bytes(
            &self,
            commit: &CommitSha,
            path: &RepositoryPath,
            maximum: usize,
        ) -> Result<Vec<u8>, GitError> {
            assert!(
                self.binary_allowed.load(Ordering::SeqCst),
                "comparison inventory must not read opaque assets"
            );
            self.binary_reads.lock().unwrap().push((
                commit.as_str().into(),
                path.as_str().into(),
                maximum,
            ));
            self.binary_started.notify_one();
            if self.binary_blocked.load(Ordering::SeqCst) {
                std::future::pending::<()>().await;
            }
            let result = self
                .binary_override
                .lock()
                .unwrap()
                .clone()
                .unwrap_or_else(|| {
                    self.trees
                        .get(commit.as_str())
                        .and_then(|tree| tree.get(path.as_str()))
                        .cloned()
                        .ok_or(GitError::FileMissing)
                });
            let hook = self.after_bytes.lock().unwrap().take();
            if let Some(hook) = hook {
                hook();
            }
            result
        }
        async fn list_files(&self, commit: &CommitSha) -> Result<Vec<FileEntry>, GitError> {
            self.tree_reads.lock().unwrap().push(commit.as_str().into());
            Ok(self.trees[commit.as_str()]
                .iter()
                .map(|(path, bytes)| FileEntry {
                    path: RepositoryPath::parse(path).unwrap(),
                    blob_sha: blob::oid(bytes, 40).unwrap(),
                    size: if self.oversized.load(Ordering::SeqCst) && path.ends_with(".md") {
                        2 * 1024 * 1024 + 1
                    } else {
                        bytes.len() as u64
                    },
                    mode: FileMode::Regular,
                })
                .collect())
        }
        async fn commit_metadata(&self, _: &CommitSha) -> Result<CommitMetadata, GitError> {
            panic!("inventory needs no commit metadata")
        }
        async fn commit_receipt(&self, _: &CommitSha) -> Result<CommitReceipt, GitError> {
            panic!("read is not save reconciliation")
        }
        async fn merge_base(
            &self,
            left: &CommitSha,
            right: &CommitSha,
        ) -> Result<CommitSha, GitError> {
            let right = self.ancestors(right.as_str());
            self.ancestors(left.as_str())
                .into_iter()
                .find(|s| right.contains(s))
                .map(|s| sha(&s))
                .ok_or(GitError::ProviderRejected)
        }
        async fn is_ancestor(
            &self,
            ancestor: &CommitSha,
            descendant: &CommitSha,
        ) -> Result<bool, GitError> {
            Ok(self
                .ancestors(descendant.as_str())
                .iter()
                .any(|s| s == ancestor.as_str()))
        }
    }
    struct Factory {
        provider: Arc<Provider>,
        binding: RepositoryBinding,
        hook: Hook,
        calls: AtomicUsize,
    }
    #[async_trait]
    impl BoundProviderFactory for Factory {
        async fn provider_bound(
            &self,
            binding: &RepositoryBinding,
        ) -> Result<Arc<dyn GitProvider>, ComparisonReadError> {
            assert!(binding == &self.binding);
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(hook) = self.hook.lock().unwrap().take() {
                hook();
            }
            Ok(self.provider.clone())
        }
    }
    struct Fixture {
        auth: Arc<Auth>,
        provider: Arc<Provider>,
        factory: Arc<Factory>,
        reader: Arc<ComparisonReader>,
        actor: Uuid,
        proposal: Uuid,
    }
    fn fixture() -> Fixture {
        fixture_body("proposal edit")
    }
    fn fixture_body(proposal_body: &str) -> Fixture {
        fixture_asset(proposal_body, None)
    }
    fn fixture_asset(proposal_body: &str, asset: Option<(String, Vec<u8>)>) -> Fixture {
        fixture_assets(proposal_body, asset.into_iter().collect())
    }
    fn fixture_assets(proposal_body: &str, assets: Vec<(String, Vec<u8>)>) -> Fixture {
        let actor = Uuid::new_v4();
        let proposal = Uuid::new_v4();
        let binding = RepositoryBinding {
            repository: Uuid::new_v4(),
            remote_id: 42,
            installation_id: 7,
        };
        let auth = Arc::new(Auth {
            context: Mutex::new(ReadContext {
                actor,
                organization: Uuid::new_v4(),
                proposal,
                connection: binding.clone(),
                branch: BranchName::parse("change/diff").unwrap(),
                recorded_head: sha(HEAD),
                recorded_base: sha(OLD),
                version: 3,
                state: "DRAFT".into(),
            }),
            allowed: AtomicBool::new(true),
        });
        let tree = |body: &str| {
            BTreeMap::from([
            (".specspace/catalog.json".into(),br#"{"schema_version":1,"documents":[{"document_id":"DOC-A","path":"docs/a.md"}]}"#.to_vec()),
            ("docs/a.md".into(),format!("---\ndocument_id: DOC-A\ntitle: Synthetic\nowner_team: fixture\nschema_version: 1\n---\n{body}\n").into_bytes()),
            ("assets/opaque.bin".into(),vec![255,0,1]),
        ])
        };
        let mut trees = BTreeMap::from([
            (OLD.into(), tree("before")),
            (HEAD.into(), tree(proposal_body)),
            (MAIN.into(), tree("main-only change")),
        ]);
        for (path, bytes) in assets {
            trees.get_mut(HEAD).unwrap().insert(path, bytes);
        }
        let provider = Arc::new(Provider {
            target: Mutex::new(MAIN.into()),
            branch: Mutex::new(Some(HEAD.into())),
            graph: BTreeMap::from([
                (HEAD.into(), vec![OLD.into()]),
                (MAIN.into(), vec![OLD.into()]),
                (NEXT.into(), vec![MAIN.into()]),
                (LATER.into(), vec![HEAD.into()]),
            ]),
            trees,
            tree_reads: Mutex::new(vec![]),
            text_reads: Mutex::new(vec![]),
            binary_reads: Mutex::new(vec![]),
            binary_override: Mutex::new(None),
            after_bytes: Mutex::new(None),
            binary_blocked: AtomicBool::new(false),
            binary_started: Notify::new(),
            binary_allowed: AtomicBool::new(false),
            after_read: Mutex::new(None),
            blocked: AtomicBool::new(false),
            started: Notify::new(),
            main_reads: AtomicUsize::new(0),
            oversized: AtomicBool::new(false),
            forged: AtomicBool::new(false),
        });
        let factory = Arc::new(Factory {
            provider: provider.clone(),
            binding,
            hook: Mutex::new(None),
            calls: AtomicUsize::new(0),
        });
        let reader = Arc::new(
            ComparisonReader::new(auth.clone(), factory.clone(), Duration::from_secs(900)).unwrap(),
        );
        Fixture {
            auth,
            provider,
            factory,
            reader,
            actor,
            proposal,
        }
    }
    #[tokio::test]
    async fn comparison_uses_merge_base_not_current_main_and_binds_current_scope_without_writes() {
        let f = fixture();
        let view = f
            .reader
            .load(f.actor, f.proposal, Selection::default())
            .await
            .unwrap();
        assert_eq!(view.target().as_str(), MAIN);
        assert_eq!(view.merge_base().as_str(), OLD);
        assert_eq!(view.head().as_str(), HEAD);
        assert_eq!(
            *f.provider.tree_reads.lock().unwrap(),
            vec![OLD.to_owned(), HEAD.to_owned()]
        );
        assert!(
            view.before()
                .source("docs/a.md")
                .unwrap()
                .contains("before")
        );
        assert!(
            view.after()
                .source("docs/a.md")
                .unwrap()
                .contains("proposal edit")
        );
        assert!(view.base_outdated());
        assert!(!view.external_branch_change());
        assert!(!view.historical_head());
        assert_eq!(view.version(), 3);
        assert!(
            view.inventory()
                .changes
                .iter()
                .all(|c| c.kind == crate::comparison::Kind::Document)
        );
        assert_eq!(f.factory.calls.load(Ordering::SeqCst), 1);
        assert!(f.provider.binary_reads.lock().unwrap().is_empty());
        f.reader.recheck(f.actor, &view).await.unwrap();
        f.auth.allowed.store(false, Ordering::SeqCst);
        assert_eq!(
            f.reader.recheck(f.actor, &view).await,
            Err(ComparisonReadError::NotFound)
        );
    }
    #[tokio::test]
    async fn historical_heads_and_targets_require_their_own_proved_lineages_and_empty_diffs_share_one_snapshot()
     {
        let f = fixture();
        let view = f
            .reader
            .load(
                f.actor,
                f.proposal,
                Selection {
                    head: Some(sha(OLD)),
                    target: Some(sha(OLD)),
                },
            )
            .await
            .unwrap();
        assert!(view.historical_head());
        assert!(view.target_outdated());
        assert!(view.inventory().changes.is_empty());
        assert!(Arc::ptr_eq(&view.before, &view.after));
        assert_eq!(f.provider.tree_reads.lock().unwrap().len(), 1);
        for selection in [
            Selection {
                head: Some(sha(MAIN)),
                target: None,
            },
            Selection {
                head: None,
                target: Some(sha(HEAD)),
            },
        ] {
            let f = fixture();
            assert!(matches!(
                f.reader.load(f.actor, f.proposal, selection).await,
                Err(ComparisonReadError::InvalidSelection)
            ));
            assert!(f.provider.tree_reads.lock().unwrap().is_empty());
        }
    }
    #[tokio::test]
    async fn revocation_reconfiguration_and_metadata_changes_during_provider_awaits_prevent_publication()
     {
        for mode in 0..3 {
            let f = fixture();
            let auth = f.auth.clone();
            let hook: Box<dyn FnOnce() + Send> = Box::new(move || match mode {
                0 => auth.allowed.store(false, Ordering::SeqCst),
                1 => auth.context.lock().unwrap().connection.installation_id += 1,
                _ => auth.context.lock().unwrap().version += 1,
            });
            if mode == 1 {
                *f.factory.hook.lock().unwrap() = Some(hook);
            } else {
                *f.provider.after_read.lock().unwrap() = Some(hook);
            }
            let error = f
                .reader
                .load(f.actor, f.proposal, Selection::default())
                .await
                .err()
                .unwrap();
            assert!(matches!(
                error,
                ComparisonReadError::NotFound | ComparisonReadError::ContextChanged
            ));
            if mode == 1 {
                assert_eq!(f.provider.main_reads.load(Ordering::SeqCst), 0);
            }
        }
    }
    #[tokio::test]
    async fn moving_refs_are_reported_without_mixing_sources_and_removed_main_lineage_fails_closed()
    {
        let f = fixture();
        let provider = f.provider.clone();
        *f.provider.after_read.lock().unwrap() = Some(Box::new(move || {
            *provider.target.lock().unwrap() = NEXT.into();
            *provider.branch.lock().unwrap() = Some(LATER.into());
        }));
        let view = f
            .reader
            .load(f.actor, f.proposal, Selection::default())
            .await
            .unwrap();
        assert_eq!(view.target().as_str(), MAIN);
        assert_eq!(view.current_target().as_str(), NEXT);
        assert!(view.target_outdated() && view.external_branch_change());
        assert_eq!(view.head().as_str(), HEAD);
        assert_eq!(
            *f.provider.tree_reads.lock().unwrap(),
            vec![OLD.to_owned(), HEAD.to_owned()]
        );
        let f = fixture();
        let provider = f.provider.clone();
        *f.provider.after_read.lock().unwrap() = Some(Box::new(move || {
            *provider.target.lock().unwrap() = LATER.into()
        }));
        assert!(matches!(
            f.reader
                .load(f.actor, f.proposal, Selection::default())
                .await,
            Err(ComparisonReadError::ContextChanged)
        ));
    }
    #[tokio::test]
    async fn declared_limits_precede_text_reads_and_forged_same_length_bytes_fail_object_verification()
     {
        let f = fixture();
        f.provider.oversized.store(true, Ordering::SeqCst);
        assert!(matches!(
            f.reader
                .load(f.actor, f.proposal, Selection::default())
                .await,
            Err(ComparisonReadError::Snapshot(ComparisonError::TooLarge))
        ));
        assert!(f.provider.text_reads.lock().unwrap().is_empty());
        let f = fixture();
        f.provider.forged.store(true, Ordering::SeqCst);
        assert!(matches!(
            f.reader
                .load(f.actor, f.proposal, Selection::default())
                .await,
            Err(ComparisonReadError::Snapshot(
                ComparisonError::ContentMismatch
            ))
        ));
    }
    #[tokio::test]
    async fn foreign_scope_never_connects_and_deleted_proposal_refs_still_have_explicit_historical_comparison()
     {
        let f = fixture();
        assert!(matches!(
            f.reader
                .load(Uuid::new_v4(), f.proposal, Selection::default())
                .await,
            Err(ComparisonReadError::NotFound)
        ));
        assert_eq!(f.factory.calls.load(Ordering::SeqCst), 0);
        *f.provider.branch.lock().unwrap() = None;
        let view = f
            .reader
            .load(f.actor, f.proposal, Selection::default())
            .await
            .unwrap();
        assert!(view.branch_tip().is_none());
        assert!(view.external_branch_change());
    }
    #[tokio::test]
    async fn cancellation_and_timeout_release_capacity_without_retrying_reads_or_mutations() {
        let f = fixture();
        f.provider.blocked.store(true, Ordering::SeqCst);
        let reader = f.reader.clone();
        let actor = f.actor;
        let proposal = f.proposal;
        let task =
            tokio::spawn(async move { reader.load(actor, proposal, Selection::default()).await });
        f.provider.started.notified().await;
        assert!(matches!(
            f.reader.load(actor, proposal, Selection::default()).await,
            Err(ComparisonReadError::Busy)
        ));
        task.abort();
        assert!(matches!(task.await,Err(error) if error.is_cancelled()));
        f.provider.blocked.store(false, Ordering::SeqCst);
        f.reader
            .load(actor, proposal, Selection::default())
            .await
            .unwrap();
        let mut reader =
            ComparisonReader::new(f.auth.clone(), f.factory.clone(), Duration::from_secs(30))
                .unwrap();
        reader.timeout = Duration::from_millis(10);
        f.provider.blocked.store(true, Ordering::SeqCst);
        assert!(matches!(
            reader.load(actor, proposal, Selection::default()).await,
            Err(ComparisonReadError::TimedOut)
        ));
        f.provider.blocked.store(false, Ordering::SeqCst);
        reader
            .load(actor, proposal, Selection::default())
            .await
            .unwrap();
    }
    #[tokio::test]
    async fn provisioning_or_invalid_context_cannot_connect_even_if_a_head_is_present() {
        for state in ["PROVISIONING", "INVALID"] {
            let f = fixture();
            f.auth.context.lock().unwrap().state = state.into();
            let error = f
                .reader
                .load(f.actor, f.proposal, Selection::default())
                .await
                .err()
                .unwrap();
            assert!(matches!(
                error,
                ComparisonReadError::ProposalNotReady | ComparisonReadError::InvalidConfiguration
            ));
            assert_eq!(f.factory.calls.load(Ordering::SeqCst), 0);
        }
    }
    mod review;
}
