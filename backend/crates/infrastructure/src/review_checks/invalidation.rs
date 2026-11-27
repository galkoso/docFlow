//! Failure-only admission resolves the original run from trusted journal data.
//! Old approval evidence and live old refs are not prerequisites for revocation.
use super::*;
use serde_json::json;
use sha2::{Digest, Sha256};
use specspace_application::review_check_publication::{
    CheckInvalidation, CheckPublicationPhase as P, CheckPurpose,
};
use sqlx::FromRow;

#[derive(FromRow)]
struct Context {
    repository_id: Uuid,
    branch: String,
    version: i64,
    state: String,
    policy_version: i64,
    provider: String,
    default_branch: String,
    remote_id: Option<String>,
    installation_id: Option<String>,
    check_app_id: Option<i64>,
    role: String,
}
impl PgReviewChecks {
    /// Explicit Maintainer/Admin revocation. Caller identifies a journaled
    /// evaluation, never a provider run ID, binding or success conclusion.
    /// This is not the automatic policy-event invalidation dispatcher.
    pub async fn invalidate(
        &self,
        actor: Uuid,
        proposal: Uuid,
        target: Uuid,
        reason: CheckInvalidation,
        operation: Uuid,
    ) -> Result<CheckReservation, CheckStoreError> {
        if [actor, proposal, target, operation]
            .iter()
            .any(Uuid::is_nil)
            || target == operation
        {
            return Err(CheckStoreError::Invalid);
        }
        let mut tx = self.transaction().await?;
        sqlx::query("SET LOCAL application_name='specspace-check-invalidation'")
            .execute(&mut *tx)
            .await?;
        let org:Option<Uuid>=sqlx::query_scalar("SELECT p.organization_id FROM proposals p JOIN memberships m ON m.organization_id=p.organization_id JOIN users u ON u.id=m.user_id WHERE p.id=$1 AND m.user_id=$2 AND u.status='ACTIVE'")
            .bind(proposal).bind(actor).fetch_optional(&mut *tx).await?;
        let org = org.ok_or(CheckStoreError::NotFound)?;
        // Same order as evaluation admission: proposal, organization quota, then
        // current connection/membership. No provider call holds these locks.
        sqlx::query("SELECT id FROM proposals WHERE organization_id=$1 AND id=$2 FOR UPDATE")
            .bind(org)
            .bind(proposal)
            .execute(&mut *tx)
            .await?;
        sqlx::query("SELECT id FROM organizations WHERE id=$1 FOR UPDATE")
            .bind(org)
            .execute(&mut *tx)
            .await?;
        let context=sqlx::query_as::<_,Context>("SELECT p.repository_id,p.branch,p.version,p.state,g.policy_version,r.provider,r.default_branch,r.remote_id,r.installation_id,r.check_app_id,m.role
            FROM proposals p JOIN organizations g ON g.id=p.organization_id JOIN repositories r ON r.id=p.repository_id AND r.organization_id=p.organization_id
            JOIN memberships m ON m.organization_id=p.organization_id JOIN users u ON u.id=m.user_id
            WHERE p.organization_id=$1 AND p.id=$2 AND m.user_id=$3 AND u.status='ACTIVE' FOR SHARE OF r,m,u")
            .bind(org).bind(proposal).bind(actor).fetch_optional(&mut *tx).await?.ok_or(CheckStoreError::NotFound)?;
        if !matches!(context.role.as_str(), "MAINTAINER" | "ADMIN") {
            return Err(CheckStoreError::Forbidden);
        }
        // Observation of an exact retry precedes mutable setup/cooldown/quota;
        // it restores the original intent without granting a new provider write.
        if let Some(replay) = read::load(&mut tx, org, operation).await? {
            if replay.intent.source().actor != actor
                || replay.intent.source().proposal != proposal
                || !matches!(replay.intent.purpose(),CheckPurpose::Invalidation{reason:r,check_operation,..} if *r==reason && *check_operation==target)
            {
                return Err(CheckStoreError::IdempotencyMismatch);
            }
            tx.commit().await?;
            return Ok(replay);
        }
        let original = read::load(&mut tx, org, target)
            .await?
            .ok_or(CheckStoreError::NotFound)?;
        if original.intent.source().proposal != proposal
            || !matches!(original.intent.purpose(), CheckPurpose::Evaluation { .. })
        {
            return Err(CheckStoreError::NotFound);
        }
        let run = match journal::phase(&mut tx, &original.intent).await? {
            P::Created(id) | P::UpdateStarted(id) => id,
            _ => return Err(CheckStoreError::InvalidReceipt),
        };
        let mut source = original.intent.source().clone();
        if context.check_app_id.is_none() {
            return Err(CheckStoreError::NotConfigured);
        }
        if context.repository_id != source.repository.repository
            || context.branch != source.branch.as_str()
            || context.provider != "GITHUB"
            || context.default_branch != "main"
            || numeric(context.remote_id.as_deref()) != Some(source.repository.remote_id)
            || numeric(context.installation_id.as_deref())
                != Some(source.repository.installation_id)
            || context.check_app_id.and_then(|n| u64::try_from(n).ok()) != Some(source.app_id)
        {
            return Err(CheckStoreError::Stale);
        }
        admission_limits(&mut tx, org, actor, proposal, operation).await?;
        journal::connection_cooldown(&mut tx, org, &source.repository).await?;
        source.actor = actor;
        source.operation = operation;
        source.authority_digest=format!("{:x}",Sha256::digest(serde_json::to_vec(&json!({
            "kind":"REVIEW_CHECK_INVALIDATION_AUTHORITY_V1","organization":org,"actor":actor,"role":context.role,
            "proposal":proposal,"version":context.version,"state":context.state,"policy_version":context.policy_version,
            "target_operation":target,"target_request_hash":original.intent.request_hash(),"run_id":run.get(),"reason":reason.code()
        })).map_err(|_|CheckStoreError::InvalidRecord)?));
        let intent = CheckPublicationIntent::invalidate(source, reason, run, target)
            .map_err(|_| CheckStoreError::InvalidRecord)?;
        let job_id = reservation::reserve(&mut tx, &intent, None).await?;
        tx.commit().await?;
        Ok(CheckReservation {
            job_id,
            replayed: false,
            source_event: None,
            intent,
        })
    }
}
fn numeric(value: Option<&str>) -> Option<u64> {
    value?
        .parse::<u64>()
        .ok()
        .filter(|n| *n > 0 && *n <= i64::MAX as u64)
}
