//! Private, source-verified file reads from a published comparison. Browser paths
//! and refs never enter provider calls; only server-resolved immutable evidence.
mod cache;

use crate::{
    comparison::{Change, Kind, Side},
    comparison_reader::{BoundProviderFactory, ComparisonReadError, ReadContext},
};
use async_trait::async_trait;
use sha2::{Digest, Sha256};
use specspace_document_engine::diff::{self, TextDiff};
use specspace_domain::RepositoryPath;
use specspace_git_adapter::{CommitSha, GitError, GitProvider, blob};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use uuid::Uuid;

#[derive(Debug, PartialEq, Eq)]
pub enum FileReadError {
    Access(ComparisonReadError),
    NotFound,
    NotReady,
    InvalidEvidence,
    UnsupportedKind,
    TooLarge,
    Busy,
    TimedOut,
    Provider(GitError),
    Unavailable,
}
/// Server-owned evidence. Deliberately has no Deserialize/Serialize/Debug.
#[derive(Clone, PartialEq, Eq)]
pub struct FileReadContext {
    pub scope: ReadContext,
    pub snapshot: Uuid,
    pub content_id: String,
    pub merge_base: CommitSha,
    pub head: CommitSha,
    pub change: Change,
}
#[async_trait]
pub trait ComparisonFileAuthorization: Send + Sync {
    async fn resolve_file(
        &self,
        actor: Uuid,
        proposal: Uuid,
        snapshot: Uuid,
        file: &str,
    ) -> Result<FileReadContext, FileReadError>;
}
/// Exact source remains private until an authorized caller materializes a bounded
/// response. An absent side and a present empty file are intentionally distinct.
pub struct LoadedFile {
    context: FileReadContext,
    text: Arc<PreparedText>,
}
struct PreparedText {
    before: Option<String>,
    after: Option<String>,
    diff: TextDiff,
}
impl LoadedFile {
    pub fn snapshot(&self) -> Uuid {
        self.context.snapshot
    }
    pub fn content_id(&self) -> &str {
        &self.context.content_id
    }
    pub fn change(&self) -> &Change {
        &self.context.change
    }
    pub fn merge_base(&self) -> &CommitSha {
        &self.context.merge_base
    }
    pub fn head(&self) -> &CommitSha {
        &self.context.head
    }
    pub fn before(&self) -> Option<&str> {
        self.text.before.as_deref()
    }
    pub fn after(&self) -> Option<&str> {
        self.text.after.as_deref()
    }
    pub fn diff(&self) -> &TextDiff {
        &self.text.diff
    }
}
pub struct ComparisonFileReader {
    authorization: Arc<dyn ComparisonFileAuthorization>,
    source: Arc<dyn BoundProviderFactory>,
    loads: Arc<Semaphore>,
    cache: Mutex<cache::FileCache>,
    timeout: Duration,
}
impl ComparisonFileReader {
    pub fn new(
        authorization: Arc<dyn ComparisonFileAuthorization>,
        source: Arc<dyn BoundProviderFactory>,
        timeout: Duration,
    ) -> Result<Self, FileReadError> {
        if !(Duration::from_secs(30)..=Duration::from_secs(120)).contains(&timeout) {
            return Err(FileReadError::InvalidEvidence);
        }
        Ok(Self {
            authorization,
            source,
            loads: Arc::new(Semaphore::new(2)),
            cache: Mutex::new(cache::FileCache::default()),
            timeout,
        })
    }
    pub async fn load(
        &self,
        actor: Uuid,
        proposal: Uuid,
        snapshot: Uuid,
        file: &str,
    ) -> Result<LoadedFile, FileReadError> {
        if actor.is_nil() || proposal.is_nil() || snapshot.is_nil() || !digest(file) {
            return Err(FileReadError::NotFound);
        }
        let permit = self
            .loads
            .clone()
            .try_acquire_owned()
            .map_err(|_| FileReadError::Busy)?;
        tokio::time::timeout(
            self.timeout,
            self.load_inner(actor, proposal, snapshot, file, permit),
        )
        .await
        .map_err(|_| FileReadError::TimedOut)?
    }
    pub async fn recheck(&self, actor: Uuid, loaded: &LoadedFile) -> Result<(), FileReadError> {
        self.check(actor, &loaded.context).await
    }
    async fn check(&self, actor: Uuid, context: &FileReadContext) -> Result<(), FileReadError> {
        let current = self
            .authorization
            .resolve_file(
                actor,
                context.scope.proposal,
                context.snapshot,
                &context.change.id,
            )
            .await?;
        if current.scope.actor != actor || current.scope.proposal != context.scope.proposal {
            return Err(FileReadError::NotFound);
        }
        if current != *context {
            return Err(FileReadError::Access(ComparisonReadError::ContextChanged));
        }
        Ok(())
    }
    async fn load_inner(
        &self,
        actor: Uuid,
        proposal: Uuid,
        snapshot: Uuid,
        file: &str,
        permit: OwnedSemaphorePermit,
    ) -> Result<LoadedFile, FileReadError> {
        let context = self
            .authorization
            .resolve_file(actor, proposal, snapshot, file)
            .await?;
        if context.scope.actor != actor
            || context.scope.proposal != proposal
            || context.snapshot != snapshot
            || context.change.id != file
        {
            return Err(FileReadError::NotFound);
        }
        validate(&context)?;
        let cached = self
            .cache
            .lock()
            .map_err(|_| FileReadError::Unavailable)?
            .get(&context, Instant::now());
        if let Some(text) = cached {
            let loaded = LoadedFile { context, text };
            self.recheck(actor, &loaded).await?;
            return Ok(loaded);
        }
        let provider = self
            .source
            .provider_bound(&context.scope.connection)
            .await
            .map_err(FileReadError::Access)?;
        self.check(actor, &context).await?;
        let before = fetch(
            provider.as_ref(),
            &context.merge_base,
            context.change.before.as_ref(),
        )
        .await?;
        self.check(actor, &context).await?;
        let after = fetch(
            provider.as_ref(),
            &context.head,
            context.change.after.as_ref(),
        )
        .await?;
        self.check(actor, &context).await?;
        // Verification and bounded alignment run off the async executor. The CPU
        // closure owns the permit even if its awaiting request is cancelled.
        let (loaded, _permit) = tokio::task::spawn_blocking(move || {
            let held_permit = permit;
            verify(context.change.before.as_ref(), before.as_deref())?;
            verify(context.change.after.as_ref(), after.as_deref())?;
            let diff = diff::compare(
                before.as_deref().unwrap_or(""),
                after.as_deref().unwrap_or(""),
            )
            .map_err(|_| FileReadError::TooLarge)?;
            Ok::<_, FileReadError>((
                LoadedFile {
                    context,
                    text: Arc::new(PreparedText {
                        before,
                        after,
                        diff,
                    }),
                },
                held_permit,
            ))
        })
        .await
        .map_err(|_| FileReadError::Unavailable)??;
        self.recheck(actor, &loaded).await?;
        self.cache
            .lock()
            .map_err(|_| FileReadError::Unavailable)?
            .insert(&loaded.context, loaded.text.clone(), Instant::now());
        Ok(loaded)
    }
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
fn validate(context: &FileReadContext) -> Result<(), FileReadError> {
    if context.scope.organization.is_nil()
        || context.scope.connection.repository.is_nil()
        || context.scope.connection.remote_id == 0
        || context.scope.connection.installation_id == 0
        || !digest(&context.content_id)
        || context.scope.version < 1
        || context.change.before.is_none() && context.change.after.is_none()
    {
        return Err(FileReadError::InvalidEvidence);
    }
    if !matches!(context.change.kind, Kind::Document | Kind::Catalog) {
        return Err(FileReadError::UnsupportedKind);
    }
    for side in [&context.change.before, &context.change.after]
        .into_iter()
        .flatten()
    {
        RepositoryPath::parse(&side.path).map_err(|_| FileReadError::InvalidEvidence)?;
        let correct_kind = match context.change.kind {
            Kind::Document => side.path.starts_with("docs/") && side.path.ends_with(".md"),
            Kind::Catalog => side.path == ".specspace/catalog.json",
            _ => false,
        };
        if !correct_kind
            || !matches!(side.blob_oid.len(), 40 | 64)
            || CommitSha::parse(&side.blob_oid).is_err()
            || !side.sha256.as_deref().is_some_and(digest)
        {
            return Err(FileReadError::InvalidEvidence);
        }
        if side.bytes > diff::MAX_FILE_BYTES as u64 {
            return Err(FileReadError::TooLarge);
        }
    }
    Ok(())
}
async fn fetch(
    provider: &dyn GitProvider,
    commit: &CommitSha,
    side: Option<&Side>,
) -> Result<Option<String>, FileReadError> {
    let Some(side) = side else { return Ok(None) };
    let path = RepositoryPath::parse(&side.path).map_err(|_| FileReadError::InvalidEvidence)?;
    let source = provider
        .read_text(commit, &path)
        .await
        .map_err(FileReadError::Provider)?;
    if source.len() > diff::MAX_FILE_BYTES {
        return Err(FileReadError::TooLarge);
    }
    Ok(Some(source))
}
fn verify(side: Option<&Side>, source: Option<&str>) -> Result<(), FileReadError> {
    match (side, source) {
        (None, None) => Ok(()),
        (Some(side), Some(source))
            if source.len() as u64 == side.bytes
                && blob::oid(source.as_bytes(), side.blob_oid.len())
                    .ok()
                    .as_ref()
                    .map(CommitSha::as_str)
                    == Some(side.blob_oid.as_str())
                && side.sha256.as_deref()
                    == Some(format!("{:x}", Sha256::digest(source.as_bytes())).as_str()) =>
        {
            Ok(())
        }
        _ => Err(FileReadError::InvalidEvidence),
    }
}

#[cfg(test)]
mod tests;
