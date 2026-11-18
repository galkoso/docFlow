//! One after-image from the sealed review media manifest, including referenced
//! assets absent from the changed-file inventory. The internal one-sided source
//! descriptor is never exposed as a claimed addition to the comparison.
use super::*;
use sha2::{Digest, Sha256};
use specspace_application::comparison::{Side, Status};
use specspace_document_engine::media;

pub struct PgReviewMediaAuthorization {
    pool: PgPool,
    path: String,
    file_id: String,
}
impl PgReviewMediaAuthorization {
    pub fn new(pool: PgPool, path: String) -> Result<Self, FileReadError> {
        let filename = path
            .strip_prefix("assets/")
            .ok_or(FileReadError::NotFound)?;
        let (hash, extension) = filename.rsplit_once('.').ok_or(FileReadError::NotFound)?;
        if !digest(hash)
            || !matches!(extension, "png" | "jpg" | "jpeg" | "webp")
            || !media::valid_path(&path)
        {
            return Err(FileReadError::NotFound);
        }
        let file_id = format!(
            "{:x}",
            Sha256::digest(format!("specspace-review-media-v1\0{path}"))
        );
        Ok(Self {
            pool,
            path,
            file_id,
        })
    }
    /// Domain-separated media identity, not a changed-file identity.
    pub fn file_id(&self) -> &str {
        &self.file_id
    }
}
#[derive(FromRow)]
struct MediaEvidence {
    #[sqlx(flatten)]
    binding: FrozenBinding,
    receipt: Value,
}
#[async_trait]
impl ComparisonFileAuthorization for PgReviewMediaAuthorization {
    async fn resolve_file(
        &self,
        actor: Uuid,
        proposal: Uuid,
        snapshot: Uuid,
        file: &str,
    ) -> Result<FileReadContext, FileReadError> {
        if [actor, proposal, snapshot].iter().any(Uuid::is_nil) || file != self.file_id {
            return Err(FileReadError::NotFound);
        }
        let unavailable = |_: sqlx::Error| FileReadError::Unavailable;
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        sqlx::query("SET LOCAL lock_timeout='5s'")
            .execute(&mut *tx)
            .await
            .map_err(unavailable)?;
        sqlx::query("SET LOCAL statement_timeout='10s'")
            .execute(&mut *tx)
            .await
            .map_err(unavailable)?;
        let scope = comparison_authorization::context_in(&mut tx, actor, proposal, true)
            .await
            .map_err(FileReadError::Access)?;
        let row = sqlx::query_as::<_, MediaEvidence>("SELECT h.repository_id,h.remote_id,h.installation_id,h.branch,h.head_sha,h.merge_base_sha,h.metadata#>>'{summary,comparison_content_id}' AS content_id,h.media_validation_version,m.receipt FROM review_snapshots h JOIN review_snapshot_media m ON m.snapshot_id=h.id AND m.organization_id=h.organization_id AND m.proposal_id=h.proposal_id WHERE h.id=$1 AND h.proposal_id=$2 AND h.organization_id=$3 AND h.state='READY' AND m.path=$4")
            .bind(snapshot).bind(proposal).bind(scope.organization).bind(&self.path).fetch_optional(&mut *tx).await.map_err(unavailable)?.ok_or(FileReadError::NotFound)?;
        let receipt: ReviewMediaReceipt =
            serde_json::from_value(row.receipt).map_err(|_| FileReadError::InvalidEvidence)?;
        if receipt.path != self.path
            || !digest(&receipt.source_sha256)
            || receipt.source_sha256 != self.path[7..71]
            || !digest(&receipt.display_sha256)
            || receipt.bytes == 0
            || receipt.bytes > media::MAX_ASSET_BYTES as u64
            || row.binding.media_validation_version < 1
            || !(1..=2048).contains(&receipt.display_width)
            || !(1..=2048).contains(&receipt.display_height)
        {
            return Err(FileReadError::InvalidEvidence);
        }
        let proof = PublishedImageEvidence {
            validation_version: row.binding.media_validation_version as u32,
            source_sha256: receipt.source_sha256,
            display_sha256: receipt.display_sha256,
            width: receipt.display_width,
            height: receipt.display_height,
        };
        let change = Change {
            id: self.file_id.clone(),
            kind: Kind::Asset,
            status: Status::Added,
            matched_by: None,
            before: None,
            after: Some(Side {
                path: receipt.path,
                blob_oid: receipt.blob_oid,
                bytes: receipt.bytes,
                executable: false,
                sha256: Some(proof.source_sha256.clone()),
                document_id: None,
                catalog_position: None,
            }),
            content_changed: true,
            mode_changed: false,
            catalog_position_changed: false,
        };
        let context = frozen_context(scope, row.binding, snapshot, change, Some(proof))?;
        tx.commit().await.map_err(unavailable)?;
        Ok(context)
    }
}
