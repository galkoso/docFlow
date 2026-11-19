//! Source-verified windows of sealed mandatory work. A proof says which bytes
//! were materialized, not that a human viewed or approved them. It cannot be
//! deserialized, and must be rechecked after later awaits before persistence.
use super::*;
use crate::review_units::{RequiredReviewUnit, UnitSource, verify_source_unit};
use discussion::AnchorSide;
use specspace_document_engine::review_sections::SourceRange;

pub const MAX_WINDOW_BYTES: usize = 64 * 1024;
#[derive(Clone, PartialEq, Eq)]
pub struct ReviewUnitContext {
    pub scope: ReadContext,
    pub snapshot: Uuid,
    pub fingerprint: String,
    pub head: CommitSha,
    pub merge_base: CommitSha,
    pub unit: RequiredReviewUnit,
}
#[async_trait]
pub trait ReviewUnitAuthorization: Send + Sync {
    async fn resolve_unit(
        &self,
        actor: Uuid,
        proposal: Uuid,
        snapshot: Uuid,
        unit: &str,
    ) -> Result<ReviewUnitContext, FileReadError>;
}
pub struct UnitWindowRequest<'a> {
    pub unit: &'a str,
    pub side: AnchorSide,
    /// Byte offset relative to the required source span, never to the file.
    pub offset: usize,
}
#[derive(Debug, PartialEq, Eq)]
pub enum UnitReadError {
    Source(FileReadError),
    InvalidWindow,
}
impl From<FileReadError> for UnitReadError {
    fn from(value: FileReadError) -> Self {
        Self::Source(value)
    }
}
pub struct VerifiedReviewUnitWindow {
    context: ReviewUnitContext,
    file: FileReadContext,
    side: AnchorSide,
    range: SourceRange,
    start_byte: usize,
    end_byte: usize,
    next_offset: Option<usize>,
    text: String,
}
impl VerifiedReviewUnitWindow {
    pub fn context(&self) -> &ReviewUnitContext {
        &self.context
    }
    pub fn side(&self) -> AnchorSide {
        self.side
    }
    pub fn range(&self) -> &SourceRange {
        &self.range
    }
    pub fn start_byte(&self) -> usize {
        self.start_byte
    }
    pub fn end_byte(&self) -> usize {
        self.end_byte
    }
    pub fn next_offset(&self) -> Option<usize> {
        self.next_offset
    }
    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn source(&self) -> &UnitSource {
        match self.side {
            AnchorSide::Before => self.context.unit.before.as_ref(),
            AnchorSide::After => self.context.unit.after.as_ref(),
        }
        .expect("verified window retains its source side")
    }
    pub fn commit(&self) -> &CommitSha {
        match self.side {
            AnchorSide::Before => &self.context.merge_base,
            AnchorSide::After => &self.context.head,
        }
    }
}
pub struct ReviewUnitReader {
    files: Arc<ComparisonFileReader>,
    authorization: Arc<dyn ReviewUnitAuthorization>,
}
impl ReviewUnitReader {
    /// Shares the supplied reader's verified cache, provider binding, deadline
    /// and process-wide source/CPU capacity; it creates no new provider pool.
    pub fn new(
        files: Arc<ComparisonFileReader>,
        authorization: Arc<dyn ReviewUnitAuthorization>,
    ) -> Self {
        Self {
            files,
            authorization,
        }
    }
    pub async fn load(
        &self,
        actor: Uuid,
        proposal: Uuid,
        snapshot: Uuid,
        request: UnitWindowRequest<'_>,
    ) -> Result<VerifiedReviewUnitWindow, UnitReadError> {
        if [actor, proposal, snapshot].iter().any(Uuid::is_nil) || !digest(request.unit) {
            return Err(FileReadError::NotFound.into());
        }
        if request.offset > diff::MAX_FILE_BYTES {
            return Err(UnitReadError::InvalidWindow);
        }
        tokio::time::timeout(self.files.timeout, async {
            let context = self
                .authorization
                .resolve_unit(actor, proposal, snapshot, request.unit)
                .await?;
            if context.scope.actor != actor
                || context.scope.proposal != proposal
                || context.snapshot != snapshot
                || context.unit.id != request.unit
            {
                return Err(FileReadError::NotFound.into());
            }
            if !digest(&context.fingerprint) || !digest(&context.unit.file_id) {
                return Err(FileReadError::InvalidEvidence.into());
            }
            let selected = match request.side {
                AnchorSide::Before => &context.unit.before,
                AnchorSide::After => &context.unit.after,
            };
            let selected = selected.as_ref().ok_or(FileReadError::NotFound)?;
            if selected.range.is_none() {
                return Err(FileReadError::UnsupportedKind.into());
            }
            let loaded = self
                .files
                .load(actor, proposal, snapshot, &context.unit.file_id)
                .await?;
            if loaded.context.scope != context.scope
                || loaded.snapshot() != context.snapshot
                || loaded.head() != &context.head
                || loaded.merge_base() != &context.merge_base
            {
                return Err(FileReadError::Access(ComparisonReadError::ContextChanged).into());
            }
            self.check(actor, &context).await?;
            self.files.recheck(actor, &loaded).await?;
            let permit = self
                .files
                .loads
                .clone()
                .try_acquire_owned()
                .map_err(|_| FileReadError::Busy)?;
            let proof = tokio::task::spawn_blocking(move || {
                let _permit = permit;
                prepare(context, loaded, request.side, request.offset)
            })
            .await
            .map_err(|_| FileReadError::Unavailable)??;
            self.recheck(actor, &proof).await?;
            Ok(proof)
        })
        .await
        .map_err(|_| FileReadError::TimedOut)?
    }
    pub async fn recheck(
        &self,
        actor: Uuid,
        proof: &VerifiedReviewUnitWindow,
    ) -> Result<(), FileReadError> {
        self.check(actor, &proof.context).await?;
        self.files.check(actor, &proof.file).await
    }
    async fn check(&self, actor: Uuid, context: &ReviewUnitContext) -> Result<(), FileReadError> {
        if actor != context.scope.actor {
            return Err(FileReadError::NotFound);
        }
        let current = self
            .authorization
            .resolve_unit(
                actor,
                context.scope.proposal,
                context.snapshot,
                &context.unit.id,
            )
            .await?;
        if current != *context {
            return Err(FileReadError::Access(ComparisonReadError::ContextChanged));
        }
        Ok(())
    }
}
fn prepare(
    context: ReviewUnitContext,
    loaded: LoadedFile,
    side: AnchorSide,
    offset: usize,
) -> Result<VerifiedReviewUnitWindow, UnitReadError> {
    verify_source_unit(
        &context.unit,
        loaded.change(),
        loaded.before(),
        loaded.after(),
    )
    .map_err(|_| FileReadError::InvalidEvidence)?;
    let selected = match side {
        AnchorSide::Before => &context.unit.before,
        AnchorSide::After => &context.unit.after,
    };
    let range = selected
        .as_ref()
        .and_then(|s| s.range.as_ref())
        .ok_or(FileReadError::InvalidEvidence)?
        .clone();
    let source = match side {
        AnchorSide::Before => loaded.before(),
        AnchorSide::After => loaded.after(),
    }
    .ok_or(FileReadError::InvalidEvidence)?;
    let start_byte = range
        .start_byte
        .checked_add(offset)
        .ok_or(UnitReadError::InvalidWindow)?;
    if start_byte > range.end_byte || !source.is_char_boundary(start_byte) {
        return Err(UnitReadError::InvalidWindow);
    }
    let mut end_byte = (start_byte + MAX_WINDOW_BYTES).min(range.end_byte);
    while !source.is_char_boundary(end_byte) {
        end_byte -= 1;
    }
    let text = source
        .get(start_byte..end_byte)
        .ok_or(FileReadError::InvalidEvidence)?
        .to_owned();
    let next_offset = (end_byte < range.end_byte).then_some(end_byte - range.start_byte);
    Ok(VerifiedReviewUnitWindow {
        context,
        file: loaded.context,
        side,
        range,
        start_byte,
        end_byte,
        next_offset,
        text,
    })
}
