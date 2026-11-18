use super::*;
use specspace_application::pr_recovery::{PrRecoveryAccepted, PrRecoveryMode, PrRecoveryRequest};

#[derive(FromRow)]
struct Receipt {
    proposal_id: Uuid,
    snapshot_id: Uuid,
    actor_id: Uuid,
    fingerprint: String,
    mode: String,
}
impl Receipt {
    fn accepted(
        self,
        request_id: Uuid,
        replayed: bool,
    ) -> Result<PrRecoveryAccepted, PrSyncStoreError> {
        Ok(PrRecoveryAccepted {
            proposal_id: self.proposal_id,
            snapshot_id: self.snapshot_id,
            request_id,
            mode: match self.mode.as_str() {
                "RETRY_UNSTARTED" => PrRecoveryMode::RetryUnstarted,
                "OBSERVE_STARTED" => PrRecoveryMode::ObserveStarted,
                _ => return Err(PrSyncStoreError::InvalidIntent),
            },
            replayed,
        })
    }
}
impl PgPrSync {
    /// An admin explicitly extends one stopped operation by eight attempts.
    /// Attempts remain monotonic; uncertain writes retain their original marker.
    pub async fn recover(
        &self,
        actor: Uuid,
        proposal: Uuid,
        snapshot: Uuid,
        request: &PrRecoveryRequest,
    ) -> Result<PrRecoveryAccepted, PrSyncStoreError> {
        if actor.is_nil()
            || proposal.is_nil()
            || snapshot.is_nil()
            || request.request_id.is_nil()
            || request.expected_fingerprint.len() != 64
            || !request
                .expected_fingerprint
                .bytes()
                .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
        {
            return Err(PrSyncStoreError::InvalidQuery);
        }
        let mut tx = self.transaction().await?;
        // Same p-before-job ordering as workers, plus current operator authority.
        let access: Option<(Uuid,String)> = sqlx::query_as("SELECT p.organization_id,m.role FROM proposals p JOIN memberships m ON m.organization_id=p.organization_id JOIN users u ON u.id=m.user_id WHERE p.id=$1 AND u.id=$2 AND u.status='ACTIVE' FOR UPDATE OF p FOR SHARE OF m,u")
            .bind(proposal).bind(actor).fetch_optional(&mut *tx).await?;
        let (org, role) = access.ok_or(SubmissionError::NotFound)?;
        if role != "ADMIN" {
            return Err(SubmissionError::Forbidden.into());
        }
        // Organization lock serializes quota and cross-proposal idempotency.
        sqlx::query("SELECT id FROM organizations WHERE id=$1 FOR UPDATE")
            .bind(org)
            .execute(&mut *tx)
            .await?;
        if let Some(old) = sqlx::query_as::<_,Receipt>("SELECT proposal_id,snapshot_id,actor_id,fingerprint,mode FROM pr_sync_recoveries WHERE organization_id=$1 AND request_id=$2")
            .bind(org).bind(request.request_id).fetch_optional(&mut *tx).await? {
            if old.proposal_id!=proposal || old.snapshot_id!=snapshot || old.actor_id!=actor || old.fingerprint!=request.expected_fingerprint {
                return Err(SubmissionError::IdempotencyMismatch.into());
            }
            tx.commit().await?;
            return old.accepted(request.request_id, true);
        }
        let frozen=sqlx::query_as::<_,Frozen>("SELECT i.* FROM pr_sync_intents i WHERE i.organization_id=$1 AND i.proposal_id=$2 AND i.snapshot_id=$3 ORDER BY i.created_at DESC,i.operation_id DESC LIMIT 1")
            .bind(org).bind(proposal).bind(snapshot).fetch_optional(&mut *tx).await?.ok_or(SubmissionError::NotFound)?.restore()?;
        let intent = frozen.intent();
        let source = intent.source();
        if source.fingerprint != request.expected_fingerprint {
            return Err(PrSyncStoreError::Stale);
        }
        let (state,op_state,attempts,max,started):(String,String,i32,i32,bool)=sqlx::query_as("SELECT j.state,o.state,j.attempts,j.max_attempts,EXISTS(SELECT 1 FROM pr_sync_writes w WHERE w.operation_id=o.id) FROM jobs j JOIN git_operations o ON o.id=j.operation_id WHERE j.id=$1 AND o.id=$2 FOR UPDATE OF j,o")
            .bind(frozen.job_id).bind(frozen.operation_id).fetch_one(&mut *tx).await?;
        if state != "FAILED" {
            return Err(PrSyncStoreError::Busy);
        }
        let mode = if started && op_state == "UNKNOWN" {
            PrRecoveryMode::ObserveStarted
        } else if !started && op_state == "FAILED" {
            PrRecoveryMode::RetryUnstarted
        } else {
            return Err(PrSyncStoreError::InvalidIntent);
        };
        journal::current_binding(&mut tx, source).await?;
        if mode == PrRecoveryMode::RetryUnstarted {
            journal::authorize_intent(&mut tx, intent)
                .await
                .map_err(|error| match error {
                    // The operator still has access. Lost original write authority
                    // changes this recovery context, not the operator's membership.
                    PrSyncStoreError::Access(
                        SubmissionError::NotFound | SubmissionError::Forbidden,
                    ) => PrSyncStoreError::Stale,
                    other => other,
                })?;
        }
        let allowed: bool=sqlx::query_scalar("SELECT count(*)<12 AND count(*) FILTER(WHERE actor_id=$2)<2 FROM pr_sync_recoveries WHERE organization_id=$1 AND created_at>clock_timestamp()-interval '10 minutes'")
            .bind(org).bind(actor).fetch_one(&mut *tx).await?;
        if !allowed {
            return Err(PrSyncStoreError::QuotaExceeded);
        }
        let max = max.checked_add(8).ok_or(PrSyncStoreError::InvalidIntent)?;
        sqlx::query("INSERT INTO pr_sync_recoveries(organization_id,request_id,proposal_id,snapshot_id,operation_id,actor_id,fingerprint,mode,previous_attempts,previous_max_attempts,max_attempts) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)")
            .bind(org).bind(request.request_id).bind(proposal).bind(snapshot).bind(source.operation).bind(actor).bind(&request.expected_fingerprint).bind(mode.as_str()).bind(attempts).bind(max-8).bind(max).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO audit_events(id,organization_id,actor_id,action,entity_id,after_ref,operation_id,result) VALUES($1,$2,$3,'PR_SYNC_RECOVERY_QUEUED',$4,$5,$6,$7)")
            .bind(Uuid::new_v4()).bind(org).bind(actor).bind(proposal).bind(request.request_id.to_string()).bind(source.operation).bind(mode.as_str()).execute(&mut *tx).await?;
        sqlx::query("UPDATE jobs SET state='QUEUED',max_attempts=$2,available_at=clock_timestamp(),result=jsonb_build_object('phase',$3::text,'error_code','PR_RECOVERY_REQUESTED') WHERE id=$1")
            .bind(frozen.job_id).bind(max).bind(if started {"RECONCILIATION_REQUIRED"}else{"RETRY_WAIT"}).execute(&mut *tx).await?;
        sqlx::query("UPDATE git_operations SET state=$2,error_code='PR_RECOVERY_REQUESTED',updated_at=clock_timestamp() WHERE id=$1")
            .bind(source.operation).bind(if started {"UNKNOWN"}else{"PENDING"}).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(PrRecoveryAccepted {
            proposal_id: proposal,
            snapshot_id: snapshot,
            request_id: request.request_id,
            mode,
            replayed: false,
        })
    }
}
