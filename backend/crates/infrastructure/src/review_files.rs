//! Shared review body authority. Current organization access and repository
//! binding are re-read; paths, commits and media receipts come only from the
//! sealed shared snapshot, never a browser locator or private comparison job.
use crate::comparison_authorization;
use async_trait::async_trait;
use serde_json::Value;
use specspace_application::{
    comparison::{Change, Kind},
    comparison_file::{
        ComparisonFileAuthorization, FileReadContext, FileReadError, PublishedImageEvidence,
    },
    comparison_reader::ComparisonReadError,
    review_snapshot::ReviewMediaReceipt,
};
use specspace_git_adapter::CommitSha;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

pub struct PgReviewFileAuthorization {
    pool: PgPool,
}
impl PgReviewFileAuthorization {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}
#[derive(FromRow)]
struct Evidence {
    repository_id: Uuid,
    remote_id: String,
    installation_id: String,
    branch: String,
    head_sha: String,
    merge_base_sha: String,
    content_id: Option<String>,
    media_validation_version: i32,
    change: Value,
    receipt: Option<Value>,
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
#[async_trait]
impl ComparisonFileAuthorization for PgReviewFileAuthorization {
    async fn resolve_file(
        &self,
        actor: Uuid,
        proposal: Uuid,
        snapshot: Uuid,
        file: &str,
    ) -> Result<FileReadContext, FileReadError> {
        if [actor, proposal, snapshot].iter().any(Uuid::is_nil) || !digest(file) {
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
        let row=sqlx::query_as::<_,Evidence>("SELECT h.repository_id,h.remote_id,h.installation_id,h.branch,h.head_sha,h.merge_base_sha,h.metadata#>>'{summary,comparison_content_id}' AS content_id,h.media_validation_version,f.change,m.receipt FROM review_snapshots h JOIN review_snapshot_files f ON f.snapshot_id=h.id AND f.organization_id=h.organization_id AND f.proposal_id=h.proposal_id LEFT JOIN review_snapshot_media m ON m.snapshot_id=h.id AND m.organization_id=h.organization_id AND m.proposal_id=h.proposal_id AND m.path=f.change#>>'{after,path}' WHERE h.id=$1 AND h.proposal_id=$2 AND h.organization_id=$3 AND h.state='READY' AND f.file_id=$4")
            .bind(snapshot).bind(proposal).bind(scope.organization).bind(file).fetch_optional(&mut *tx).await.map_err(unavailable)?.ok_or(FileReadError::NotFound)?;
        if row.repository_id != scope.connection.repository
            || row.remote_id.parse::<u64>().ok() != Some(scope.connection.remote_id)
            || row.installation_id.parse::<u64>().ok() != Some(scope.connection.installation_id)
            || row.branch != scope.branch.as_str()
        {
            return Err(FileReadError::Access(ComparisonReadError::ContextChanged));
        }
        let content_id = row
            .content_id
            .filter(|value| digest(value))
            .ok_or(FileReadError::InvalidEvidence)?;
        let change: Change =
            serde_json::from_value(row.change).map_err(|_| FileReadError::InvalidEvidence)?;
        if change.id != file {
            return Err(FileReadError::InvalidEvidence);
        }
        let published_after_image = if change.kind == Kind::Asset && change.after.is_some() {
            let side = change
                .after
                .as_ref()
                .ok_or(FileReadError::InvalidEvidence)?;
            let receipt: ReviewMediaReceipt =
                serde_json::from_value(row.receipt.ok_or(FileReadError::InvalidEvidence)?)
                    .map_err(|_| FileReadError::InvalidEvidence)?;
            if receipt.path != side.path
                || receipt.blob_oid != side.blob_oid
                || receipt.bytes != side.bytes
                || row.media_validation_version < 1
                || !digest(&receipt.source_sha256)
                || !digest(&receipt.display_sha256)
                || !(1..=2048).contains(&receipt.display_width)
                || !(1..=2048).contains(&receipt.display_height)
            {
                return Err(FileReadError::InvalidEvidence);
            }
            Some(PublishedImageEvidence {
                validation_version: row.media_validation_version as u32,
                source_sha256: receipt.source_sha256,
                display_sha256: receipt.display_sha256,
                width: receipt.display_width,
                height: receipt.display_height,
            })
        } else {
            None
        };
        let context = FileReadContext {
            scope,
            snapshot,
            content_id,
            merge_base: CommitSha::parse(row.merge_base_sha)
                .map_err(|_| FileReadError::InvalidEvidence)?,
            head: CommitSha::parse(row.head_sha).map_err(|_| FileReadError::InvalidEvidence)?,
            change,
            published_after_image,
        };
        tx.commit().await.map_err(unavailable)?;
        Ok(context)
    }
}
