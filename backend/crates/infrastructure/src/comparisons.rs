//! Durable derived comparisons. No provider call or raw source inside a DB
//! transaction. Ready metadata/file pages publish atomically and stay immutable.
use crate::comparison_authorization::context_in;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use specspace_application::{
    comparison::Change,
    comparison_reader::{
        ComparisonReadError, ComparisonSummary, LoadedComparison, ReadContext, RepositoryBinding,
        Selection,
    },
    proposal::IdempotencyKey,
};
use specspace_domain::BranchName;
use specspace_git_adapter::CommitSha;
use sqlx::{FromRow, PgPool, Postgres, QueryBuilder, Transaction};
use uuid::Uuid;
#[derive(Debug, PartialEq, Eq)]
pub enum ComparisonStoreError {
    Read(ComparisonReadError),
    NotFound,
    NotReady,
    Unavailable,
    InvalidSnapshot,
    InvalidQuery,
    IdempotencyMismatch,
    QuotaExceeded,
    LeaseLost,
}
impl From<sqlx::Error> for ComparisonStoreError {
    fn from(_: sqlx::Error) -> Self {
        Self::Unavailable
    }
}
impl From<ComparisonReadError> for ComparisonStoreError {
    fn from(e: ComparisonReadError) -> Self {
        Self::Read(e)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, FromRow, PartialEq, Eq)]
pub struct ComparisonTicket {
    pub proposal_id: Uuid,
    pub snapshot_id: Uuid,
    pub job_id: Uuid,
}
pub struct ComparisonLease {
    job_id: Uuid,
    snapshot_id: Uuid,
    attempts: i32,
    worker: Uuid,
    context: ReadContext,
    head: CommitSha,
    target: Option<CommitSha>,
}
impl ComparisonLease {
    pub fn job_id(&self) -> Uuid {
        self.job_id
    }
    pub fn snapshot_id(&self) -> Uuid {
        self.snapshot_id
    }
    pub fn attempts(&self) -> i32 {
        self.attempts
    }
    pub fn actor(&self) -> Uuid {
        self.context.actor
    }
    pub fn proposal(&self) -> Uuid {
        self.context.proposal
    }
    pub fn selection(&self) -> Selection {
        Selection {
            head: Some(self.head.clone()),
            target: self.target.clone(),
        }
    }
}
#[derive(Serialize)]
pub struct ComparisonStatus {
    pub ticket: ComparisonTicket,
    pub state: String,
    pub job_state: String,
    pub phase: String,
    pub attempts: i32,
    pub error_code: Option<String>,
    pub current_head_sha: String,
    pub current_version: i64,
    pub current_state: String,
    pub completed_at: Option<String>,
    pub summary: Option<ComparisonSummary>,
}
#[derive(Serialize)]
pub struct ComparisonPage {
    pub snapshot_id: Uuid,
    pub content_id: String,
    pub offset: usize,
    pub total: usize,
    pub next_offset: Option<usize>,
    pub files: Vec<Change>,
}
#[derive(FromRow)]
struct Raw {
    id: Uuid,
    organization_id: Uuid,
    proposal_id: Uuid,
    repository_id: Uuid,
    actor_id: Uuid,
    job_id: Uuid,
    request_hash: String,
    branch: String,
    recorded_head: String,
    recorded_base: String,
    proposal_version: i64,
    proposal_state: String,
    remote_id: String,
    installation_id: String,
    selected_head: String,
    selected_target: Option<String>,
    state: String,
    metadata: Option<serde_json::Value>,
    change_count: i32,
    error_code: Option<String>,
}
impl Raw {
    fn context(&self) -> Result<ReadContext, ComparisonStoreError> {
        let number = |text: &str| {
            text.parse::<u64>()
                .ok()
                .filter(|n| *n > 0)
                .ok_or(ComparisonStoreError::InvalidSnapshot)
        };
        Ok(ReadContext {
            actor: self.actor_id,
            organization: self.organization_id,
            proposal: self.proposal_id,
            connection: RepositoryBinding {
                repository: self.repository_id,
                remote_id: number(&self.remote_id)?,
                installation_id: number(&self.installation_id)?,
            },
            branch: BranchName::parse(&self.branch)
                .map_err(|_| ComparisonStoreError::InvalidSnapshot)?,
            recorded_head: sha(&self.recorded_head)?,
            recorded_base: sha(&self.recorded_base)?,
            version: self.proposal_version,
            state: self.proposal_state.clone(),
        })
    }
    fn ticket(&self) -> ComparisonTicket {
        ComparisonTicket {
            proposal_id: self.proposal_id,
            snapshot_id: self.id,
            job_id: self.job_id,
        }
    }
}
fn sha(text: &str) -> Result<CommitSha, ComparisonStoreError> {
    CommitSha::parse(text).map_err(|_| ComparisonStoreError::InvalidSnapshot)
}
fn code(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}
pub struct PgComparisons {
    pool: PgPool,
}
impl PgComparisons {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
    async fn transaction(&self) -> Result<Transaction<'_, Postgres>, ComparisonStoreError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SET LOCAL lock_timeout='2s'")
            .execute(&mut *tx)
            .await?;
        sqlx::query("SET LOCAL statement_timeout='5s'")
            .execute(&mut *tx)
            .await?;
        Ok(tx)
    }
    pub async fn reserve(
        &self,
        actor: Uuid,
        proposal: Uuid,
        key: &IdempotencyKey,
        selection: Selection,
    ) -> Result<ComparisonTicket, ComparisonStoreError> {
        let hash = format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&(
                    "specspace-comparison-request-v1",
                    actor,
                    proposal,
                    selection.head.as_ref().map(CommitSha::as_str),
                    selection.target.as_ref().map(CommitSha::as_str)
                ))
                .map_err(|_| ComparisonStoreError::InvalidQuery)?
            )
        );
        let mut tx = self.transaction().await?;
        let context = context_in(&mut tx, actor, proposal, true).await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!("comparison:{}:{}", context.organization, actor))
            .execute(&mut *tx)
            .await?;
        if let Some(row)=sqlx::query_as::<_,Raw>("SELECT * FROM comparison_snapshots WHERE organization_id=$1 AND actor_id=$2 AND idempotency_key=$3").bind(context.organization).bind(actor).bind(key.as_str()).fetch_optional(&mut *tx).await? {
            if row.request_hash!=hash {return Err(ComparisonStoreError::IdempotencyMismatch);}
            same_connection(&row,&context)?;let ticket=row.ticket();tx.commit().await?;return Ok(ticket);
        }
        let (active,recent):(i64,i64)=sqlx::query_as("SELECT count(*) FILTER(WHERE state='PENDING'),count(*) FILTER(WHERE created_at>clock_timestamp()-interval '1 minute') FROM comparison_snapshots WHERE organization_id=$1 AND actor_id=$2")
            .bind(context.organization).bind(actor).fetch_one(&mut *tx).await?;
        if active >= 2 || recent >= 10 {
            return Err(ComparisonStoreError::QuotaExceeded);
        }
        let snapshot = Uuid::new_v4();
        let job = Uuid::new_v4();
        sqlx::query("INSERT INTO jobs(id,organization_id,kind,payload,max_attempts) VALUES($1,$2,'COMPARE_PROPOSAL',$3,5)").bind(job).bind(context.organization).bind(serde_json::json!({"snapshot_id":snapshot})).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO comparison_snapshots(id,organization_id,proposal_id,repository_id,actor_id,job_id,idempotency_key,request_hash,branch,recorded_head,recorded_base,proposal_version,proposal_state,remote_id,installation_id,selected_head,selected_target) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17)")
            .bind(snapshot).bind(context.organization).bind(proposal).bind(context.connection.repository).bind(actor).bind(job).bind(key.as_str()).bind(hash).bind(context.branch.as_str()).bind(context.recorded_head.as_str()).bind(context.recorded_base.as_str()).bind(context.version).bind(&context.state).bind(context.connection.remote_id.to_string()).bind(context.connection.installation_id.to_string())
            .bind(selection.head.as_ref().unwrap_or(&context.recorded_head).as_str()).bind(selection.target.as_ref().map(CommitSha::as_str)).execute(&mut *tx).await?;
        audit(
            &mut tx,
            &context,
            snapshot,
            "COMPARISON_REQUESTED",
            "QUEUED",
        )
        .await?;
        tx.commit().await?;
        Ok(ComparisonTicket {
            proposal_id: proposal,
            snapshot_id: snapshot,
            job_id: job,
        })
    }
    pub async fn claim(
        &self,
        worker: Uuid,
        seconds: i32,
    ) -> Result<Option<ComparisonLease>, ComparisonStoreError> {
        if !(1..=3600).contains(&seconds) {
            return Err(ComparisonStoreError::InvalidQuery);
        }
        let mut tx = self.transaction().await?;
        sqlx::query("WITH stale AS (SELECT j.id FROM jobs j WHERE j.kind='COMPARE_PROPOSAL' AND (j.state='QUEUED' OR (j.state='RUNNING' AND j.leased_until<=clock_timestamp())) AND (j.attempts>=j.max_attempts OR NOT EXISTS(SELECT 1 FROM comparison_snapshots s WHERE s.job_id=j.id AND s.organization_id=j.organization_id AND s.state='PENDING')) FOR UPDATE OF j SKIP LOCKED LIMIT 100), failed AS (UPDATE jobs j SET state='FAILED',lease_owner=NULL,leased_until=NULL,result=jsonb_build_object('error_code','COMPARISON_EXHAUSTED_OR_INVALID') FROM stale x WHERE j.id=x.id RETURNING j.id,j.organization_id), terminal AS (UPDATE comparison_snapshots s SET state='FAILED',error_code='COMPARISON_EXHAUSTED_OR_INVALID',completed_at=clock_timestamp() FROM failed f WHERE s.job_id=f.id AND s.organization_id=f.organization_id AND s.state='PENDING' RETURNING s.id,s.organization_id,s.actor_id) INSERT INTO audit_events(id,organization_id,actor_id,action,entity_id,result) SELECT gen_random_uuid(),organization_id,actor_id,'COMPARISON_FAILED',id,'EXHAUSTED_OR_INVALID' FROM terminal")
            .execute(&mut *tx).await?;
        let claim:Option<(Uuid,i32,serde_json::Value,Option<Uuid>)>=sqlx::query_as("WITH candidate AS (SELECT j.id,s.id AS snapshot_id FROM jobs j JOIN comparison_snapshots s ON s.job_id=j.id AND s.organization_id=j.organization_id AND s.state='PENDING' WHERE j.kind='COMPARE_PROPOSAL' AND j.attempts<j.max_attempts AND ((j.state='QUEUED' AND j.available_at<=clock_timestamp()) OR (j.state='RUNNING' AND j.leased_until<=clock_timestamp())) ORDER BY j.created_at,j.id FOR UPDATE OF j SKIP LOCKED LIMIT 1) UPDATE jobs j SET state='RUNNING',lease_owner=$1,leased_until=clock_timestamp()+make_interval(secs=>$2),heartbeat_at=clock_timestamp(),attempts=attempts+1,result=jsonb_build_object('snapshot_id',c.snapshot_id,'phase','LOADING') FROM candidate c WHERE j.id=c.id RETURNING j.id,j.attempts,j.payload,j.operation_id")
            .bind(worker).bind(f64::from(seconds)).fetch_optional(&mut *tx).await?;
        let Some((job_id, attempts, payload, operation)) = claim else {
            tx.commit().await?;
            return Ok(None);
        };
        let row = sqlx::query_as::<_, Raw>(
            "SELECT * FROM comparison_snapshots WHERE job_id=$1 FOR UPDATE",
        )
        .bind(job_id)
        .fetch_one(&mut *tx)
        .await?;
        let prepared = (|| {
            Ok::<_, ComparisonStoreError>((
                row.context()?,
                sha(&row.selected_head)?,
                row.selected_target.as_deref().map(sha).transpose()?,
            ))
        })();
        if operation.is_some()
            || payload != serde_json::json!({"snapshot_id":row.id})
            || prepared.is_err()
        {
            sqlx::query("UPDATE jobs SET state='FAILED',lease_owner=NULL,leased_until=NULL,result=jsonb_build_object('error_code','INVALID_COMPARISON_JOB') WHERE id=$1").bind(job_id).execute(&mut *tx).await?;
            sqlx::query("UPDATE comparison_snapshots SET state='FAILED',error_code='INVALID_COMPARISON_JOB',completed_at=clock_timestamp() WHERE id=$1").bind(row.id).execute(&mut *tx).await?;
            sqlx::query("INSERT INTO audit_events(id,organization_id,actor_id,action,entity_id,result) VALUES($1,$2,$3,'COMPARISON_FAILED',$4,'INVALID_JOB')")
                .bind(Uuid::new_v4()).bind(row.organization_id).bind(row.actor_id).bind(row.id).execute(&mut *tx).await?;
            tx.commit().await?;
            return Err(ComparisonStoreError::InvalidSnapshot);
        }
        let (context, head, target) = prepared?;
        let lease = ComparisonLease {
            job_id,
            snapshot_id: row.id,
            attempts,
            worker,
            context,
            head,
            target,
        };
        tx.commit().await?;
        Ok(Some(lease))
    }
    pub async fn authorize(&self, lease: &ComparisonLease) -> Result<(), ComparisonStoreError> {
        let mut tx = self.transaction().await?;
        lock_job(&mut tx, lease).await?;
        if context_in(&mut tx, lease.actor(), lease.proposal(), true).await? != lease.context {
            return Err(ComparisonStoreError::Read(
                ComparisonReadError::ContextChanged,
            ));
        }
        tx.commit().await?;
        Ok(())
    }
    pub async fn heartbeat(
        &self,
        lease: &ComparisonLease,
        seconds: i32,
    ) -> Result<(), ComparisonStoreError> {
        self.renew(lease, seconds, false).await
    }
    pub async fn publication_lease(
        &self,
        lease: &ComparisonLease,
    ) -> Result<(), ComparisonStoreError> {
        self.renew(lease, 120, true).await
    }
    async fn renew(
        &self,
        lease: &ComparisonLease,
        seconds: i32,
        publishing: bool,
    ) -> Result<(), ComparisonStoreError> {
        if !(1..=3600).contains(&seconds) {
            return Err(ComparisonStoreError::InvalidQuery);
        }
        let mut tx = self.transaction().await?;
        lock_job(&mut tx, lease).await?;
        if context_in(&mut tx, lease.actor(), lease.proposal(), true).await? != lease.context {
            return Err(ComparisonStoreError::Read(
                ComparisonReadError::ContextChanged,
            ));
        }
        sqlx::query("UPDATE jobs SET leased_until=clock_timestamp()+make_interval(secs=>$2),heartbeat_at=clock_timestamp(),result=CASE WHEN $3 THEN jsonb_build_object('snapshot_id',$4::uuid,'phase','PUBLISHING') ELSE result END WHERE id=$1")
            .bind(lease.job_id).bind(f64::from(seconds)).bind(publishing).bind(lease.snapshot_id).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }
    pub async fn publish(
        &self,
        lease: &ComparisonLease,
        loaded: &LoadedComparison,
    ) -> Result<(), ComparisonStoreError> {
        if !loaded.matches_context(&lease.context)
            || loaded.head() != &lease.head
            || lease.target.as_ref().is_some_and(|s| s != loaded.target())
        {
            return Err(ComparisonStoreError::InvalidSnapshot);
        }
        let metadata = serde_json::to_value(loaded.summary())
            .map_err(|_| ComparisonStoreError::InvalidSnapshot)?;
        if serde_json::to_vec(&metadata)
            .map_err(|_| ComparisonStoreError::InvalidSnapshot)?
            .len()
            > 4 * 1024 * 1024
            || loaded.inventory().changes.len() > 40000
        {
            return Err(ComparisonStoreError::InvalidSnapshot);
        }
        let mut tx = self.transaction().await?;
        lock_job(&mut tx, lease).await?;
        if context_in(&mut tx, lease.actor(), lease.proposal(), true).await? != lease.context {
            return Err(ComparisonStoreError::Read(
                ComparisonReadError::ContextChanged,
            ));
        }
        sqlx::query("DELETE FROM comparison_files WHERE snapshot_id=$1")
            .bind(lease.snapshot_id)
            .execute(&mut *tx)
            .await?;
        for (batch, changes) in loaded.inventory().changes.chunks(500).enumerate() {
            let mut query = QueryBuilder::<Postgres>::new(
                "INSERT INTO comparison_files(organization_id,snapshot_id,ordinal,file_id,change) ",
            );
            query.push_values(changes.iter().enumerate(), |mut row, (offset, change)| {
                row.push_bind(lease.context.organization)
                    .push_bind(lease.snapshot_id)
                    .push_bind((batch * 500 + offset) as i32)
                    .push_bind(&change.id)
                    .push_bind(sqlx::types::Json(change));
            });
            query.build().execute(&mut *tx).await?;
        }
        lock_job(&mut tx, lease).await?;
        sqlx::query("UPDATE comparison_snapshots SET state='READY',metadata=$2,change_count=$3,error_code=NULL,completed_at=clock_timestamp() WHERE id=$1 AND state='PENDING'")
            .bind(lease.snapshot_id).bind(metadata).bind(loaded.inventory().changes.len() as i32).execute(&mut *tx).await?;
        fenced(sqlx::query("UPDATE jobs SET state='SUCCEEDED',lease_owner=NULL,leased_until=NULL,result=$4 WHERE id=$1 AND lease_owner=$2 AND state='RUNNING' AND leased_until>clock_timestamp() AND organization_id=$3")
            .bind(lease.job_id).bind(lease.worker).bind(lease.context.organization).bind(serde_json::json!({"snapshot_id":lease.snapshot_id,"content_id":loaded.id(),"phase":"READY"})).execute(&mut *tx).await?.rows_affected())?;
        audit(
            &mut tx,
            &lease.context,
            lease.snapshot_id,
            "COMPARISON_READY",
            "READY",
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }
    pub async fn fail(
        &self,
        lease: &ComparisonLease,
        error_code: &str,
        retry_after: Option<u32>,
    ) -> Result<bool, ComparisonStoreError> {
        if !code(error_code) || retry_after.is_some_and(|n| n == 0 || n > 86400) {
            return Err(ComparisonStoreError::InvalidQuery);
        }
        let mut tx = self.transaction().await?;
        lock_job(&mut tx, lease).await?;
        let max: i32 = sqlx::query_scalar("SELECT max_attempts FROM jobs WHERE id=$1")
            .bind(lease.job_id)
            .fetch_one(&mut *tx)
            .await?;
        let retry = retry_after.is_some() && lease.attempts < max;
        sqlx::query("UPDATE jobs SET state=$2,lease_owner=NULL,leased_until=NULL,available_at=clock_timestamp()+make_interval(secs=>$3),result=$4 WHERE id=$1")
            .bind(lease.job_id).bind(if retry {"QUEUED"}else{"FAILED"}).bind(f64::from(retry_after.unwrap_or(0))).bind(serde_json::json!({"snapshot_id":lease.snapshot_id,"error_code":error_code,"phase":if retry {"RETRY_WAIT"}else{"FAILED"}})).execute(&mut *tx).await?;
        sqlx::query("UPDATE comparison_snapshots SET state=$2,error_code=$3,completed_at=CASE WHEN $2='FAILED' THEN clock_timestamp() ELSE NULL END WHERE id=$1")
            .bind(lease.snapshot_id).bind(if retry {"PENDING"}else{"FAILED"}).bind(error_code).execute(&mut *tx).await?;
        audit(
            &mut tx,
            &lease.context,
            lease.snapshot_id,
            "COMPARISON_ATTEMPT",
            if retry { "DEFERRED" } else { "FAILED" },
        )
        .await?;
        tx.commit().await?;
        Ok(retry)
    }
    async fn read_scope(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        actor: Uuid,
        proposal: Uuid,
        snapshot: Uuid,
    ) -> Result<(Raw, ReadContext), ComparisonStoreError> {
        let current = context_in(tx, actor, proposal, true).await?;
        let row=sqlx::query_as::<_,Raw>("SELECT * FROM comparison_snapshots WHERE id=$1 AND proposal_id=$2 AND actor_id=$3 AND organization_id=$4 FOR SHARE")
            .bind(snapshot).bind(proposal).bind(actor).bind(current.organization).fetch_optional(&mut **tx).await?.ok_or(ComparisonStoreError::NotFound)?;
        same_connection(&row, &current)?;
        Ok((row, current))
    }
    pub async fn status(
        &self,
        actor: Uuid,
        proposal: Uuid,
        snapshot: Uuid,
    ) -> Result<ComparisonStatus, ComparisonStoreError> {
        let mut tx = self.transaction().await?;
        let (row, current) = self.read_scope(&mut tx, actor, proposal, snapshot).await?;
        let summary = ready_summary(&mut tx, &row).await?;
        let (job_state,attempts,job_result,completed_at):(String,i32,Option<serde_json::Value>,Option<String>)=sqlx::query_as(r#"SELECT j.state,j.attempts,j.result,to_char(s.completed_at AT TIME ZONE 'UTC','YYYY-MM-DD"T"HH24:MI:SS.MS"Z"') FROM jobs j JOIN comparison_snapshots s ON s.job_id=j.id AND s.organization_id=j.organization_id WHERE j.id=$1 AND j.organization_id=$2 AND j.kind='COMPARE_PROPOSAL'"#).bind(row.job_id).bind(row.organization_id).fetch_one(&mut *tx).await?;
        if row.state == "PENDING" && !matches!(job_state.as_str(), "QUEUED" | "RUNNING") {
            return Err(ComparisonStoreError::InvalidSnapshot);
        }
        let phase = if row.state == "READY" {
            "READY"
        } else if row.state == "FAILED" {
            "FAILED"
        } else if job_state == "RUNNING" {
            if job_result
                .as_ref()
                .and_then(|v| v.get("phase"))
                .and_then(|v| v.as_str())
                == Some("PUBLISHING")
            {
                "PUBLISHING"
            } else {
                "LOADING"
            }
        } else if row.error_code.is_some() {
            "RETRY_WAIT"
        } else {
            "QUEUED"
        };
        let result = ComparisonStatus {
            job_state,
            phase: phase.into(),
            attempts,
            ticket: row.ticket(),
            state: row.state,
            error_code: row.error_code,
            current_head_sha: current.recorded_head.as_str().into(),
            current_version: current.version,
            current_state: current.state,
            completed_at,
            summary,
        };
        tx.commit().await?;
        Ok(result)
    }
    pub async fn page(
        &self,
        actor: Uuid,
        proposal: Uuid,
        snapshot: Uuid,
        offset: usize,
        limit: usize,
    ) -> Result<ComparisonPage, ComparisonStoreError> {
        if offset > 40000 || !(1..=100).contains(&limit) {
            return Err(ComparisonStoreError::InvalidQuery);
        }
        let mut tx = self.transaction().await?;
        let (row, _) = self.read_scope(&mut tx, actor, proposal, snapshot).await?;
        let summary = ready_summary(&mut tx, &row)
            .await?
            .ok_or(ComparisonStoreError::NotReady)?;
        let rows:Vec<(i32,String,serde_json::Value)>=sqlx::query_as("SELECT ordinal,file_id,change FROM comparison_files WHERE snapshot_id=$1 AND organization_id=$2 AND ordinal>=$3 ORDER BY ordinal LIMIT $4")
            .bind(snapshot).bind(row.organization_id).bind(offset as i32).bind(limit as i64).fetch_all(&mut *tx).await?;
        let expected = (summary.changes_total.saturating_sub(offset)).min(limit);
        if rows.len() != expected {
            return Err(ComparisonStoreError::InvalidSnapshot);
        }
        let mut files = vec![];
        for (index, (ordinal, id, value)) in rows.into_iter().enumerate() {
            let change: Change =
                serde_json::from_value(value).map_err(|_| ComparisonStoreError::InvalidSnapshot)?;
            if ordinal as usize != offset + index || change.id != id {
                return Err(ComparisonStoreError::InvalidSnapshot);
            }
            files.push(change);
        }
        let next = offset + files.len();
        let page = ComparisonPage {
            snapshot_id: snapshot,
            content_id: summary.content_id,
            offset,
            total: summary.changes_total,
            next_offset: (next < summary.changes_total).then_some(next),
            files,
        };
        tx.commit().await?;
        Ok(page)
    }
}
fn same_connection(row: &Raw, current: &ReadContext) -> Result<(), ComparisonStoreError> {
    let previous = row.context()?;
    if previous.organization != current.organization
        || previous.proposal != current.proposal
        || previous.connection != current.connection
        || previous.branch != current.branch
    {
        return Err(ComparisonStoreError::Read(
            ComparisonReadError::ContextChanged,
        ));
    }
    Ok(())
}
async fn lock_job(
    tx: &mut Transaction<'_, Postgres>,
    lease: &ComparisonLease,
) -> Result<(), ComparisonStoreError> {
    let valid:Option<Uuid>=sqlx::query_scalar("SELECT j.id FROM jobs j JOIN comparison_snapshots s ON s.job_id=j.id AND s.organization_id=j.organization_id WHERE j.id=$1 AND s.id=$2 AND j.organization_id=$3 AND j.lease_owner=$4 AND j.kind='COMPARE_PROPOSAL' AND j.operation_id IS NULL AND j.state='RUNNING' AND j.leased_until>clock_timestamp() AND s.state='PENDING' AND j.payload=jsonb_build_object('snapshot_id',s.id) FOR UPDATE OF j,s")
        .bind(lease.job_id).bind(lease.snapshot_id).bind(lease.context.organization).bind(lease.worker).fetch_optional(&mut **tx).await?;
    if valid.is_none() {
        return Err(ComparisonStoreError::LeaseLost);
    }
    Ok(())
}
async fn ready_summary(
    tx: &mut Transaction<'_, Postgres>,
    row: &Raw,
) -> Result<Option<ComparisonSummary>, ComparisonStoreError> {
    if row.state != "READY" {
        return Ok(None);
    }
    let valid:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM jobs WHERE id=$1 AND organization_id=$2 AND kind='COMPARE_PROPOSAL' AND operation_id IS NULL AND state='SUCCEEDED')").bind(row.job_id).bind(row.organization_id).fetch_one(&mut **tx).await?;
    if !valid {
        return Err(ComparisonStoreError::InvalidSnapshot);
    }
    let summary: ComparisonSummary = serde_json::from_value(
        row.metadata
            .clone()
            .ok_or(ComparisonStoreError::InvalidSnapshot)?,
    )
    .map_err(|_| ComparisonStoreError::InvalidSnapshot)?;
    if summary.proposal_id != row.proposal_id
        || summary.repository_id != row.repository_id
        || summary.head_sha != row.selected_head
        || summary.changes_total != row.change_count as usize
        || summary.proposal_version != row.proposal_version
    {
        return Err(ComparisonStoreError::InvalidSnapshot);
    }
    Ok(Some(summary))
}
fn fenced(count: u64) -> Result<(), ComparisonStoreError> {
    if count == 1 {
        Ok(())
    } else {
        Err(ComparisonStoreError::LeaseLost)
    }
}
async fn audit(
    tx: &mut Transaction<'_, Postgres>,
    context: &ReadContext,
    snapshot: Uuid,
    action: &str,
    result: &str,
) -> Result<(), ComparisonStoreError> {
    sqlx::query("INSERT INTO audit_events(id,organization_id,actor_id,action,entity_id,result) VALUES($1,$2,$3,$4,$5,$6)").bind(Uuid::new_v4()).bind(context.organization).bind(context.actor).bind(action).bind(snapshot).bind(result).execute(&mut **tx).await?;
    Ok(())
}

#[async_trait::async_trait]
impl specspace_application::comparison_file::ComparisonFileAuthorization for PgComparisons {
    async fn resolve_file(
        &self,
        actor: Uuid,
        proposal: Uuid,
        snapshot: Uuid,
        file: &str,
    ) -> Result<
        specspace_application::comparison_file::FileReadContext,
        specspace_application::comparison_file::FileReadError,
    > {
        use specspace_application::comparison_file::{FileReadContext, FileReadError};
        let read = async {
            if file.len() != 64 || !file.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
                return Err(ComparisonStoreError::NotFound);
            }
            let mut tx = self.transaction().await?;
            let (row, scope) = self.read_scope(&mut tx, actor, proposal, snapshot).await?;
            let summary = ready_summary(&mut tx, &row).await?.ok_or(ComparisonStoreError::NotReady)?;
            let value: Option<serde_json::Value> = sqlx::query_scalar("SELECT change FROM comparison_files WHERE snapshot_id=$1 AND organization_id=$2 AND file_id=$3")
                .bind(snapshot).bind(row.organization_id).bind(file).fetch_optional(&mut *tx).await?;
            let change: Change = serde_json::from_value(value.ok_or(ComparisonStoreError::NotFound)?)
                .map_err(|_| ComparisonStoreError::InvalidSnapshot)?;
            if change.id != file { return Err(ComparisonStoreError::InvalidSnapshot); }
            let context = FileReadContext {
                scope,
                snapshot,
                content_id: summary.content_id,
                merge_base: CommitSha::parse(summary.merge_base_sha).map_err(|_| ComparisonStoreError::InvalidSnapshot)?,
                head: CommitSha::parse(summary.head_sha).map_err(|_| ComparisonStoreError::InvalidSnapshot)?,
                change,
            };
            tx.commit().await?;
            Ok(context)
        }.await;
        read.map_err(|error| match error {
            ComparisonStoreError::Read(error) => FileReadError::Access(error),
            ComparisonStoreError::NotFound => FileReadError::NotFound,
            ComparisonStoreError::NotReady => FileReadError::NotReady,
            ComparisonStoreError::Unavailable => FileReadError::Unavailable,
            _ => FileReadError::InvalidEvidence,
        })
    }
}
