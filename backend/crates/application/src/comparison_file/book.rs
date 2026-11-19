//! Complete-book source authority is separate from the changed-file namespace.
//! Unchanged documents never become synthetic diff entries or provider paths
//! chosen by a browser. Proofs retain their exact current and historical scope.
use super::*;
use crate::review_snapshot::ReviewBookDocument;
use discussion::{AnchorReadError, TextSelection};
use specspace_document_engine::discussion_anchor::TextAnchor;

#[derive(Clone, PartialEq, Eq)]
pub struct BookDocumentContext {
    pub scope: ReadContext,
    pub snapshot: Uuid,
    pub fingerprint: String,
    pub head: CommitSha,
    pub document: ReviewBookDocument,
}

#[async_trait]
pub trait ReviewBookAuthorization: Send + Sync {
    async fn resolve_document(
        &self,
        actor: Uuid,
        proposal: Uuid,
        snapshot: Uuid,
        entry: &str,
    ) -> Result<BookDocumentContext, FileReadError>;
}

/// No deserialization, serialization or public constructor. A stored manifest
/// or a selected document ID alone cannot construct this source capability.
pub struct VerifiedBookAnchor {
    context: BookDocumentContext,
    text: TextAnchor,
}
impl VerifiedBookAnchor {
    pub fn context(&self) -> &BookDocumentContext {
        &self.context
    }
    pub fn source(&self) -> &Side {
        &self.context.document.source
    }
    pub fn text(&self) -> &TextAnchor {
        &self.text
    }
    pub fn commit(&self) -> &CommitSha {
        &self.context.head
    }
}

pub struct ReviewBookReader {
    authorization: Arc<dyn ReviewBookAuthorization>,
    source: Arc<dyn BoundProviderFactory>,
    loads: Arc<Semaphore>,
    timeout: Duration,
}
impl ComparisonFileReader {
    /// Shares the process-wide source capacity and exact provider factory while
    /// keeping complete-book authorization separate from changed-file reads.
    pub fn with_book_authorization(
        &self,
        authorization: Arc<dyn ReviewBookAuthorization>,
    ) -> ReviewBookReader {
        ReviewBookReader {
            authorization,
            source: self.source.clone(),
            loads: self.loads.clone(),
            timeout: self.timeout,
        }
    }
}
impl ReviewBookReader {
    pub async fn load_anchor(
        &self,
        actor: Uuid,
        proposal: Uuid,
        snapshot: Uuid,
        entry: &str,
        selection: TextSelection,
    ) -> Result<VerifiedBookAnchor, AnchorReadError> {
        selection.validate()?;
        if [actor, proposal, snapshot].iter().any(Uuid::is_nil) || !digest(entry) {
            return Err(FileReadError::NotFound.into());
        }
        let permit = self
            .loads
            .clone()
            .try_acquire_owned()
            .map_err(|_| FileReadError::Busy)?;
        tokio::time::timeout(self.timeout, async {
            let context = self
                .authorization
                .resolve_document(actor, proposal, snapshot, entry)
                .await?;
            if context.scope.actor != actor
                || context.scope.proposal != proposal
                || context.snapshot != snapshot
                || context.document.id != entry
            {
                return Err(FileReadError::NotFound.into());
            }
            validate(&context)?;
            let provider = self
                .source
                .provider_bound(&context.scope.connection)
                .await
                .map_err(FileReadError::Access)?;
            self.check(actor, &context).await?;
            let source = fetch(
                provider.as_ref(),
                &context.head,
                Some(&context.document.source),
            )
            .await?
            .ok_or(FileReadError::InvalidEvidence)?;
            self.check(actor, &context).await?;
            let proof = tokio::task::spawn_blocking(move || {
                let _permit = permit;
                verify(Some(&context.document.source), Some(&source))?;
                let document_id = context
                    .document
                    .source
                    .document_id
                    .as_deref()
                    .ok_or(FileReadError::InvalidEvidence)?;
                let text = match selection {
                    TextSelection::Document {} => TextAnchor::document(&source, document_id)?,
                    TextSelection::Section { section_id } => {
                        TextAnchor::section(&source, document_id, &section_id)?
                    }
                    TextSelection::Lines {
                        start_line,
                        end_line,
                    } => {
                        // Line selection also verifies the manifest's stable
                        // document identity against actual front matter.
                        TextAnchor::document(&source, document_id)?;
                        TextAnchor::lines(&source, start_line, end_line)?
                    }
                };
                Ok::<_, AnchorReadError>(VerifiedBookAnchor { context, text })
            })
            .await
            .map_err(|_| FileReadError::Unavailable)??;
            self.recheck(actor, &proof).await?;
            Ok(proof)
        })
        .await
        .map_err(|_| FileReadError::TimedOut)?
    }
    /// Call after any later await; the mutation transaction must additionally
    /// lock and recheck current write permission and the captured admission.
    pub async fn recheck(
        &self,
        actor: Uuid,
        proof: &VerifiedBookAnchor,
    ) -> Result<(), FileReadError> {
        self.check(actor, &proof.context).await
    }
    async fn check(&self, actor: Uuid, context: &BookDocumentContext) -> Result<(), FileReadError> {
        if actor != context.scope.actor {
            return Err(FileReadError::NotFound);
        }
        let current = self
            .authorization
            .resolve_document(
                actor,
                context.scope.proposal,
                context.snapshot,
                &context.document.id,
            )
            .await?;
        if current != *context {
            return Err(FileReadError::Access(ComparisonReadError::ContextChanged));
        }
        Ok(())
    }
}
fn validate(context: &BookDocumentContext) -> Result<(), FileReadError> {
    let scope = &context.scope;
    let side = &context.document.source;
    if scope.organization.is_nil()
        || scope.connection.repository.is_nil()
        || scope.connection.remote_id == 0
        || scope.connection.installation_id == 0
        || scope.version < 1
        || !digest(&context.fingerprint)
        || !side.path.starts_with("docs/")
        || !side.path.ends_with(".md")
        || RepositoryPath::parse(&side.path).is_err()
        || side.executable
        || !matches!(side.blob_oid.len(), 40 | 64)
        || CommitSha::parse(&side.blob_oid).is_err()
        || !side.sha256.as_deref().is_some_and(digest)
        || !side
            .document_id
            .as_deref()
            .is_some_and(crate::draft::valid_document_id)
        || !side
            .catalog_position
            .is_some_and(|position| position < 1000)
        || ReviewBookDocument::identity(scope.connection.repository, &context.head, side)
            .map_err(|_| FileReadError::InvalidEvidence)?
            != context.document.id
    {
        return Err(FileReadError::InvalidEvidence);
    }
    if side.bytes > diff::MAX_FILE_BYTES as u64 {
        return Err(FileReadError::TooLarge);
    }
    Ok(())
}
