//! Verified display derivatives from one server-resolved snapshot side. No
//! caller path/ref, current-head selection, originals or decoded-image cache.
use super::*;
use serde::{Deserialize, Serialize};
use specspace_document_engine::media::{self, DisplayImage};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum ImageSide {
    Before,
    After,
}
pub struct LoadedImage {
    context: FileReadContext,
    side: ImageSide,
    source_sha256: String,
    display_sha256: String,
    image: DisplayImage,
}
impl LoadedImage {
    pub fn context(&self) -> &FileReadContext {
        &self.context
    }
    pub fn side(&self) -> ImageSide {
        self.side
    }
    pub fn source_sha256(&self) -> &str {
        &self.source_sha256
    }
    pub fn display_sha256(&self) -> &str {
        &self.display_sha256
    }
    pub fn image(&self) -> &DisplayImage {
        &self.image
    }
    pub fn into_image(self) -> DisplayImage {
        self.image
    }
}
impl ComparisonFileReader {
    pub async fn load_image(
        &self,
        actor: Uuid,
        proposal: Uuid,
        snapshot: Uuid,
        file: &str,
        side: ImageSide,
    ) -> Result<LoadedImage, FileReadError> {
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
            self.image_inner(actor, proposal, snapshot, file, side, permit),
        )
        .await
        .map_err(|_| FileReadError::TimedOut)?
    }
    pub async fn recheck_image(
        &self,
        actor: Uuid,
        image: &LoadedImage,
    ) -> Result<(), FileReadError> {
        self.check(actor, &image.context).await
    }
    async fn image_inner(
        &self,
        actor: Uuid,
        proposal: Uuid,
        snapshot: Uuid,
        file: &str,
        side: ImageSide,
        permit: OwnedSemaphorePermit,
    ) -> Result<LoadedImage, FileReadError> {
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
        validate_context(&context)?;
        if context.change.kind != Kind::Asset {
            return Err(FileReadError::UnsupportedKind);
        }
        let source = selected(&context, side).ok_or(FileReadError::NotFound)?;
        if !media::valid_path(&source.path) || source.executable {
            return Err(FileReadError::InvalidImage("INVALID_ASSET_PATH"));
        }
        let path =
            RepositoryPath::parse(&source.path).map_err(|_| FileReadError::InvalidEvidence)?;
        if !matches!(source.blob_oid.len(), 40 | 64)
            || CommitSha::parse(&source.blob_oid).is_err()
            || source.sha256.as_deref().is_some_and(|hash| !digest(hash))
        {
            return Err(FileReadError::InvalidEvidence);
        }
        if source.bytes > media::MAX_ASSET_BYTES as u64 {
            return Err(FileReadError::TooLarge);
        }
        let provider = self
            .source
            .provider_bound(&context.scope.connection)
            .await
            .map_err(FileReadError::Access)?;
        self.check(actor, &context).await?;
        let commit = match side {
            ImageSide::Before => &context.merge_base,
            ImageSide::After => &context.head,
        };
        let bytes = provider
            .read_bytes(commit, &path, media::MAX_ASSET_BYTES)
            .await
            .map_err(FileReadError::Provider)?;
        if bytes.len() > media::MAX_ASSET_BYTES {
            return Err(FileReadError::TooLarge);
        }
        self.check(actor, &context).await?;
        let (loaded, _permit) = tokio::task::spawn_blocking(move || {
            let held = permit;
            let source = selected(&context, side).ok_or(FileReadError::InvalidEvidence)?;
            if bytes.len() as u64 != source.bytes
                || blob::oid(&bytes, source.blob_oid.len())
                    .ok()
                    .as_ref()
                    .map(CommitSha::as_str)
                    != Some(source.blob_oid.as_str())
            {
                return Err(FileReadError::InvalidEvidence);
            }
            let source_sha256 = format!("{:x}", Sha256::digest(&bytes));
            if source
                .sha256
                .as_ref()
                .is_some_and(|hash| hash != &source_sha256)
            {
                return Err(FileReadError::InvalidEvidence);
            }
            let image =
                media::display(&source.path, &bytes).map_err(FileReadError::InvalidImage)?;
            let display_sha256 = format!("{:x}", Sha256::digest(&image.png));
            Ok::<_, FileReadError>((
                LoadedImage {
                    context,
                    side,
                    source_sha256,
                    display_sha256,
                    image,
                },
                held,
            ))
        })
        .await
        .map_err(|_| FileReadError::Unavailable)??;
        self.recheck_image(actor, &loaded).await?;
        Ok(loaded)
    }
}
fn selected(context: &FileReadContext, side: ImageSide) -> Option<&Side> {
    match side {
        ImageSide::Before => context.change.before.as_ref(),
        ImageSide::After => context.change.after.as_ref(),
    }
}
