//! Operator admission extends the original job; it never transfers publication
//! authority to the operator or replaces an uncertain write with a new operation.
use super::*;
use serde_json::json;
use sqlx::FromRow;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckRecoveryMode {
    RetryUnstarted,
    ObserveStarted,
}
impl CheckRecoveryMode {
    fn code(self) -> &'static str {
        match self {
            Self::RetryUnstarted => "RETRY_UNSTARTED",
            Self::ObserveStarted => "OBSERVE_STARTED",
        }
    }
}
#[derive(Debug, PartialEq, Eq)]
pub struct CheckRecovery {
    pub request_id: Uuid,
    pub job_id: Uuid,
    pub operation_id: Uuid,
    pub mode: CheckRecoveryMode,
    pub max_attempts: i32,
    pub replayed: bool,
}
#[derive(FromRow)]
struct Receipt {
    proposal_id: Uuid,
    operation_id: Uuid,
    actor_id: Uuid,
    request_hash: String,
    mode: String,
    max_attempts: i32,
}
impl PgReviewChecks {
    /// Recover exactly the expected canonical intent after explicit admin action.
    /// Current original authority/evidence is needed to retry unstarted writes;
    /// started work can only continue from its existing durable phase.
    pub async fn recover(
        &self,
        actor: Uuid,
        proposal: Uuid,
        operation: Uuid,
        request: Uuid,
        expected_hash: &str,
    ) -> Result<CheckRecovery, CheckStoreError> {
        if [actor, proposal, operation, request]
            .iter()
            .any(Uuid::is_nil)
            || expected_hash.len() != 64
            || !expected_hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(CheckStoreError::Invalid);
        }
        let mut tx = self.transaction().await?;
        let org:Option<Uuid>=sqlx::query_scalar("SELECT p.organization_id FROM proposals p JOIN memberships m ON m.organization_id=p.organization_id JOIN users u ON u.id=m.user_id WHERE p.id=$1 AND m.user_id=$2 AND u.status='ACTIVE'").bind(proposal).bind(actor).fetch_optional(&mut *tx).await?;
        let org = org.ok_or(CheckStoreError::NotFound)?;
        sqlx::query("SELECT id FROM proposals WHERE organization_id=$1 AND id=$2 FOR UPDATE")
            .bind(org)
            .bind(proposal)
            .execute(&mut *tx)
            .await?;
        sqlx::query("SELECT id FROM organizations WHERE id=$1 FOR UPDATE")
            .bind(org)
            .execute(&mut *tx)
            .await?;
        let role:Option<String>=sqlx::query_scalar("SELECT m.role FROM memberships m JOIN users u ON u.id=m.user_id WHERE m.organization_id=$1 AND m.user_id=$2 AND u.status='ACTIVE' FOR SHARE OF m,u").bind(org).bind(actor).fetch_optional(&mut *tx).await?;
        if role.as_deref() != Some("ADMIN") {
            return Err(CheckStoreError::Forbidden);
        }
        let prior=sqlx::query_as::<_,Receipt>("SELECT proposal_id,operation_id,actor_id,request_hash,mode,max_attempts FROM review_check_recoveries WHERE organization_id=$1 AND request_id=$2")
            .bind(org).bind(request).fetch_optional(&mut *tx).await?;
        if let Some(old) = prior {
            if old.proposal_id != proposal
                || old.operation_id != operation
                || old.actor_id != actor
                || old.request_hash != expected_hash
            {
                return Err(CheckStoreError::IdempotencyMismatch);
            }
            let original = read::load(&mut tx, org, operation)
                .await?
                .ok_or(CheckStoreError::InvalidRecord)?;
            let mode = match old.mode.as_str() {
                "RETRY_UNSTARTED" => CheckRecoveryMode::RetryUnstarted,
                "OBSERVE_STARTED" => CheckRecoveryMode::ObserveStarted,
                _ => return Err(CheckStoreError::InvalidRecord),
            };
            tx.commit().await?;
            return Ok(CheckRecovery {
                request_id: request,
                job_id: original.job_id,
                operation_id: operation,
                mode,
                max_attempts: old.max_attempts,
                replayed: true,
            });
        }
        let original = read::load(&mut tx, org, operation)
            .await?
            .ok_or(CheckStoreError::NotFound)?;
        if original.intent.source().proposal != proposal {
            return Err(CheckStoreError::NotFound);
        }
        if original.intent.request_hash() != expected_hash {
            return Err(CheckStoreError::Stale);
        }
        let (job_state,op_state,attempts,max):(String,String,i32,i32)=sqlx::query_as("SELECT j.state,o.state,j.attempts,j.max_attempts FROM jobs j JOIN git_operations o ON o.id=j.operation_id WHERE j.id=$1 AND o.id=$2 FOR UPDATE OF j,o")
            .bind(original.job_id).bind(operation).fetch_one(&mut *tx).await?;
        if job_state != "FAILED" {
            return Err(CheckStoreError::Busy);
        }
        let phase = journal::phase(&mut tx, &original.intent).await?;
        let mode = if phase
            == specspace_application::review_check_publication::CheckPublicationPhase::NotStarted
            && op_state == "FAILED"
        {
            CheckRecoveryMode::RetryUnstarted
        } else if phase
            != specspace_application::review_check_publication::CheckPublicationPhase::NotStarted
            && op_state == "UNKNOWN"
        {
            CheckRecoveryMode::ObserveStarted
        } else {
            return Err(CheckStoreError::InvalidRecord);
        };
        if !sqlx::query_scalar::<_, bool>("SELECT review_check_lock_scope($1,false)")
            .bind(operation)
            .fetch_one(&mut *tx)
            .await?
        {
            return Err(CheckStoreError::Stale);
        }
        if mode == CheckRecoveryMode::RetryUnstarted {
            journal::authorize(&mut tx, &original.intent).await?;
        }
        // No second active operation may be displaced by an operator retry.
        let busy:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM git_operations WHERE organization_id=$1 AND proposal_id=$2 AND kind='REVIEW_CHECK' AND id<>$3 AND state IN ('PENDING','RUNNING','UNKNOWN'))")
            .bind(org).bind(proposal).bind(operation).fetch_one(&mut *tx).await?;
        if busy {
            return Err(CheckStoreError::Busy);
        }
        journal::connection_cooldown(&mut tx, org, &original.intent.source().repository).await?;
        let allowed:bool=sqlx::query_scalar("SELECT count(*)<12 AND count(*) FILTER(WHERE actor_id=$2)<2 FROM review_check_recoveries WHERE organization_id=$1 AND created_at>clock_timestamp()-interval '10 minutes'")
            .bind(org).bind(actor).fetch_one(&mut *tx).await?;
        if !allowed {
            return Err(CheckStoreError::Quota);
        }
        let extended = max.checked_add(8).ok_or(CheckStoreError::InvalidRecord)?;
        let audit = Uuid::new_v4();
        let event = Uuid::new_v4();
        sqlx::query("INSERT INTO review_check_recoveries(organization_id,request_id,proposal_id,operation_id,actor_id,request_hash,mode,previous_attempts,previous_max_attempts,max_attempts,audit_id,event_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)")
            .bind(org).bind(request).bind(proposal).bind(operation).bind(actor).bind(expected_hash).bind(mode.code()).bind(attempts).bind(max).bind(extended).bind(audit).bind(event).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO audit_events(id,organization_id,actor_id,action,entity_id,after_ref,operation_id,result) VALUES($1,$2,$3,'REVIEW_CHECK_RECOVERY_QUEUED',$4,$5,$6,$7)")
            .bind(audit).bind(org).bind(actor).bind(proposal).bind(request.to_string()).bind(operation).bind(mode.code()).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO outbox(id,organization_id,event_type,entity_id,payload) VALUES($1,$2,'REVIEW_CHECK_RECOVERY_QUEUED',$3,$4)")
            .bind(event).bind(org).bind(proposal).bind(json!({"operation_id":operation,"request_id":request,"mode":mode.code()})).execute(&mut *tx).await?;
        sqlx::query("UPDATE jobs SET state='QUEUED',max_attempts=$2,available_at=clock_timestamp(),result=jsonb_build_object('phase',$3::text,'error_code','CHECK_RECOVERY_REQUESTED') WHERE id=$1")
            .bind(original.job_id).bind(extended).bind(if mode==CheckRecoveryMode::ObserveStarted {"RECONCILIATION_REQUIRED"} else {"RETRY_WAIT"}).execute(&mut *tx).await?;
        sqlx::query("UPDATE git_operations SET state=$2,error_code='CHECK_RECOVERY_REQUESTED',updated_at=clock_timestamp() WHERE id=$1")
            .bind(operation).bind(if mode==CheckRecoveryMode::ObserveStarted {"UNKNOWN"} else {"PENDING"}).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(CheckRecovery {
            request_id: request,
            job_id: original.job_id,
            operation_id: operation,
            mode,
            max_attempts: extended,
            replayed: false,
        })
    }
}
