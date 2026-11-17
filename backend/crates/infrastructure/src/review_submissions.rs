//! Immutable metadata intent and read-only leases. Provider-started/unknown
//! operations are never reclaimed or failed by this preparation journal.
use crate::review_authorization::{self, PgReviewAuthorization};
use specspace_application::{
    comparison_reader::ComparisonReadError,
    proposal::IdempotencyKey,
    review_context::{ReviewContextError, ReviewExpectation, ReviewScope},
    review_submission::{
        ReviewSubmissionAccepted, ReviewSubmissionRequest, ReviewSubmissionStatus,
        ValidatedReviewSubmission,
    },
};
use specspace_domain::BranchName;
use sqlx::{FromRow, PgConnection, PgPool, Postgres, Transaction};
use uuid::Uuid;

#[derive(Debug, PartialEq, Eq)]
pub enum SubmissionError {
    NotFound,
    Forbidden,
    Busy,
    QuotaExceeded,
    IdempotencyMismatch,
    InvalidIntent,
    InvalidQuery,
    LeaseLost,
    Unavailable,
    Context(ReviewContextError),
}
impl From<sqlx::Error> for SubmissionError {
    fn from(error: sqlx::Error) -> Self {
        if error
            .as_database_error()
            .and_then(|error| error.constraint())
            == Some("proposal_active_git_mutation")
        {
            Self::Busy
        } else {
            Self::Unavailable
        }
    }
}
impl From<ReviewContextError> for SubmissionError {
    fn from(error: ReviewContextError) -> Self {
        match error {
            ReviewContextError::LeaseLost => Self::LeaseLost,
            ReviewContextError::Busy => Self::Busy,
            ReviewContextError::Forbidden => Self::Forbidden,
            ReviewContextError::Read(ComparisonReadError::NotFound) => Self::NotFound,
            other => Self::Context(other),
        }
    }
}
#[derive(Clone, FromRow)]
struct Frozen {
    operation_id: Uuid,
    job_id: Uuid,
    organization_id: Uuid,
    proposal_id: Uuid,
    repository_id: Uuid,
    actor_id: Uuid,
    request_hash: String,
    scope_hash: String,
    expected_head: String,
    expected_target: String,
    expected_version: i64,
    configuration_version: i64,
    policy_version: i64,
    branch: String,
    remote_id: String,
    installation_id: String,
}
impl Frozen {
    fn ticket(&self) -> ReviewSubmissionAccepted {
        ReviewSubmissionAccepted {
            proposal_id: self.proposal_id,
            operation_id: self.operation_id,
            job_id: self.job_id,
        }
    }
    fn input(&self) -> Result<ValidatedReviewSubmission, SubmissionError> {
        let input = ValidatedReviewSubmission::new(
            self.actor_id,
            self.proposal_id,
            ReviewSubmissionRequest {
                expected_head_sha: self.expected_head.clone(),
                expected_target_sha: self.expected_target.clone(),
                expected_proposal_version: self.expected_version,
                expected_configuration_version: self.configuration_version,
                expected_policy_version: self.policy_version,
            },
        )
        .map_err(|_| SubmissionError::InvalidIntent)?;
        let number = |value: &str| value.parse::<u64>().ok().filter(|id| *id > 0).is_some();
        if [
            self.operation_id,
            self.job_id,
            self.organization_id,
            self.repository_id,
        ]
        .iter()
        .any(Uuid::is_nil)
            || self.scope_hash.len() != 64
            || !self
                .scope_hash
                .bytes()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
            || input.fingerprint(self.organization_id) != self.request_hash
            || BranchName::parse(&self.branch).is_err()
            || !number(&self.remote_id)
            || !number(&self.installation_id)
        {
            return Err(SubmissionError::InvalidIntent);
        }
        Ok(input)
    }
}
#[derive(Clone)]
pub struct SubmissionLease {
    frozen: Frozen,
    expectation: ReviewExpectation,
    worker: Uuid,
    attempts: i32,
}
impl SubmissionLease {
    pub fn job_id(&self) -> Uuid {
        self.frozen.job_id
    }
    pub fn operation_id(&self) -> Uuid {
        self.frozen.operation_id
    }
    pub fn actor(&self) -> Uuid {
        self.frozen.actor_id
    }
    pub fn proposal(&self) -> Uuid {
        self.frozen.proposal_id
    }
    pub fn organization(&self) -> Uuid {
        self.frozen.organization_id
    }
    pub fn expectation(&self) -> &ReviewExpectation {
        &self.expectation
    }
    pub fn attempts(&self) -> i32 {
        self.attempts
    }
    pub(crate) fn worker(&self) -> Uuid {
        self.worker
    }
    pub(crate) fn matches_scope(&self, scope: &ReviewScope) -> bool {
        let binding = &scope.comparison.connection;
        scope.comparison.organization == self.organization()
            && binding.repository == self.frozen.repository_id
            && scope.comparison.branch.as_str() == self.frozen.branch
            && self.frozen.remote_id.parse::<u64>().ok() == Some(binding.remote_id)
            && self.frozen.installation_id.parse::<u64>().ok() == Some(binding.installation_id)
            && scope.authority_digest() == self.frozen.scope_hash
    }
}
pub struct PgReviewSubmissions {
    pool: PgPool,
}
impl PgReviewSubmissions {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
    async fn transaction(&self) -> Result<Transaction<'_, Postgres>, SubmissionError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SET LOCAL lock_timeout='5s'")
            .execute(&mut *tx)
            .await?;
        sqlx::query("SET LOCAL statement_timeout='10s'")
            .execute(&mut *tx)
            .await?;
        Ok(tx)
    }
    pub fn authorization(&self, lease: &SubmissionLease) -> PgReviewAuthorization {
        PgReviewAuthorization::for_submission(self.pool.clone(), lease.clone())
    }
    pub async fn reserve(
        &self,
        input: &ValidatedReviewSubmission,
        key: &IdempotencyKey,
    ) -> Result<ReviewSubmissionAccepted, SubmissionError> {
        let mut tx = self.transaction().await?;
        let org = access(&mut tx, input.actor(), input.proposal(), true).await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!(
                "review-submit:{org}:{}:{}",
                input.actor(),
                key.as_str()
            ))
            .execute(&mut *tx)
            .await?;
        let hash = input.fingerprint(org);
        let old=sqlx::query_as::<_,Frozen>("SELECT s.* FROM review_submissions s JOIN git_operations o ON o.id=s.operation_id AND o.organization_id=s.organization_id WHERE s.organization_id=$1 AND s.actor_id=$2 AND o.kind='SUBMIT' AND o.idempotency_key=$3")
            .bind(org).bind(input.actor()).bind(key.as_str()).fetch_optional(&mut *tx).await?;
        if let Some(old) = old {
            old.input()?;
            if old.request_hash != hash {
                return Err(SubmissionError::IdempotencyMismatch);
            }
            let ticket = old.ticket();
            tx.commit().await?;
            return Ok(ticket);
        }
        // Serialize admission within the organization across actors and keys.
        // Exact receipt replay above remains available when a quota is full.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!("review-submit-admission:{org}"))
            .execute(&mut *tx)
            .await?;
        let (actor_active,org_active):(i64,i64)=sqlx::query_as("SELECT count(*) FILTER(WHERE actor_id=$2),count(*) FROM git_operations WHERE organization_id=$1 AND kind='SUBMIT' AND state IN ('PENDING','RUNNING','UNKNOWN')")
            .bind(org).bind(input.actor()).fetch_one(&mut *tx).await?;
        let (actor_recent,org_recent):(i64,i64)=sqlx::query_as("SELECT count(*) FILTER(WHERE actor_id=$2),count(*) FROM review_submissions WHERE organization_id=$1 AND created_at>statement_timestamp()-interval '10 minutes'")
            .bind(org).bind(input.actor()).fetch_one(&mut *tx).await?;
        if actor_active >= 2 || org_active >= 12 || actor_recent >= 10 || org_recent >= 60 {
            return Err(SubmissionError::QuotaExceeded);
        }
        let scope = review_authorization::context_in(
            &mut tx,
            input.actor(),
            input.proposal(),
            input.expectation(),
        )
        .await?;
        let (operation, job) = (Uuid::new_v4(), Uuid::new_v4());
        sqlx::query("INSERT INTO git_operations(id,organization_id,proposal_id,actor_id,kind,idempotency_key,request_hash,expected_sha) VALUES($1,$2,$3,$4,'SUBMIT',$5,$6,$7)")
            .bind(operation).bind(org).bind(input.proposal()).bind(input.actor()).bind(key.as_str()).bind(&hash).bind(input.expectation().head.as_str()).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO jobs(id,organization_id,operation_id,kind,payload,max_attempts,result) VALUES($1,$2,$3,'SUBMIT_REVIEW',jsonb_build_object('operation_id',$3::uuid),8,jsonb_build_object('phase','QUEUED'))")
            .bind(job).bind(org).bind(operation).execute(&mut *tx).await?;
        let read = &scope.comparison;
        sqlx::query("INSERT INTO review_submissions(operation_id,job_id,organization_id,proposal_id,repository_id,actor_id,request_hash,scope_hash,expected_head,expected_target,expected_version,configuration_version,policy_version,branch,remote_id,installation_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16)")
            .bind(operation).bind(job).bind(org).bind(input.proposal()).bind(read.connection.repository).bind(input.actor()).bind(hash).bind(scope.authority_digest())
            .bind(input.expectation().head.as_str()).bind(input.expectation().target.as_str()).bind(input.request().expected_proposal_version)
            .bind(input.request().expected_configuration_version).bind(input.request().expected_policy_version).bind(read.branch.as_str())
            .bind(read.connection.remote_id.to_string()).bind(read.connection.installation_id.to_string()).execute(&mut *tx).await?;
        audit(
            &mut tx,
            org,
            input.actor(),
            input.proposal(),
            operation,
            "REVIEW_SUBMISSION_REQUESTED",
            "PENDING",
        )
        .await?;
        tx.commit().await?;
        Ok(ReviewSubmissionAccepted {
            proposal_id: input.proposal(),
            operation_id: operation,
            job_id: job,
        })
    }
    pub async fn claim(
        &self,
        worker: Uuid,
        seconds: i32,
    ) -> Result<Option<SubmissionLease>, SubmissionError> {
        if worker.is_nil() || !(5..=300).contains(&seconds) {
            return Err(SubmissionError::InvalidQuery);
        }
        let mut tx = self.transaction().await?;
        sqlx::query("WITH exhausted AS (SELECT j.id FROM jobs j JOIN git_operations o ON o.id=j.operation_id AND o.organization_id=j.organization_id WHERE j.kind='SUBMIT_REVIEW' AND o.kind='SUBMIT' AND o.state='PENDING' AND j.attempts>=j.max_attempts AND (j.state='QUEUED' OR (j.state='RUNNING' AND j.leased_until<=clock_timestamp())) FOR UPDATE OF j SKIP LOCKED LIMIT 100), failed AS (UPDATE jobs j SET state='FAILED',lease_owner=NULL,leased_until=NULL,result=jsonb_build_object('phase','FAILED','error_code','REVIEW_SUBMISSION_EXHAUSTED') FROM exhausted x WHERE j.id=x.id RETURNING j.operation_id,j.organization_id), stopped AS (UPDATE git_operations o SET state='FAILED',error_code='REVIEW_SUBMISSION_EXHAUSTED',updated_at=clock_timestamp() FROM failed f WHERE o.id=f.operation_id AND o.organization_id=f.organization_id AND o.state='PENDING' RETURNING o.organization_id,o.actor_id,o.proposal_id,o.id) INSERT INTO audit_events(id,organization_id,actor_id,action,entity_id,after_ref,result) SELECT gen_random_uuid(),organization_id,actor_id,'REVIEW_SUBMISSION_FAILED',proposal_id,id::text,'EXHAUSTED' FROM stopped")
            .execute(&mut *tx).await?;
        let claimed:Option<(Uuid,i32)>=sqlx::query_as("WITH candidate AS (SELECT j.id FROM jobs j JOIN git_operations o ON o.id=j.operation_id AND o.organization_id=j.organization_id JOIN review_submissions s ON s.job_id=j.id AND s.operation_id=o.id AND s.organization_id=j.organization_id WHERE j.kind='SUBMIT_REVIEW' AND o.kind='SUBMIT' AND o.state='PENDING' AND j.attempts<j.max_attempts AND ((j.state='QUEUED' AND j.available_at<=clock_timestamp()) OR (j.state='RUNNING' AND j.leased_until<=clock_timestamp())) ORDER BY j.created_at,j.id FOR UPDATE OF j SKIP LOCKED LIMIT 1) UPDATE jobs j SET state='RUNNING',attempts=attempts+1,lease_owner=$1,leased_until=clock_timestamp()+make_interval(secs=>$2),heartbeat_at=clock_timestamp(),result=jsonb_build_object('phase','PREPARING') FROM candidate c WHERE j.id=c.id RETURNING j.id,j.attempts")
            .bind(worker).bind(f64::from(seconds)).fetch_optional(&mut *tx).await?;
        let Some((job, attempts)) = claimed else {
            tx.commit().await?;
            return Ok(None);
        };
        let row = sqlx::query_as::<_, Frozen>("SELECT * FROM review_submissions WHERE job_id=$1")
            .bind(job)
            .fetch_one(&mut *tx)
            .await?;
        let input = match row.input() {
            Ok(input) => input,
            Err(error) => {
                stop_invalid(&mut tx, &row).await?;
                tx.commit().await?;
                return Err(error);
            }
        };
        let lease = SubmissionLease {
            expectation: input.expectation().clone(),
            frozen: row,
            worker,
            attempts,
        };
        lock_lease(&mut tx, &lease, true).await?;
        tx.commit().await?;
        Ok(Some(lease))
    }
    pub async fn heartbeat(
        &self,
        lease: &SubmissionLease,
        seconds: i32,
    ) -> Result<(), SubmissionError> {
        if !(5..=300).contains(&seconds) {
            return Err(SubmissionError::InvalidQuery);
        }
        let mut tx = self.transaction().await?;
        review_authorization::lease_context_in(&mut tx, lease, true).await?;
        let changed=sqlx::query("UPDATE jobs SET leased_until=clock_timestamp()+make_interval(secs=>$2),heartbeat_at=clock_timestamp() WHERE id=$1 AND state='RUNNING' AND lease_owner=$3 AND attempts=$4 AND leased_until>clock_timestamp()")
            .bind(lease.job_id()).bind(f64::from(seconds)).bind(lease.worker).bind(lease.attempts).execute(&mut *tx).await?.rows_affected();
        if changed != 1 {
            return Err(SubmissionError::LeaseLost);
        }
        tx.commit().await?;
        Ok(())
    }
    pub async fn fail(
        &self,
        lease: &SubmissionLease,
        code: &str,
        retry_after: Option<u32>,
    ) -> Result<bool, SubmissionError> {
        if !error_code(code) || retry_after.is_some_and(|delay| !(1..=86400).contains(&delay)) {
            return Err(SubmissionError::InvalidQuery);
        }
        let mut tx = self.transaction().await?;
        lock_proposal(&mut tx, lease).await?;
        lock_lease(&mut tx, lease, true).await?;
        let max: i32 = sqlx::query_scalar("SELECT max_attempts FROM jobs WHERE id=$1")
            .bind(lease.job_id())
            .fetch_one(&mut *tx)
            .await?;
        let retry = retry_after.is_some() && lease.attempts < max;
        let changed=sqlx::query("UPDATE jobs SET state=$2,lease_owner=NULL,leased_until=NULL,available_at=clock_timestamp()+make_interval(secs=>$3),result=jsonb_build_object('phase',$4::text,'error_code',$5::text) WHERE id=$1 AND leased_until>clock_timestamp()")
            .bind(lease.job_id()).bind(if retry {"QUEUED"}else{"FAILED"}).bind(f64::from(retry_after.unwrap_or(0))).bind(if retry {"RETRY_WAIT"}else{"FAILED"}).bind(code).execute(&mut *tx).await?.rows_affected();
        if changed != 1 {
            return Err(SubmissionError::LeaseLost);
        }
        if !retry {
            sqlx::query("UPDATE git_operations SET state='FAILED',error_code=$2,updated_at=clock_timestamp() WHERE id=$1 AND state='PENDING'")
                .bind(lease.operation_id()).bind(code).execute(&mut *tx).await?;
        }
        audit(
            &mut tx,
            lease.organization(),
            lease.actor(),
            lease.proposal(),
            lease.operation_id(),
            if retry {
                "REVIEW_SUBMISSION_DEFERRED"
            } else {
                "REVIEW_SUBMISSION_FAILED"
            },
            code,
        )
        .await?;
        tx.commit().await?;
        Ok(retry)
    }
    pub async fn status(
        &self,
        actor: Uuid,
        job: Uuid,
    ) -> Result<ReviewSubmissionStatus, SubmissionError> {
        let mut tx = self.transaction().await?;
        let row = sqlx::query_as::<_, Frozen>(
            "SELECT * FROM review_submissions WHERE job_id=$1 AND actor_id=$2",
        )
        .bind(job)
        .bind(actor)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(SubmissionError::NotFound)?;
        if access(&mut tx, actor, row.proposal_id, false).await? != row.organization_id {
            return Err(SubmissionError::NotFound);
        }
        row.input()?;
        let (state,attempts,operation,phase,code):(String,i32,String,Option<String>,Option<String>)=sqlx::query_as("SELECT j.state,j.attempts,o.state,j.result->>'phase',j.result->>'error_code' FROM jobs j JOIN git_operations o ON o.id=j.operation_id AND o.organization_id=j.organization_id WHERE j.id=$1 AND o.id=$2 AND j.kind='SUBMIT_REVIEW' AND o.kind='SUBMIT' FOR SHARE OF j,o")
            .bind(job).bind(row.operation_id).fetch_one(&mut *tx).await?;
        let phase = if matches!(operation.as_str(), "RUNNING" | "UNKNOWN") {
            "RECONCILIATION_REQUIRED".into()
        } else {
            phase.unwrap_or_else(|| state.clone())
        };
        let consistent = match operation.as_str() {
            "PENDING" => match state.as_str() {
                "QUEUED" => matches!(phase.as_str(), "QUEUED" | "RETRY_WAIT"),
                "RUNNING" => phase == "PREPARING",
                _ => false,
            },
            "FAILED" => matches!(state.as_str(), "FAILED" | "CANCELLED") && phase == "FAILED",
            "RUNNING" | "UNKNOWN" => state != "SUCCEEDED",
            "SUCCEEDED" => state == "SUCCEEDED" && phase == "SNAPSHOT_READY" && code.is_none(),
            _ => false,
        };
        if !consistent
            || !matches!(
                phase.as_str(),
                "QUEUED"
                    | "PREPARING"
                    | "RETRY_WAIT"
                    | "FAILED"
                    | "RECONCILIATION_REQUIRED"
                    | "SNAPSHOT_READY"
            )
            || code.as_deref().is_some_and(|code| !error_code(code))
        {
            return Err(SubmissionError::InvalidIntent);
        }
        let publication =
            crate::review_snapshots::publication_in(&mut tx, row.operation_id).await?;
        if (operation == "SUCCEEDED") != publication.is_some() {
            return Err(SubmissionError::InvalidIntent);
        }
        let result = ReviewSubmissionStatus {
            proposal_id: row.proposal_id,
            operation_id: row.operation_id,
            job_id: job,
            state,
            attempts,
            phase,
            error_code: code,
            publication,
        };
        tx.commit().await?;
        Ok(result)
    }
}
pub(crate) async fn lock_proposal(
    connection: &mut PgConnection,
    lease: &SubmissionLease,
) -> Result<(), SubmissionError> {
    let row: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM proposals WHERE id=$1 AND organization_id=$2 FOR SHARE")
            .bind(lease.proposal())
            .bind(lease.organization())
            .fetch_optional(connection)
            .await?;
    if row.is_none() {
        return Err(SubmissionError::LeaseLost);
    }
    Ok(())
}
pub(crate) async fn lock_lease(
    connection: &mut PgConnection,
    lease: &SubmissionLease,
    write: bool,
) -> Result<(), SubmissionError> {
    let locks = if write {
        "FOR UPDATE OF j,o FOR SHARE OF s"
    } else {
        "FOR SHARE OF j,o,s"
    };
    let valid:Option<Uuid>=sqlx::query_scalar(&format!("SELECT j.id FROM jobs j JOIN git_operations o ON o.id=j.operation_id AND o.organization_id=j.organization_id JOIN review_submissions s ON s.job_id=j.id AND s.operation_id=o.id AND s.organization_id=j.organization_id WHERE j.id=$1 AND s.operation_id=$2 AND s.organization_id=$3 AND j.kind='SUBMIT_REVIEW' AND j.state='RUNNING' AND j.lease_owner=$4 AND j.attempts=$5 AND j.leased_until>clock_timestamp() AND j.payload=jsonb_build_object('operation_id',s.operation_id) AND o.kind='SUBMIT' AND o.state='PENDING' AND o.actor_id=s.actor_id AND o.proposal_id=s.proposal_id AND o.request_hash=s.request_hash AND o.expected_sha=s.expected_head AND s.request_hash=$6 AND s.scope_hash=$7 {locks}"))
        .bind(lease.job_id()).bind(lease.operation_id()).bind(lease.organization()).bind(lease.worker).bind(lease.attempts).bind(&lease.frozen.request_hash).bind(&lease.frozen.scope_hash).fetch_optional(connection).await?;
    if valid.is_none() {
        return Err(SubmissionError::LeaseLost);
    }
    Ok(())
}
pub(crate) async fn access(
    connection: &mut PgConnection,
    actor: Uuid,
    proposal: Uuid,
    write: bool,
) -> Result<Uuid, SubmissionError> {
    let locks = if write {
        "FOR UPDATE OF p FOR SHARE OF r,m,u"
    } else {
        "FOR SHARE OF p,r,m,u"
    };
    let row:Option<(Uuid,Uuid,String)>=sqlx::query_as(&format!("SELECT p.organization_id,p.owner_id,m.role FROM proposals p JOIN repositories r ON r.id=p.repository_id AND r.organization_id=p.organization_id JOIN memberships m ON m.organization_id=p.organization_id JOIN users u ON u.id=m.user_id WHERE p.id=$1 AND u.id=$2 AND u.status='ACTIVE' {locks}"))
        .bind(proposal).bind(actor).fetch_optional(&mut *connection).await?;
    let Some((org, owner, role)) = row else {
        return Err(SubmissionError::NotFound);
    };
    if role == "READER" {
        return Err(SubmissionError::Forbidden);
    }
    if role != "ADMIN" && owner != actor {
        let access:Option<String>=sqlx::query_scalar("SELECT access FROM proposal_collaborators WHERE organization_id=$1 AND proposal_id=$2 AND user_id=$3 FOR SHARE")
            .bind(org).bind(proposal).bind(actor).fetch_optional(connection).await?;
        if access.as_deref() != Some("EDIT") {
            return Err(SubmissionError::Forbidden);
        }
    }
    Ok(org)
}
async fn audit(
    connection: &mut PgConnection,
    org: Uuid,
    actor: Uuid,
    proposal: Uuid,
    operation: Uuid,
    action: &str,
    result: &str,
) -> Result<(), SubmissionError> {
    sqlx::query("INSERT INTO audit_events(id,organization_id,actor_id,action,entity_id,after_ref,result) VALUES($1,$2,$3,$4,$5,$6,$7)")
        .bind(Uuid::new_v4()).bind(org).bind(actor).bind(action).bind(proposal).bind(operation.to_string()).bind(result).execute(connection).await?;
    Ok(())
}
async fn stop_invalid(connection: &mut PgConnection, row: &Frozen) -> Result<(), SubmissionError> {
    sqlx::query("UPDATE jobs SET state='FAILED',lease_owner=NULL,leased_until=NULL,result=jsonb_build_object('phase','FAILED','error_code','INVALID_REVIEW_SUBMISSION') WHERE id=$1").bind(row.job_id).execute(&mut *connection).await?;
    sqlx::query("UPDATE git_operations SET state='FAILED',error_code='INVALID_REVIEW_SUBMISSION',updated_at=clock_timestamp() WHERE id=$1 AND state='PENDING'").bind(row.operation_id).execute(&mut *connection).await?;
    audit(
        connection,
        row.organization_id,
        row.actor_id,
        row.proposal_id,
        row.operation_id,
        "REVIEW_SUBMISSION_FAILED",
        "INVALID_INTENT",
    )
    .await
}
fn error_code(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= 100
        && code
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}
