use super::*;
use specspace_application::{
    comparison_file::review_unit::{ReviewUnitAuthorization, ReviewUnitContext},
    review_units::RequiredReviewUnit,
};

pub struct PgReviewUnitAuthorization {
    pool: PgPool,
}
impl PgReviewUnitAuthorization {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}
#[derive(FromRow)]
struct UnitEvidence {
    #[sqlx(flatten)]
    binding: FrozenBinding,
    fingerprint: String,
    review_units_version: i32,
    unit: Option<Value>,
}
#[async_trait]
impl ReviewUnitAuthorization for PgReviewUnitAuthorization {
    async fn resolve_unit(
        &self,
        actor: Uuid,
        proposal: Uuid,
        snapshot: Uuid,
        unit: &str,
    ) -> Result<ReviewUnitContext, FileReadError> {
        if [actor, proposal, snapshot].iter().any(Uuid::is_nil) || !digest(unit) {
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
        let row = sqlx::query_as::<_,UnitEvidence>("SELECT h.repository_id,h.remote_id,h.installation_id,h.branch,h.head_sha,h.merge_base_sha,h.metadata#>>'{summary,comparison_content_id}' AS content_id,h.media_validation_version,h.fingerprint,h.review_units_version,u.unit FROM review_snapshots h LEFT JOIN review_snapshot_units u ON u.snapshot_id=h.id AND u.organization_id=h.organization_id AND u.proposal_id=h.proposal_id AND u.unit_id=$4 WHERE h.id=$1 AND h.proposal_id=$2 AND h.organization_id=$3 AND h.state='READY'")
            .bind(snapshot).bind(proposal).bind(scope.organization).bind(unit).fetch_optional(&mut *tx).await.map_err(unavailable)?.ok_or(FileReadError::NotFound)?;
        check_binding(&scope, &row.binding)?;
        if row.review_units_version == 0 {
            return Err(FileReadError::NotReady);
        }
        if row.review_units_version != 1 || !digest(&row.fingerprint) {
            return Err(FileReadError::InvalidEvidence);
        }
        let required: RequiredReviewUnit =
            serde_json::from_value(row.unit.ok_or(FileReadError::NotFound)?)
                .map_err(|_| FileReadError::InvalidEvidence)?;
        if required.id != unit || !digest(&required.file_id) {
            return Err(FileReadError::InvalidEvidence);
        }
        let context = ReviewUnitContext {
            scope,
            snapshot,
            fingerprint: row.fingerprint,
            head: CommitSha::parse(row.binding.head_sha)
                .map_err(|_| FileReadError::InvalidEvidence)?,
            merge_base: CommitSha::parse(row.binding.merge_base_sha)
                .map_err(|_| FileReadError::InvalidEvidence)?,
            unit: required,
        };
        tx.commit().await.map_err(unavailable)?;
        Ok(context)
    }
}
