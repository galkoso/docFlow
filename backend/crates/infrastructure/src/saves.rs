//! Short PostgreSQL transactions only. Pending source is private; jobs/audit
//! carry references and digests. Started mutations are never reset for retry.
mod plan;
pub use plan::{SaveManifest, SavedDocument};
use serde::{Deserialize, Serialize};
use specspace_application::{
    proposal::IdempotencyKey,
    save::{PreparedSave, SaveRequest, ValidatedSave},
};
use specspace_domain::BranchName;
use specspace_git_adapter::CommitSha;
use sqlx::{FromRow, PgConnection, PgPool, Postgres, Transaction};
use uuid::Uuid;

#[derive(Debug, PartialEq, Eq)]
pub enum SaveJournalError {
    Unavailable,
    NotFound,
    Forbidden,
    StateChanged,
    Busy,
    IdempotencyMismatch,
    InvalidPlan,
    InvalidLease,
    LeaseLost,
}
impl From<sqlx::Error> for SaveJournalError {
    fn from(_: sqlx::Error) -> Self {
        Self::Unavailable
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SaveContext {
    pub(crate) organization: Uuid,
    pub(crate) proposal: Uuid,
    pub(crate) repository: Uuid,
    pub(crate) actor: Uuid,
    pub(crate) branch: String,
    pub(crate) remote_id: String,
    pub(crate) installation_id: String,
    pub(crate) head: String,
    pub(crate) version: i64,
}
impl SaveContext {
    pub fn proposal(&self) -> Uuid {
        self.proposal
    }
    pub fn repository(&self) -> Uuid {
        self.repository
    }
    pub fn actor(&self) -> Uuid {
        self.actor
    }
    pub fn branch(&self) -> Result<BranchName, SaveJournalError> {
        BranchName::parse(&self.branch).map_err(|_| SaveJournalError::InvalidPlan)
    }
    pub fn head(&self) -> Result<CommitSha, SaveJournalError> {
        CommitSha::parse(&self.head).map_err(|_| SaveJournalError::InvalidPlan)
    }
    pub fn version(&self) -> i64 {
        self.version
    }
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq, FromRow)]
pub struct SaveTicket {
    pub proposal_id: Uuid,
    pub operation_id: Uuid,
    pub job_id: Uuid,
}

pub struct SaveLease {
    pub id: Uuid,
    pub attempts: i32,
    operation: Uuid,
    worker: Uuid,
    context: SaveContext,
    request_hash: String,
    request: Option<ValidatedSave>,
    manifest: Option<SaveManifest>,
    reconcile: bool,
}
impl SaveLease {
    pub fn operation(&self) -> Uuid {
        self.operation
    }
    pub fn context(&self) -> &SaveContext {
        &self.context
    }
    pub fn request(&self) -> Option<&ValidatedSave> {
        self.request.as_ref()
    }
    pub fn manifest(&self) -> Option<&SaveManifest> {
        self.manifest.as_ref()
    }
    pub fn needs_reconciliation(&self) -> bool {
        self.reconcile
    }
    pub(crate) fn fingerprint(&self) -> &str {
        &self.request_hash
    }
}

#[derive(FromRow)]
struct Current {
    organization_id: Uuid,
    repository_id: Uuid,
    owner_id: Uuid,
    branch: String,
    state: String,
    head_sha: Option<String>,
    version: i64,
    role: String,
    remote_id: String,
    installation_id: String,
    provider: String,
    default_branch: String,
}
#[derive(FromRow)]
struct RawSave {
    operation_id: Uuid,
    proposal_id: Uuid,
    repository_id: Uuid,
    actor_id: Uuid,
    branch: String,
    remote_id: String,
    installation_id: String,
    expected_version: i64,
    expected_head: String,
    request: Option<serde_json::Value>,
    manifest: Option<serde_json::Value>,
    manifest_hash: Option<String>,
    request_hash: String,
    operation_state: String,
}
pub struct PgSaves {
    pool: PgPool,
}
impl PgSaves {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
    async fn transaction(&self) -> Result<Transaction<'_, Postgres>, SaveJournalError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SET LOCAL lock_timeout='2s'")
            .execute(&mut *tx)
            .await?;
        sqlx::query("SET LOCAL statement_timeout='5s'")
            .execute(&mut *tx)
            .await?;
        Ok(tx)
    }
    pub async fn context(
        &self,
        actor: Uuid,
        proposal: Uuid,
    ) -> Result<SaveContext, SaveJournalError> {
        let mut tx = self.transaction().await?;
        let (context, state) = current(&mut tx, actor, proposal, false).await?;
        require_open(&state)?;
        tx.commit().await?;
        Ok(context)
    }
    pub async fn reserve(
        &self,
        intent: &ValidatedSave,
        key: &IdempotencyKey,
    ) -> Result<SaveTicket, SaveJournalError> {
        let mut tx = self.transaction().await?;
        let (context, state) = current(&mut tx, intent.actor(), intent.proposal(), true).await?;
        if context.repository != intent.repository() {
            return Err(SaveJournalError::NotFound);
        }
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!(
                "save:{}:{}:{}",
                context.organization,
                context.actor,
                key.as_str()
            ))
            .execute(&mut *tx)
            .await?;
        let existing:Option<(String,Uuid,Uuid,Uuid)>=sqlx::query_as("SELECT o.request_hash,o.proposal_id,o.id,j.id FROM git_operations o JOIN jobs j ON j.operation_id=o.id AND j.organization_id=o.organization_id AND j.kind='SAVE_PROPOSAL' WHERE o.organization_id=$1 AND o.actor_id=$2 AND o.kind='COMMIT' AND o.idempotency_key=$3")
            .bind(context.organization).bind(context.actor).bind(key.as_str()).fetch_optional(&mut *tx).await?;
        if let Some((hash, proposal_id, operation_id, job_id)) = existing {
            if hash != intent.fingerprint() {
                return Err(SaveJournalError::IdempotencyMismatch);
            }
            tx.commit().await?;
            return Ok(SaveTicket {
                proposal_id,
                operation_id,
                job_id,
            });
        }
        require_open(&state)?;
        if context.head != intent.head().as_str()
            || context.version != intent.request().expected_proposal_version
        {
            return Err(SaveJournalError::StateChanged);
        }
        let busy:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM git_operations WHERE proposal_id=$1 AND kind IN ('COMMIT','SUBMIT','IMPORT_APPLY','REFRESH_BASE','MERGE','REVERT') AND state IN ('PENDING','RUNNING','UNKNOWN'))")
            .bind(context.proposal).fetch_one(&mut *tx).await?;
        if busy {
            return Err(SaveJournalError::Busy);
        }
        let operation_id = Uuid::new_v4();
        let job_id = Uuid::new_v4();
        sqlx::query("INSERT INTO git_operations(id,organization_id,proposal_id,actor_id,kind,idempotency_key,request_hash,expected_sha) VALUES($1,$2,$3,$4,'COMMIT',$5,$6,$7)")
            .bind(operation_id).bind(context.organization).bind(context.proposal).bind(context.actor).bind(key.as_str()).bind(intent.fingerprint()).bind(&context.head).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO proposal_saves(operation_id,organization_id,proposal_id,repository_id,actor_id,branch,remote_id,installation_id,expected_version,expected_head,request) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)")
            .bind(operation_id).bind(context.organization).bind(context.proposal).bind(context.repository).bind(context.actor).bind(&context.branch).bind(&context.remote_id).bind(&context.installation_id).bind(context.version).bind(&context.head)
            .bind(serde_json::to_value(intent.request()).map_err(|_|SaveJournalError::InvalidPlan)?).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO jobs(id,organization_id,operation_id,kind,payload,max_attempts) VALUES($1,$2,$3,'SAVE_PROPOSAL',$4,8)")
            .bind(job_id).bind(context.organization).bind(operation_id).bind(serde_json::json!({"proposal_id":context.proposal,"operation_id":operation_id})).execute(&mut *tx).await?;
        audit(
            &mut tx,
            &context,
            operation_id,
            "PROPOSAL_SAVE_REQUESTED",
            "PENDING",
            None,
        )
        .await?;
        tx.commit().await?;
        Ok(SaveTicket {
            proposal_id: context.proposal,
            operation_id,
            job_id,
        })
    }

    pub async fn claim(
        &self,
        worker: Uuid,
        seconds: i32,
    ) -> Result<Option<SaveLease>, SaveJournalError> {
        duration(seconds)?;
        let mut tx = self.transaction().await?;
        // Expired/exhausted work preserves UNKNOWN and the active-operation
        // reservation if a mutation crossed its durable boundary.
        sqlx::query("WITH stale AS (SELECT j.id,j.operation_id,j.organization_id,coalesce(o.state<>'PENDING',true) AS uncertain FROM jobs j LEFT JOIN git_operations o ON o.id=j.operation_id AND o.organization_id=j.organization_id AND o.kind='COMMIT' WHERE j.kind='SAVE_PROPOSAL' AND j.attempts>=j.max_attempts AND (j.state='QUEUED' OR (j.state='RUNNING' AND j.leased_until<=clock_timestamp())) FOR UPDATE OF j SKIP LOCKED LIMIT 100), failed AS (UPDATE jobs j SET state='FAILED',lease_owner=NULL,leased_until=NULL,result=jsonb_build_object('error_code','SAVE_ATTEMPTS_EXHAUSTED','phase',CASE WHEN s.uncertain THEN 'RECONCILIATION_REQUIRED' ELSE 'PREFLIGHT' END) FROM stale s WHERE j.id=s.id RETURNING j.operation_id,j.organization_id), ops AS (UPDATE git_operations o SET state=CASE WHEN o.state='PENDING' THEN 'FAILED' ELSE 'UNKNOWN' END,error_code='SAVE_ATTEMPTS_EXHAUSTED',updated_at=clock_timestamp() FROM failed f WHERE o.id=f.operation_id AND o.organization_id=f.organization_id AND o.kind='COMMIT' AND o.state IN ('PENDING','RUNNING','UNKNOWN') RETURNING o.*) INSERT INTO audit_events(id,organization_id,actor_id,action,entity_id,before_ref,operation_id,result) SELECT gen_random_uuid(),organization_id,actor_id,'PROPOSAL_SAVE_EXHAUSTED',proposal_id,expected_sha,id,state FROM ops")
            .execute(&mut *tx).await?;
        sqlx::query("WITH expired AS (SELECT s.operation_id FROM proposal_saves s JOIN git_operations o ON o.id=s.operation_id AND o.organization_id=s.organization_id WHERE o.state IN ('FAILED','SUCCEEDED') AND s.request IS NOT NULL ORDER BY s.created_at,s.operation_id FOR UPDATE OF s SKIP LOCKED LIMIT 100) UPDATE proposal_saves s SET request=NULL FROM expired e WHERE s.operation_id=e.operation_id")
            .execute(&mut *tx).await?;
        let job=sqlx::query_as::<_,crate::LeasedJob>("WITH candidate AS (SELECT id FROM jobs WHERE kind='SAVE_PROPOSAL' AND attempts<max_attempts AND ((state='QUEUED' AND available_at<=clock_timestamp()) OR (state='RUNNING' AND leased_until<=clock_timestamp())) ORDER BY created_at,id FOR UPDATE SKIP LOCKED LIMIT 1) UPDATE jobs SET state='RUNNING',lease_owner=$1,leased_until=clock_timestamp()+make_interval(secs=>$2),heartbeat_at=clock_timestamp(),attempts=attempts+1 FROM candidate WHERE jobs.id=candidate.id RETURNING jobs.id,organization_id,operation_id,kind,payload,attempts")
            .bind(worker).bind(f64::from(seconds)).fetch_optional(&mut *tx).await?;
        let Some(job) = job else {
            tx.commit().await?;
            return Ok(None);
        };
        let raw=sqlx::query_as::<_,RawSave>("SELECT s.operation_id,s.proposal_id,s.repository_id,s.actor_id,s.branch,s.remote_id,s.installation_id,s.expected_version,s.expected_head,s.request,s.manifest,s.manifest_hash,o.request_hash,o.state AS operation_state FROM proposal_saves s JOIN git_operations o ON o.id=s.operation_id AND o.organization_id=s.organization_id AND o.kind='COMMIT' AND o.actor_id=s.actor_id AND o.proposal_id=s.proposal_id AND o.expected_sha=s.expected_head JOIN jobs j ON j.operation_id=o.id AND j.organization_id=o.organization_id WHERE j.id=$1 AND j.organization_id=$2 AND j.payload=jsonb_build_object('proposal_id',s.proposal_id,'operation_id',s.operation_id) FOR UPDATE OF o")
            .bind(job.id).bind(job.organization_id).fetch_optional(&mut *tx).await?;
        let built = raw
            .ok_or(SaveJournalError::InvalidPlan)
            .and_then(|raw| build_lease(raw, &job, worker));
        let lease = match built {
            Ok(lease) => lease,
            Err(error) => {
                sqlx::query("UPDATE git_operations SET state=CASE WHEN state='PENDING' THEN 'FAILED' ELSE 'UNKNOWN' END,error_code='INVALID_SAVE_PLAN',updated_at=clock_timestamp() WHERE id=$1 AND organization_id=$2 AND kind='COMMIT' AND state IN ('PENDING','RUNNING','UNKNOWN')")
                    .bind(job.operation_id).bind(job.organization_id).execute(&mut *tx).await?;
                sqlx::query("UPDATE jobs SET state='FAILED',lease_owner=NULL,leased_until=NULL,result=jsonb_build_object('error_code','INVALID_SAVE_PLAN','phase',CASE WHEN EXISTS(SELECT 1 FROM git_operations WHERE id=$2 AND organization_id=$3 AND state='UNKNOWN') THEN 'RECONCILIATION_REQUIRED' ELSE 'PREFLIGHT' END) WHERE id=$1")
                    .bind(job.id).bind(job.operation_id).bind(job.organization_id).execute(&mut *tx).await?;
                sqlx::query("INSERT INTO audit_events(id,organization_id,actor_id,action,entity_id,before_ref,operation_id,result) SELECT gen_random_uuid(),organization_id,actor_id,'PROPOSAL_SAVE_INVALID',proposal_id,expected_sha,id,state FROM git_operations WHERE id=$1 AND organization_id=$2 AND kind='COMMIT'")
                    .bind(job.operation_id).bind(job.organization_id).execute(&mut *tx).await?;
                sqlx::query("UPDATE proposal_saves s SET request=NULL FROM git_operations o WHERE s.operation_id=$1 AND o.id=s.operation_id AND o.organization_id=s.organization_id AND o.state='FAILED'")
                    .bind(job.operation_id).execute(&mut *tx).await?;
                tx.commit().await?;
                return Err(error);
            }
        };
        if lease.reconcile {
            sqlx::query("UPDATE git_operations SET state='UNKNOWN',updated_at=clock_timestamp() WHERE id=$1").bind(lease.operation).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(Some(lease))
    }
    pub async fn heartbeat(&self, lease: &SaveLease, seconds: i32) -> Result<(), SaveJournalError> {
        duration(seconds)?;
        fenced(sqlx::query("UPDATE jobs SET leased_until=clock_timestamp()+make_interval(secs=>$4),heartbeat_at=clock_timestamp() WHERE id=$1 AND organization_id=$2 AND lease_owner=$3 AND kind='SAVE_PROPOSAL' AND state='RUNNING' AND leased_until>clock_timestamp()")
            .bind(lease.id).bind(lease.context.organization).bind(lease.worker).bind(f64::from(seconds)).execute(&self.pool).await?.rows_affected())
    }
    pub async fn authorize(&self, lease: &SaveLease) -> Result<(), SaveJournalError> {
        let mut tx = self.transaction().await?;
        eligible(&mut tx, lease, false).await?;
        tx.commit().await?;
        Ok(())
    }
    pub async fn begin_mutation(
        &self,
        lease: &SaveLease,
        plan: &PreparedSave,
    ) -> Result<SaveManifest, SaveJournalError> {
        if lease.reconcile || plan.is_noop() {
            return Err(SaveJournalError::InvalidPlan);
        }
        check_prepared(lease, plan)?;
        let manifest = SaveManifest::from_prepared(plan);
        manifest.validate(&lease.request_hash)?;
        let mut tx = self.transaction().await?;
        lock_job(&mut tx, lease).await?;
        eligible(&mut tx, lease, true).await?;
        fenced(sqlx::query("UPDATE git_operations SET state='RUNNING',updated_at=clock_timestamp() WHERE id=$1 AND organization_id=$2 AND state='PENDING'")
            .bind(lease.operation).bind(lease.context.organization).execute(&mut *tx).await?.rows_affected())?;
        sqlx::query("UPDATE proposal_saves SET manifest=$2,manifest_hash=$3 WHERE operation_id=$1 AND manifest IS NULL")
            .bind(lease.operation).bind(serde_json::to_value(&manifest).map_err(|_|SaveJournalError::InvalidPlan)?).bind(manifest.hash(&lease.context,lease.operation)?).execute(&mut *tx).await?;
        audit(
            &mut tx,
            &lease.context,
            lease.operation,
            "PROPOSAL_SAVE_STARTED",
            "RUNNING",
            None,
        )
        .await?;
        lock_job(&mut tx, lease).await?;
        tx.commit().await?;
        Ok(manifest)
    }
    pub async fn fail(
        &self,
        lease: &SaveLease,
        code: &'static str,
        delay: Option<u32>,
        definitely_not_written: bool,
    ) -> Result<bool, SaveJournalError> {
        self.fail_detailed(lease, code, delay, definitely_not_written, None)
            .await
    }
    pub(crate) async fn fail_detailed(
        &self,
        lease: &SaveLease,
        code: &'static str,
        delay: Option<u32>,
        definitely_not_written: bool,
        details: Option<serde_json::Value>,
    ) -> Result<bool, SaveJournalError> {
        if details.as_ref().is_some_and(|value| {
            serde_json::to_vec(value).map_or(true, |bytes| bytes.len() > 2 * 1024 * 1024)
        }) {
            return Err(SaveJournalError::InvalidPlan);
        }
        if delay.is_some_and(|value| !(1..=86400).contains(&value)) {
            return Err(SaveJournalError::InvalidLease);
        }
        let mut tx = self.transaction().await?;
        let (attempts, max) = lock_job(&mut tx, lease).await?;
        let state: String = sqlx::query_scalar(
            "SELECT state FROM git_operations WHERE id=$1 AND organization_id=$2 FOR UPDATE",
        )
        .bind(lease.operation)
        .bind(lease.context.organization)
        .fetch_one(&mut *tx)
        .await?;
        if !matches!(state.as_str(), "PENDING" | "RUNNING" | "UNKNOWN") {
            return Err(SaveJournalError::InvalidPlan);
        }
        let uncertain = state == "UNKNOWN" || (state == "RUNNING" && !definitely_not_written);
        let retry = delay.is_some() && attempts < max && (state == "PENDING" || uncertain);
        let next = if uncertain {
            "UNKNOWN"
        } else if retry {
            "PENDING"
        } else {
            "FAILED"
        };
        sqlx::query("UPDATE git_operations SET state=$3,error_code=$4,updated_at=clock_timestamp() WHERE id=$1 AND organization_id=$2")
            .bind(lease.operation).bind(lease.context.organization).bind(next).bind(code).execute(&mut *tx).await?;
        fenced(sqlx::query("UPDATE jobs SET state=$4,result=$5,available_at=clock_timestamp()+make_interval(secs=>$6),lease_owner=NULL,leased_until=NULL WHERE id=$1 AND organization_id=$2 AND lease_owner=$3 AND state='RUNNING' AND leased_until>clock_timestamp()")
            .bind(lease.id).bind(lease.context.organization).bind(lease.worker).bind(if retry {"QUEUED"}else{"FAILED"})
            .bind(serde_json::json!({"error_code":code,"phase":if uncertain {"RECONCILIATION_REQUIRED"}else{"PREFLIGHT"},"proposal_id":lease.context.proposal,"details":details}))
            .bind(f64::from(delay.unwrap_or(1))).execute(&mut *tx).await?.rows_affected())?;
        if next == "FAILED" {
            sqlx::query("UPDATE proposal_saves SET request=NULL WHERE operation_id=$1")
                .bind(lease.operation)
                .execute(&mut *tx)
                .await?;
        }
        audit(
            &mut tx,
            &lease.context,
            lease.operation,
            "PROPOSAL_SAVE_FAILED",
            next,
            None,
        )
        .await?;
        tx.commit().await?;
        Ok(retry)
    }

    pub async fn complete(
        &self,
        lease: &SaveLease,
        proof: &crate::save_verification::VerifiedSave,
    ) -> Result<(), SaveJournalError> {
        if proof.operation() != lease.operation || proof.commit().as_str() == lease.context.head {
            return Err(SaveJournalError::InvalidPlan);
        }
        let mut tx = self.transaction().await?;
        lock_job(&mut tx, lease).await?;
        eligible(&mut tx, lease, true).await?;
        let (value, hash): (serde_json::Value, String) = sqlx::query_as(
            "SELECT manifest,manifest_hash FROM proposal_saves WHERE operation_id=$1",
        )
        .bind(lease.operation)
        .fetch_one(&mut *tx)
        .await?;
        let manifest: SaveManifest =
            serde_json::from_value(value).map_err(|_| SaveJournalError::InvalidPlan)?;
        manifest.validate(&lease.request_hash)?;
        if manifest.is_noop()
            || manifest.hash(&lease.context, lease.operation)? != hash
            || proof.manifest_hash() != hash
        {
            return Err(SaveJournalError::InvalidPlan);
        }
        fenced(sqlx::query("UPDATE git_operations SET state='SUCCEEDED',result_sha=$3,error_code=NULL,updated_at=clock_timestamp() WHERE id=$1 AND organization_id=$2 AND state IN ('RUNNING','UNKNOWN') AND kind='COMMIT'")
            .bind(lease.operation).bind(lease.context.organization).bind(proof.commit().as_str()).execute(&mut *tx).await?.rows_affected())?;
        let version:Option<i64>=sqlx::query_scalar("UPDATE proposals SET head_sha=$3,version=version+1,state='DRAFT',updated_at=clock_timestamp() WHERE id=$1 AND organization_id=$2 AND head_sha=$4 AND version=$5 AND state IN ('DRAFT','IN_REVIEW','CHANGES_REQUESTED','APPROVED') RETURNING version")
            .bind(lease.context.proposal).bind(lease.context.organization).bind(proof.commit().as_str()).bind(&lease.context.head).bind(lease.context.version).fetch_optional(&mut *tx).await?;
        let version = version.ok_or(SaveJournalError::StateChanged)?;
        let result = serde_json::json!({"proposal_id":lease.context.proposal,"head_sha":proof.commit().as_str(),"proposal_version":version,
            "committed_at":proof.committed_at(),"no_op":false,"phase":"READY","reconciled":lease.reconcile,"documents":manifest.documents});
        finish(&mut tx, lease, &result).await?;
        audit(
            &mut tx,
            &lease.context,
            lease.operation,
            "PROPOSAL_SAVED",
            if lease.reconcile {
                "RECONCILED"
            } else {
                "VERIFIED"
            },
            Some(proof.commit().as_str()),
        )
        .await?;
        sqlx::query("INSERT INTO outbox(id,organization_id,event_type,entity_id,payload) VALUES($1,$2,'PROPOSAL_CONTENT_CHANGED',$3,$4)")
            .bind(Uuid::new_v4()).bind(lease.context.organization).bind(lease.context.proposal)
            .bind(serde_json::json!({"operation_id":lease.operation,"before_head_sha":lease.context.head,"head_sha":proof.commit().as_str(),"proposal_version":version,"invalidate_review":true})).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }
    pub async fn complete_noop(
        &self,
        lease: &SaveLease,
        plan: &PreparedSave,
    ) -> Result<(), SaveJournalError> {
        if lease.reconcile || !plan.is_noop() {
            return Err(SaveJournalError::InvalidPlan);
        }
        check_prepared(lease, plan)?;
        let manifest = SaveManifest::from_prepared(plan);
        let mut tx = self.transaction().await?;
        lock_job(&mut tx, lease).await?;
        eligible(&mut tx, lease, true).await?;
        fenced(sqlx::query("UPDATE git_operations SET state='SUCCEEDED',result_sha=expected_sha,error_code=NULL,updated_at=clock_timestamp() WHERE id=$1 AND organization_id=$2 AND state='PENDING' AND kind='COMMIT'")
            .bind(lease.operation).bind(lease.context.organization).execute(&mut *tx).await?.rows_affected())?;
        sqlx::query("UPDATE proposal_saves SET manifest=$2,manifest_hash=$3 WHERE operation_id=$1 AND manifest IS NULL")
            .bind(lease.operation).bind(serde_json::to_value(&manifest).map_err(|_|SaveJournalError::InvalidPlan)?).bind(manifest.hash(&lease.context,lease.operation)?).execute(&mut *tx).await?;
        let result = serde_json::json!({"proposal_id":lease.context.proposal,"head_sha":lease.context.head,"proposal_version":lease.context.version,
            "committed_at":null,"no_op":true,"phase":"READY","reconciled":false,"documents":[]});
        finish(&mut tx, lease, &result).await?;
        audit(
            &mut tx,
            &lease.context,
            lease.operation,
            "PROPOSAL_SAVE_NO_CHANGE",
            "NO_CHANGE",
            Some(&lease.context.head),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }
}

fn build_lease(
    raw: RawSave,
    job: &crate::LeasedJob,
    worker: Uuid,
) -> Result<SaveLease, SaveJournalError> {
    if !matches!(
        raw.operation_state.as_str(),
        "PENDING" | "RUNNING" | "UNKNOWN"
    ) {
        return Err(SaveJournalError::InvalidPlan);
    }
    let context = SaveContext {
        organization: job.organization_id,
        proposal: raw.proposal_id,
        repository: raw.repository_id,
        actor: raw.actor_id,
        branch: raw.branch,
        remote_id: raw.remote_id,
        installation_id: raw.installation_id,
        head: raw.expected_head,
        version: raw.expected_version,
    };
    context.branch()?;
    context.head()?;
    let reconcile = raw.operation_state != "PENDING";
    let manifest = raw
        .manifest
        .map(serde_json::from_value::<SaveManifest>)
        .transpose()
        .map_err(|_| SaveJournalError::InvalidPlan)?;
    if let Some(manifest) = &manifest {
        manifest.validate(&raw.request_hash)?;
        if Some(manifest.hash(&context, raw.operation_id)?) != raw.manifest_hash {
            return Err(SaveJournalError::InvalidPlan);
        }
    }
    if reconcile && manifest.is_none() || !reconcile && manifest.is_some() {
        return Err(SaveJournalError::InvalidPlan);
    }
    let request = if !reconcile {
        let request: SaveRequest =
            serde_json::from_value(raw.request.ok_or(SaveJournalError::InvalidPlan)?)
                .map_err(|_| SaveJournalError::InvalidPlan)?;
        let intent =
            ValidatedSave::new(context.actor, context.proposal, context.repository, request)
                .map_err(|_| SaveJournalError::InvalidPlan)?;
        if intent.fingerprint() != raw.request_hash
            || intent.head().as_str() != context.head
            || intent.request().expected_proposal_version != context.version
        {
            return Err(SaveJournalError::InvalidPlan);
        }
        Some(intent)
    } else {
        None
    };
    Ok(SaveLease {
        id: job.id,
        attempts: job.attempts,
        operation: raw.operation_id,
        worker,
        context,
        request_hash: raw.request_hash,
        request,
        manifest,
        reconcile,
    })
}
async fn current(
    connection: &mut PgConnection,
    actor: Uuid,
    proposal: Uuid,
    write: bool,
) -> Result<(SaveContext, String), SaveJournalError> {
    let lock = if write {
        "FOR UPDATE OF p FOR SHARE OF r,m,u"
    } else {
        "FOR SHARE OF p,r,m,u"
    };
    let row=sqlx::query_as::<_,Current>(&format!("SELECT p.organization_id,p.repository_id,p.owner_id,p.branch,p.state,p.head_sha,p.version,m.role,r.remote_id,r.installation_id,r.provider,r.default_branch FROM proposals p JOIN repositories r ON r.id=p.repository_id AND r.organization_id=p.organization_id JOIN memberships m ON m.organization_id=p.organization_id JOIN users u ON u.id=m.user_id WHERE p.id=$1 AND u.id=$2 AND u.status='ACTIVE' {lock}"))
        .bind(proposal).bind(actor).fetch_optional(&mut *connection).await?.ok_or(SaveJournalError::NotFound)?;
    if row.role == "READER" {
        return Err(SaveJournalError::Forbidden);
    }
    if row.owner_id != actor && row.role != "ADMIN" {
        let access:Option<String>=sqlx::query_scalar("SELECT access FROM proposal_collaborators WHERE proposal_id=$1 AND organization_id=$2 AND user_id=$3 FOR SHARE")
            .bind(proposal).bind(row.organization_id).bind(actor).fetch_optional(&mut *connection).await?;
        if access.as_deref() != Some("EDIT") {
            return Err(SaveJournalError::Forbidden);
        }
    }
    if row.provider != "GITHUB" || row.default_branch != "main" {
        return Err(SaveJournalError::StateChanged);
    }
    let context = SaveContext {
        organization: row.organization_id,
        proposal,
        repository: row.repository_id,
        actor,
        branch: row.branch,
        remote_id: row.remote_id,
        installation_id: row.installation_id,
        head: row.head_sha.ok_or(SaveJournalError::StateChanged)?,
        version: row.version,
    };
    context.head()?;
    context.branch()?;
    Ok((context, row.state))
}
async fn eligible(
    connection: &mut PgConnection,
    lease: &SaveLease,
    write: bool,
) -> Result<(), SaveJournalError> {
    let (context, state) = current(
        connection,
        lease.context.actor,
        lease.context.proposal,
        write,
    )
    .await?;
    require_open(&state)?;
    if context != lease.context {
        return Err(SaveJournalError::StateChanged);
    }
    Ok(())
}
fn require_open(state: &str) -> Result<(), SaveJournalError> {
    if matches!(
        state,
        "DRAFT" | "IN_REVIEW" | "CHANGES_REQUESTED" | "APPROVED"
    ) {
        Ok(())
    } else {
        Err(SaveJournalError::StateChanged)
    }
}
fn check_prepared(lease: &SaveLease, plan: &PreparedSave) -> Result<(), SaveJournalError> {
    if plan.actor() != lease.context.actor
        || plan.proposal() != lease.context.proposal
        || plan.repository() != lease.context.repository
        || plan.fingerprint() != lease.request_hash
        || plan.expected_head().as_str() != lease.context.head
        || plan.expected_proposal_version() != lease.context.version
    {
        return Err(SaveJournalError::InvalidPlan);
    }
    Ok(())
}
fn duration(seconds: i32) -> Result<(), SaveJournalError> {
    if (1..=3600).contains(&seconds) {
        Ok(())
    } else {
        Err(SaveJournalError::InvalidLease)
    }
}
fn fenced(rows: u64) -> Result<(), SaveJournalError> {
    if rows == 1 {
        Ok(())
    } else {
        Err(SaveJournalError::LeaseLost)
    }
}
async fn lock_job(
    connection: &mut PgConnection,
    lease: &SaveLease,
) -> Result<(i32, i32), SaveJournalError> {
    sqlx::query_as("SELECT j.attempts,j.max_attempts FROM jobs j JOIN git_operations o ON o.id=j.operation_id AND o.organization_id=j.organization_id WHERE j.id=$1 AND j.organization_id=$2 AND j.lease_owner=$3 AND j.kind='SAVE_PROPOSAL' AND j.state='RUNNING' AND j.leased_until>clock_timestamp() AND o.id=$4 AND o.kind='COMMIT' AND o.actor_id=$5 AND o.proposal_id=$6 AND o.expected_sha=$7 AND o.request_hash=$8 FOR UPDATE OF j,o")
        .bind(lease.id).bind(lease.context.organization).bind(lease.worker).bind(lease.operation).bind(lease.context.actor).bind(lease.context.proposal).bind(&lease.context.head).bind(&lease.request_hash).fetch_optional(connection).await?.ok_or(SaveJournalError::LeaseLost)
}
async fn finish(
    connection: &mut PgConnection,
    lease: &SaveLease,
    result: &serde_json::Value,
) -> Result<(), SaveJournalError> {
    sqlx::query("UPDATE proposal_saves SET request=NULL WHERE operation_id=$1")
        .bind(lease.operation)
        .execute(&mut *connection)
        .await?;
    fenced(sqlx::query("UPDATE jobs SET state='SUCCEEDED',result=$4,lease_owner=NULL,leased_until=NULL WHERE id=$1 AND organization_id=$2 AND lease_owner=$3 AND state='RUNNING' AND leased_until>clock_timestamp()")
        .bind(lease.id).bind(lease.context.organization).bind(lease.worker).bind(result).execute(connection).await?.rows_affected())
}
async fn audit(
    connection: &mut PgConnection,
    context: &SaveContext,
    operation: Uuid,
    action: &str,
    result: &str,
    after: Option<&str>,
) -> Result<(), SaveJournalError> {
    sqlx::query("INSERT INTO audit_events(id,organization_id,actor_id,action,entity_id,before_ref,after_ref,operation_id,result) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)")
        .bind(Uuid::new_v4()).bind(context.organization).bind(context.actor).bind(action).bind(context.proposal).bind(&context.head).bind(after).bind(operation).bind(result).execute(connection).await?;
    Ok(())
}
