use super::*;
use specspace_application::{
    review_check_operations::ReviewCheckOperation,
    review_check_publication::{CheckPublicationPhase as P, CheckPurpose},
};
impl PgReviewChecks {
    /// Read only journal metadata under current operator authority. No provider
    /// observation, claim, admission or recovery occurs on this path.
    pub async fn status(
        &self,
        actor: Uuid,
        proposal: Uuid,
        operation: Uuid,
    ) -> Result<ReviewCheckOperation, CheckStoreError> {
        if [actor, proposal, operation].iter().any(Uuid::is_nil) {
            return Err(CheckStoreError::Invalid);
        }
        let mut tx = self.transaction().await?;
        let access:Option<(Uuid,String)>=sqlx::query_as("SELECT p.organization_id,m.role FROM proposals p JOIN memberships m ON m.organization_id=p.organization_id JOIN users u ON u.id=m.user_id WHERE p.id=$1 AND m.user_id=$2 AND u.status='ACTIVE' FOR SHARE OF p,m,u")
            .bind(proposal).bind(actor).fetch_optional(&mut *tx).await?;
        let (org, role) = access.ok_or(CheckStoreError::NotFound)?;
        if role != "ADMIN" {
            return Err(CheckStoreError::Forbidden);
        }
        let frame = read::load(&mut tx, org, operation)
            .await?
            .ok_or(CheckStoreError::NotFound)?;
        if frame.intent.source().proposal != proposal {
            return Err(CheckStoreError::NotFound);
        }
        let(job_state,operation_state,attempts,max_attempts,error_code,published):(String,String,i32,i32,Option<String>,bool)=sqlx::query_as("SELECT j.state,o.state,j.attempts,j.max_attempts,o.error_code,EXISTS(SELECT 1 FROM review_check_results x WHERE x.operation_id=o.id) FROM jobs j JOIN git_operations o ON o.id=j.operation_id WHERE j.id=$1 AND o.id=$2 FOR SHARE OF j,o")
            .bind(frame.job_id).bind(operation).fetch_one(&mut *tx).await?;
        if error_code.as_ref().is_some_and(|s| {
            s.is_empty()
                || s.len() > 80
                || !s
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        }) {
            return Err(CheckStoreError::InvalidRecord);
        }
        let phase = journal::phase(&mut tx, &frame.intent).await?;
        let known_run = matches!(phase, P::Created(_) | P::UpdateStarted(_))
            || matches!(frame.intent.purpose(), CheckPurpose::Invalidation { .. });
        let write_phase = match phase {
            P::NotStarted => "NOT_STARTED",
            P::CreateStarted => "CREATE_STARTED",
            P::Created(_) => "CREATED",
            P::UpdateStarted(_) => "UPDATE_STARTED",
        }
        .to_owned();
        let purpose = match frame.intent.purpose() {
            CheckPurpose::Evaluation { .. } => "EVALUATION",
            CheckPurpose::Invalidation { .. } => "INVALIDATION",
        }
        .to_owned();
        tx.commit().await?;
        Ok(ReviewCheckOperation {
            viewer_id: actor,
            proposal_id: proposal,
            operation_id: operation,
            snapshot_id: frame.intent.source().snapshot,
            request_hash: frame.intent.request_hash().to_owned(),
            purpose,
            desired_result: read::result(frame.intent.result()).to_owned(),
            job_state,
            operation_state,
            attempts,
            max_attempts,
            write_phase,
            known_run,
            published,
            error_code,
            provider_verified: false,
        })
    }
}
