use super::*;
use specspace_application::{
    comparison_file::book::{BookDocumentContext, ReviewBookAuthorization},
    review_snapshot::ReviewBookDocument,
};

pub struct PgReviewBookAuthorization {
    pool: PgPool,
}
impl PgReviewBookAuthorization {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}
#[derive(FromRow)]
struct BookEvidence {
    #[sqlx(flatten)]
    binding: FrozenBinding,
    fingerprint: String,
    book_manifest_version: i32,
    book_exists: bool,
    document: Option<Value>,
}
#[async_trait]
impl ReviewBookAuthorization for PgReviewBookAuthorization {
    async fn resolve_document(
        &self,
        actor: Uuid,
        proposal: Uuid,
        snapshot: Uuid,
        entry: &str,
    ) -> Result<BookDocumentContext, FileReadError> {
        if [actor, proposal, snapshot].iter().any(Uuid::is_nil) || !digest(entry) {
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
        let row = sqlx::query_as::<_, BookEvidence>("SELECT h.repository_id,h.remote_id,h.installation_id,h.branch,h.head_sha,h.merge_base_sha,h.metadata#>>'{summary,comparison_content_id}' AS content_id,h.media_validation_version,h.fingerprint,h.book_manifest_version,b.snapshot_id IS NOT NULL AS book_exists,d.value AS document FROM review_snapshots h LEFT JOIN review_snapshot_books b ON b.snapshot_id=h.id AND b.organization_id=h.organization_id AND b.proposal_id=h.proposal_id LEFT JOIN LATERAL (SELECT value FROM jsonb_array_elements(b.documents) WHERE value->>'id'=$4) d ON true WHERE h.id=$1 AND h.proposal_id=$2 AND h.organization_id=$3 AND h.state='READY'")
            .bind(snapshot).bind(proposal).bind(scope.organization).bind(entry).fetch_optional(&mut *tx).await.map_err(unavailable)?.ok_or(FileReadError::NotFound)?;
        check_binding(&scope, &row.binding)?;
        if row.book_manifest_version == 0 {
            return Err(FileReadError::NotReady);
        }
        if row.book_manifest_version != 1 || !row.book_exists || !digest(&row.fingerprint) {
            return Err(FileReadError::InvalidEvidence);
        }
        let document: ReviewBookDocument =
            serde_json::from_value(row.document.ok_or(FileReadError::NotFound)?)
                .map_err(|_| FileReadError::InvalidEvidence)?;
        let head =
            CommitSha::parse(row.binding.head_sha).map_err(|_| FileReadError::InvalidEvidence)?;
        if document.id != entry
            || ReviewBookDocument::identity(scope.connection.repository, &head, &document.source)
                .map_err(|_| FileReadError::InvalidEvidence)?
                != entry
        {
            return Err(FileReadError::InvalidEvidence);
        }
        let context = BookDocumentContext {
            scope,
            snapshot,
            fingerprint: row.fingerprint,
            head,
            document,
        };
        tx.commit().await.map_err(unavailable)?;
        Ok(context)
    }
}
