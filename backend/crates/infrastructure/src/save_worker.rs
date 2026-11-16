//! Prepare from one pinned snapshot, verify media, durably cross the write
//! boundary, commit once, then prove the exact resulting tree. Recovery observes.
use crate::{
    save_verification::{self, VerificationError},
    saves::{PgSaves, SaveJournalError, SaveLease},
};
use specspace_application::{
    book::BookError,
    reader::BookProviderFactory,
    save::{SaveError, SaveSnapshot, SourceFile},
};
use specspace_document_engine::{catalog::Catalog, media};
use specspace_domain::RepositoryPath;
use specspace_git_adapter::{CommitRequest, CommitSha, FileEntry, FileMode, GitError, GitProvider};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
    time::Duration,
};
use tokio::sync::{Semaphore, watch};
use uuid::Uuid;

#[derive(Debug, PartialEq, Eq)]
pub enum SaveWork {
    Idle,
    Completed(Uuid),
    Deferred(Uuid),
    Failed(Uuid),
    LeaseLost(Uuid),
}
pub struct SaveWorker {
    journal: Arc<PgSaves>,
    source: Arc<dyn BookProviderFactory>,
    worker: Uuid,
    timeout: Duration,
    cpu: Arc<Semaphore>,
}
impl SaveWorker {
    pub fn new(
        journal: Arc<PgSaves>,
        source: Arc<dyn BookProviderFactory>,
        timeout: Duration,
    ) -> Result<Self, SaveJournalError> {
        if !(30..=3600).contains(&timeout.as_secs()) {
            return Err(SaveJournalError::InvalidLease);
        }
        Ok(Self {
            journal,
            source,
            worker: Uuid::new_v4(),
            timeout,
            cpu: Arc::new(Semaphore::new(1)),
        })
    }
    pub async fn run_once(&self) -> Result<SaveWork, SaveJournalError> {
        let Some(lease) = self.journal.claim(self.worker, 60).await? else {
            return Ok(SaveWork::Idle);
        };
        let work = self.save(&lease);
        tokio::pin!(work);
        let deadline = tokio::time::sleep(self.timeout);
        tokio::pin!(deadline);
        let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        heartbeat.tick().await;
        let result = loop {
            tokio::select! {
                result=&mut work=>break result,
                _=heartbeat.tick()=>{
                    if let Err(error)=self.journal.heartbeat(&lease,60).await {break Err(TaskError::Journal(error));}
                    if let Err(error)=self.journal.authorize(&lease).await {break Err(TaskError::Journal(error));}
                },
                _=&mut deadline=>break Err(TaskError::Stop("SAVE_TIMEOUT",Some(backoff(lease.attempts)))),
            }
        };
        match result {
            Ok(()) => Ok(SaveWork::Completed(lease.id)),
            Err(TaskError::Journal(SaveJournalError::LeaseLost)) => {
                Ok(SaveWork::LeaseLost(lease.id))
            }
            Err(TaskError::Journal(SaveJournalError::Unavailable)) => {
                Err(SaveJournalError::Unavailable)
            }
            Err(error) => {
                let details = match &error {
                    TaskError::Plan(SaveError::Validation(report)) => {
                        serde_json::to_value(report).ok()
                    }
                    TaskError::Plan(SaveError::DocumentChanged(document)) => {
                        Some(serde_json::json!({"code":"DOCUMENT_CHANGED","document_id":document}))
                    }
                    TaskError::Plan(SaveError::Invalid(code)) => {
                        Some(serde_json::json!({"code":code}))
                    }
                    _ => None,
                };
                let (code, delay, definite) = classify(error, lease.attempts);
                match self
                    .journal
                    .fail_detailed(&lease, code, delay, definite, details)
                    .await
                {
                    Ok(true) => Ok(SaveWork::Deferred(lease.id)),
                    Ok(false) => Ok(SaveWork::Failed(lease.id)),
                    Err(SaveJournalError::LeaseLost) => Ok(SaveWork::LeaseLost(lease.id)),
                    Err(error) => Err(error),
                }
            }
        }
    }
    async fn save(&self, lease: &SaveLease) -> Result<(), TaskError> {
        self.journal.authorize(lease).await?;
        let provider = self
            .source
            .provider(lease.context().repository())
            .await
            .map_err(TaskError::Source)?;
        self.journal.authorize(lease).await?;
        if lease.needs_reconciliation() {
            let manifest = lease
                .manifest()
                .ok_or(TaskError::Stop("INVALID_SAVE_PLAN", None))?;
            let commit = self.observe(provider.as_ref(), lease).await?;
            let proof = save_verification::verify(provider.as_ref(), lease, manifest, &commit)
                .await
                .map_err(TaskError::Verify)?;
            self.journal.complete(lease, &proof).await?;
            return Ok(());
        }
        let head = lease.context().head()?;
        let branch = lease.context().branch()?;
        if provider.branch_head(&branch).await? != head {
            return Err(TaskError::Stop("SAVE_HEAD_CHANGED", None));
        }
        let tree = provider.list_files(&head).await?;
        let files = checked_tree(tree)?;
        let catalog_path = RepositoryPath::parse(".specspace/catalog.json")
            .map_err(|_| TaskError::Stop("INVALID_CATALOG", None))?;
        let catalog_entry = files
            .get(catalog_path.as_str())
            .ok_or(TaskError::Stop("CATALOG_FILE_MISSING", None))?;
        if catalog_entry.mode != FileMode::Regular || catalog_entry.size > 2 * 1024 * 1024 {
            return Err(TaskError::Stop("INVALID_CATALOG", None));
        }
        let catalog_source = provider.read_text(&head, &catalog_path).await?;
        if catalog_source.len() as u64 != catalog_entry.size
            || save_verification::git_blob_hash(
                catalog_source.as_bytes(),
                catalog_entry.blob_sha.as_str().len(),
            )
            .map_err(TaskError::Verify)?
                != catalog_entry.blob_sha
        {
            return Err(TaskError::Stop("CATALOG_BLOB_MISMATCH", None));
        }
        let catalog: Catalog = serde_json::from_str(&catalog_source)
            .map_err(|_| TaskError::Stop("INVALID_CATALOG", None))?;
        if catalog.documents.len() > 1000 {
            return Err(TaskError::Stop("BOOK_TOO_LARGE", None));
        }
        let mut text: Vec<_> = files
            .values()
            .filter(|file| {
                file.path.as_str().starts_with("docs/") && file.path.as_str().ends_with(".md")
            })
            .collect();
        text.sort_by(|a, b| a.path.as_str().cmp(b.path.as_str()));
        if text.len() > 1000
            || text
                .iter()
                .try_fold(0u64, |total, file| total.checked_add(file.size))
                .is_none_or(|total| total > 20 * 1024 * 1024)
        {
            return Err(TaskError::Stop("BOOK_TOO_LARGE", None));
        }
        let mut documents = vec![];
        let mut bytes = 0usize;
        for file in text {
            if file.size > 2 * 1024 * 1024 {
                return Err(TaskError::Stop("FILE_TOO_LARGE", None));
            }
            let markdown = provider.read_text(&head, &file.path).await?;
            bytes = bytes.saturating_add(markdown.len());
            if bytes > 20 * 1024 * 1024 {
                return Err(TaskError::Stop("BOOK_TOO_LARGE", None));
            }
            if markdown.len() as u64 != file.size
                || save_verification::git_blob_hash(
                    markdown.as_bytes(),
                    file.blob_sha.as_str().len(),
                )
                .map_err(TaskError::Verify)?
                    != file.blob_sha
            {
                return Err(TaskError::Stop("SOURCE_BLOB_MISMATCH", None));
            }
            documents.push(SourceFile {
                path: file.path.as_str().into(),
                markdown,
            });
        }
        self.journal.authorize(lease).await?;
        let request = lease
            .request()
            .ok_or(TaskError::Stop("INVALID_SAVE_PLAN", None))?;
        // Rebuild the private immutable intent inside bounded CPU work; no DB
        // transaction is held while parsing, normalization or media decoding runs.
        let actor = request.actor();
        let proposal = request.proposal();
        let repository = request.repository();
        let body = request.request().clone();
        let snapshot = SaveSnapshot {
            repository_id: repository,
            commit: head.clone(),
            catalog,
            files: documents,
            asset_paths: files
                .keys()
                .filter(|path| media::valid_path(path))
                .cloned()
                .collect(),
        };
        let slot = self
            .cpu
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| TaskError::Stop("SAVE_PROCESSING_UNAVAILABLE", Some(2)))?;
        let plan = tokio::task::spawn_blocking(move || {
            let _slot = slot;
            let intent =
                specspace_application::save::ValidatedSave::new(actor, proposal, repository, body)?;
            intent.prepare(snapshot)
        })
        .await
        .map_err(|_| TaskError::Stop("SAVE_PROCESSING_UNAVAILABLE", Some(2)))?
        .map_err(TaskError::Plan)?;
        for document in plan.documents() {
            if files
                .get(document.path.as_str())
                .is_none_or(|file| file.mode != FileMode::Regular)
            {
                return Err(TaskError::Stop("UNSUPPORTED_DOCUMENT_MODE", None));
            }
        }
        for path in &plan.validation().required_assets {
            let entry = files
                .get(path)
                .ok_or(TaskError::Stop("ASSET_MISSING", None))?;
            if entry.mode != FileMode::Regular || entry.size > media::MAX_ASSET_BYTES as u64 {
                return Err(TaskError::Stop("INVALID_ASSET", None));
            }
            let data = provider
                .read_bytes(&head, &entry.path, media::MAX_ASSET_BYTES)
                .await?;
            if data.len() as u64 != entry.size
                || save_verification::git_blob_hash(&data, entry.blob_sha.as_str().len())
                    .map_err(TaskError::Verify)?
                    != entry.blob_sha
            {
                return Err(TaskError::Stop("ASSET_BLOB_MISMATCH", None));
            }
            let path = path.clone();
            let slot = self
                .cpu
                .clone()
                .acquire_owned()
                .await
                .map_err(|_| TaskError::Stop("SAVE_PROCESSING_UNAVAILABLE", Some(2)))?;
            tokio::task::spawn_blocking(move || {
                let _slot = slot;
                media::display(&path, &data).map(|_| ())
            })
            .await
            .map_err(|_| TaskError::Stop("SAVE_PROCESSING_UNAVAILABLE", Some(2)))?
            .map_err(|_| TaskError::Stop("INVALID_ASSET", None))?;
        }
        self.journal.authorize(lease).await?;
        if provider.branch_head(&branch).await? != head {
            return Err(TaskError::Stop("SAVE_HEAD_CHANGED", None));
        }
        if plan.is_noop() {
            self.journal.complete_noop(lease, &plan).await?;
            return Ok(());
        }
        let manifest = self.journal.begin_mutation(lease, &plan).await?;
        let changes = plan.file_changes();
        let commit = provider
            .commit(CommitRequest {
                operation_id: lease.operation(),
                branch: &branch,
                expected_head: &head,
                message: &request.request().message,
                actor_id: actor,
                changes: &changes,
            })
            .await?;
        let proof = save_verification::verify(provider.as_ref(), lease, &manifest, &commit)
            .await
            .map_err(TaskError::Verify)?;
        self.journal.complete(lease, &proof).await?;
        Ok(())
    }
    async fn observe(
        &self,
        provider: &dyn GitProvider,
        lease: &SaveLease,
    ) -> Result<CommitSha, TaskError> {
        let head = lease.context().head()?;
        let branch = lease.context().branch()?;
        let tip = provider.branch_head(&branch).await?;
        if tip == head {
            return Err(TaskError::Stop(
                "SAVE_RESULT_UNKNOWN",
                Some(backoff(lease.attempts)),
            ));
        }
        if !provider.is_ancestor(&head, &tip).await? {
            return Err(TaskError::Stop("SAVE_BRANCH_DIVERGED", None));
        }
        let mut pending = VecDeque::from([tip]);
        let mut seen = HashSet::new();
        while let Some(commit) = pending.pop_front() {
            if commit == head || !seen.insert(commit.clone()) {
                continue;
            }
            if seen.len() > 128 {
                return Err(TaskError::Stop("SAVE_HISTORY_LIMIT", None));
            }
            let receipt = provider.commit_receipt(&commit).await?;
            if receipt.sha != commit || receipt.parents.len() > 16 {
                return Err(TaskError::Stop("INVALID_COMMIT_EVIDENCE", None));
            }
            if receipt.matches_operation(lease.operation(), lease.context().actor(), &head) {
                return Ok(commit);
            }
            for parent in receipt.parents {
                if parent != head && !seen.contains(&parent) {
                    pending.push_back(parent);
                }
            }
            if pending.len() > 2048 {
                return Err(TaskError::Stop("SAVE_HISTORY_LIMIT", None));
            }
        }
        Err(TaskError::Stop("SAVE_RESULT_NOT_FOUND", None))
    }
    pub async fn run(&self, mut stop: watch::Receiver<bool>) {
        loop {
            if *stop.borrow() {
                break;
            }
            let result =
                tokio::select! {result=self.run_once()=>result,_=stop.wait_for(|stop|*stop)=>break};
            match result {
                Ok(SaveWork::Idle) => {}
                Ok(result) => tracing::info!(?result, "proposal save iteration completed"),
                Err(error) => tracing::warn!(?error, "proposal save iteration failed"),
            }
            tokio::select! {_=tokio::time::sleep(Duration::from_secs(2))=>{},_=stop.wait_for(|stop|*stop)=>break}
        }
    }
}
fn checked_tree(entries: Vec<FileEntry>) -> Result<HashMap<String, FileEntry>, TaskError> {
    if entries.len() > 20000 {
        return Err(TaskError::Stop("REPOSITORY_TOO_LARGE", None));
    }
    let mut result = HashMap::new();
    let mut paths = HashSet::new();
    for entry in entries {
        if !paths.insert(entry.path.as_str().to_lowercase())
            || result.insert(entry.path.as_str().into(), entry).is_some()
        {
            return Err(TaskError::Stop("INVALID_SOURCE_MANIFEST", None));
        }
    }
    Ok(result)
}
enum TaskError {
    Journal(SaveJournalError),
    Source(BookError),
    Provider(GitError),
    Plan(SaveError),
    Verify(VerificationError),
    Stop(&'static str, Option<u32>),
}
impl From<SaveJournalError> for TaskError {
    fn from(error: SaveJournalError) -> Self {
        Self::Journal(error)
    }
}
impl From<GitError> for TaskError {
    fn from(error: GitError) -> Self {
        Self::Provider(error)
    }
}
fn backoff(attempts: i32) -> u32 {
    2u32.saturating_pow(attempts.clamp(1, 6) as u32).min(60)
}
fn delay(seconds: Option<u64>, attempts: i32) -> Option<u32> {
    u32::try_from(seconds.unwrap_or(u64::from(backoff(attempts))).max(1))
        .ok()
        .filter(|seconds| *seconds <= 86400)
}
fn classify(error: TaskError, attempts: i32) -> (&'static str, Option<u32>, bool) {
    match error {
        TaskError::Stop(code, retry) => (code, retry, false),
        TaskError::Journal(SaveJournalError::Forbidden | SaveJournalError::NotFound) => {
            ("SAVE_PERMISSION_REVOKED", None, false)
        }
        TaskError::Journal(_) => ("SAVE_CONTEXT_CHANGED", None, false),
        TaskError::Plan(SaveError::Validation(_)) => ("SAVE_VALIDATION_FAILED", None, true),
        TaskError::Plan(_) => ("INVALID_SAVE_PLAN", None, true),
        TaskError::Source(
            BookError::RepositoryNotConfigured | BookError::InvalidRepositoryConfiguration,
        ) => ("REPOSITORY_NOT_CONFIGURED", None, true),
        TaskError::Verify(VerificationError::Provider(error)) => {
            let seconds = match error {
                GitError::RateLimited {
                    retry_after_seconds,
                } => retry_after_seconds,
                GitError::ResultUnknownWithRetryAfter {
                    retry_after_seconds,
                } => Some(retry_after_seconds),
                _ => None,
            };
            ("SAVE_VERIFICATION_PENDING", delay(seconds, attempts), false)
        }
        TaskError::Source(BookError::Provider(error)) | TaskError::Provider(error) => match error {
            GitError::RateLimited {
                retry_after_seconds,
            } => (
                "PROVIDER_RATE_LIMIT",
                delay(retry_after_seconds, attempts),
                true,
            ),
            GitError::ResultUnknownWithRetryAfter {
                retry_after_seconds,
            } => (
                "SAVE_RESULT_UNKNOWN",
                delay(Some(retry_after_seconds), attempts),
                false,
            ),
            GitError::ResultUnknown | GitError::ProviderUnavailable => {
                ("SAVE_RESULT_UNKNOWN", Some(backoff(attempts)), false)
            }
            GitError::HeadMismatch => ("SAVE_HEAD_CHANGED", None, true),
            GitError::Unauthorized
            | GitError::InvalidInput
            | GitError::ProviderRejected
            | GitError::BranchMissing
            | GitError::FileMissing => ("PROVIDER_REJECTED", None, true),
            _ => ("INVALID_PROVIDER_RESPONSE", None, false),
        },
        TaskError::Source(_) => ("SAVE_SOURCE_UNAVAILABLE", Some(backoff(attempts)), true),
        TaskError::Verify(_) => ("SAVE_VERIFICATION_FAILED", None, false),
    }
}
