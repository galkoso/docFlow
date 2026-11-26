//! Durable execution primitives. No check-provider mutation happens here. A
//! started phase is never rearmed; only an exact observed result can resolve it.
use super::*;
use crate::provider_backoff::{self, ConnectionKey};
use serde_json::json;
use specspace_application::review_check_publication::{
    CheckPublicationDecision as D, CheckPublicationPhase as P, CheckPurpose,
};
use specspace_git_adapter::checks::{CheckRun, CheckRunId, CheckState};
use sqlx::{FromRow, Postgres, Transaction};

#[derive(Clone)]
pub struct CheckLease {
    intent: CheckPublicationIntent,
    job: Uuid,
    worker: Uuid,
    attempt: i32,
}
impl CheckLease {
    pub fn intent(&self) -> &CheckPublicationIntent {
        &self.intent
    }
    pub fn job_id(&self) -> Uuid {
        self.job
    }
    pub fn attempts(&self) -> i32 {
        self.attempt
    }
    pub fn operation_id(&self) -> Uuid {
        self.intent.source().operation
    }
}
pub struct CheckObservation {
    pub phase: P,
}
#[derive(FromRow)]
struct Write {
    phase: String,
    run_id: Option<i64>,
    check_operation: Uuid,
    result: String,
}
#[derive(FromRow)]
struct RunBinding {
    run_id: i64,
    organization_id: Uuid,
    proposal_id: Uuid,
    repository_id: Uuid,
    remote_id: i64,
    app_id: i64,
    head_sha: String,
}
#[derive(FromRow)]
struct Recorded {
    run_id: i64,
    check_operation: Uuid,
    result: String,
}

impl PgReviewChecks {
    pub(super) async fn transaction(&self) -> Result<Transaction<'_, Postgres>, CheckStoreError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SET LOCAL lock_timeout='5s'")
            .execute(&mut *tx)
            .await?;
        sqlx::query("SET LOCAL statement_timeout='10s'")
            .execute(&mut *tx)
            .await?;
        Ok(tx)
    }
    pub async fn claim(
        &self,
        worker: Uuid,
        seconds: i32,
    ) -> Result<Option<CheckLease>, CheckStoreError> {
        if worker.is_nil() || !(5..=300).contains(&seconds) {
            return Err(CheckStoreError::Invalid);
        }
        let mut tx = self.transaction().await?;
        let exhausted: Vec<(Uuid,Uuid,Uuid)> = sqlx::query_as("SELECT j.id,o.id,i.organization_id FROM jobs j JOIN review_check_intents i ON i.job_id=j.id JOIN git_operations o ON o.id=i.operation_id
            WHERE j.kind='PUBLISH_REVIEW_CHECK' AND o.kind='REVIEW_CHECK' AND o.state IN ('PENDING','RUNNING','UNKNOWN') AND j.attempts>=j.max_attempts
            AND (j.state='QUEUED' OR (j.state='RUNNING' AND j.leased_until<=clock_timestamp())) ORDER BY j.created_at,j.id FOR UPDATE OF j SKIP LOCKED LIMIT 20")
            .fetch_all(&mut *tx).await?;
        for (job, op, org) in exhausted {
            let record = read::load(&mut tx, org, op)
                .await?
                .ok_or(CheckStoreError::InvalidRecord)?;
            let started = has_write(&mut tx, op).await?;
            audit(
                &mut tx,
                record.intent.source(),
                "REVIEW_CHECK_EXHAUSTED",
                if started {
                    "RECONCILIATION_REQUIRED"
                } else {
                    "FAILED"
                },
            )
            .await?;
            sqlx::query("UPDATE jobs SET state='FAILED',lease_owner=NULL,leased_until=NULL,result=jsonb_build_object('phase',$2::text,'error_code','REVIEW_CHECK_ATTEMPTS_EXHAUSTED') WHERE id=$1")
                .bind(job).bind(if started {"RECONCILIATION_REQUIRED"} else {"FAILED"}).execute(&mut *tx).await?;
            sqlx::query("UPDATE git_operations SET state=$2,error_code='REVIEW_CHECK_ATTEMPTS_EXHAUSTED',updated_at=clock_timestamp() WHERE id=$1")
                .bind(op).bind(if started {"UNKNOWN"} else {"FAILED"}).execute(&mut *tx).await?;
        }
        let claimed: Option<(Uuid,Uuid,Uuid,i32)> = sqlx::query_as("WITH candidate AS (
            SELECT j.id FROM jobs j JOIN review_check_intents i ON i.job_id=j.id AND i.organization_id=j.organization_id
            JOIN git_operations o ON o.id=i.operation_id AND o.organization_id=i.organization_id
            WHERE j.kind='PUBLISH_REVIEW_CHECK' AND o.kind='REVIEW_CHECK' AND o.state IN ('PENDING','RUNNING','UNKNOWN')
              AND j.attempts<j.max_attempts AND j.available_at<=clock_timestamp()
              AND (j.state='QUEUED' OR (j.state='RUNNING' AND j.leased_until<=clock_timestamp()))
            ORDER BY j.created_at,j.id FOR UPDATE OF j SKIP LOCKED LIMIT 1)
            UPDATE jobs j SET state='RUNNING',lease_owner=$1,attempts=attempts+1,leased_until=clock_timestamp()+make_interval(secs=>$2),heartbeat_at=clock_timestamp()
            FROM candidate c WHERE j.id=c.id RETURNING j.id,j.operation_id,j.organization_id,j.attempts")
            .bind(worker).bind(f64::from(seconds)).fetch_optional(&mut *tx).await?;
        let Some((job, op, org, attempt)) = claimed else {
            tx.commit().await?;
            return Ok(None);
        };
        let record = read::load(&mut tx, org, op)
            .await?
            .ok_or(CheckStoreError::InvalidRecord)?;
        let lease = CheckLease {
            intent: record.intent,
            job,
            worker,
            attempt,
        };
        if has_write(&mut tx, op).await? {
            sqlx::query("UPDATE git_operations SET state='UNKNOWN',updated_at=clock_timestamp() WHERE id=$1 AND state='RUNNING'").bind(op).execute(&mut *tx).await?;
        }
        lock_lease(&mut tx, &lease).await?;
        tx.commit().await?;
        Ok(Some(lease))
    }
    /// Observe owned immutable side effects even after actor/context changes.
    /// This grants no permission to send a new provider mutation.
    pub async fn observe(&self, lease: &CheckLease) -> Result<CheckObservation, CheckStoreError> {
        let mut tx = self.transaction().await?;
        lock_proposal(&mut tx, lease).await?;
        lock_lease(&mut tx, lease).await?;
        binding(&mut tx, lease, false).await?;
        cooldown(&mut tx, lease.intent.source()).await?;
        let phase = phase(&mut tx, &lease.intent).await?;
        tx.commit().await?;
        Ok(CheckObservation { phase })
    }
    /// Fresh provider refs/policy are checked outside a transaction, then current
    /// local facts and the lease are checked again. Renew before/after this bounded
    /// operation; do not race a heartbeat against its short DB transactions.
    pub async fn check(&self, lease: &CheckLease) -> Result<CheckObservation, CheckStoreError> {
        self.observe(lease).await?;
        if matches!(lease.intent.purpose(), CheckPurpose::Evaluation { .. }) {
            let s = lease.intent.source();
            let fresh = self
                .readiness
                .fresh_capture(s.actor, s.proposal, s.snapshot, self.source.as_ref())
                .await?;
            matches_capture(&fresh, s)?;
        }
        let mut tx = self.transaction().await?;
        lock_proposal(&mut tx, lease).await?;
        lock_lease(&mut tx, lease).await?;
        current(&mut tx, lease).await?;
        cooldown(&mut tx, lease.intent.source()).await?;
        let phase = phase(&mut tx, &lease.intent).await?;
        lock_lease(&mut tx, lease).await?;
        tx.commit().await?;
        Ok(CheckObservation { phase })
    }
    pub async fn heartbeat(&self, lease: &CheckLease, seconds: i32) -> Result<(), CheckStoreError> {
        if !(5..=300).contains(&seconds) {
            return Err(CheckStoreError::Invalid);
        }
        let mut tx = self.transaction().await?;
        lock_proposal(&mut tx, lease).await?;
        lock_lease(&mut tx, lease).await?;
        if has_write(&mut tx, lease.operation_id()).await? {
            binding(&mut tx, lease, false).await?;
        } else {
            current(&mut tx, lease).await?;
        }
        let changed=sqlx::query("UPDATE jobs SET leased_until=clock_timestamp()+make_interval(secs=>$2),heartbeat_at=clock_timestamp() WHERE id=$1 AND state='RUNNING' AND lease_owner=$3 AND attempts=$4 AND leased_until>clock_timestamp()")
            .bind(lease.job).bind(f64::from(seconds)).bind(lease.worker).bind(lease.attempt).execute(&mut *tx).await?.rows_affected();
        if changed != 1 {
            return Err(CheckStoreError::LeaseLost);
        }
        tx.commit().await?;
        Ok(())
    }
    /// Complete bounded discovery is supplied by the worker. This method also
    /// checks fresh refs and policy, then commits the marker before any HTTP write.
    pub async fn start_write(
        &self,
        lease: &CheckLease,
        observed: &[CheckRun],
        complete: bool,
    ) -> Result<P, CheckStoreError> {
        self.check(lease).await?;
        let mut tx = self.transaction().await?;
        lock_proposal(&mut tx, lease).await?;
        lock_lease(&mut tx, lease).await?;
        current(&mut tx, lease).await?;
        cooldown(&mut tx, lease.intent.source()).await?;
        let old = phase(&mut tx, &lease.intent).await?;
        let (name, id, result, next) = match lease.intent.decide(observed, complete, old) {
            D::CreatePending => (
                "CREATE",
                None,
                specspace_git_adapter::checks::CheckResult::Pending,
                P::CreateStarted,
            ),
            D::Update(id, result) => ("UPDATE", Some(id), result, P::UpdateStarted(id)),
            _ if matches!(old, P::CreateStarted | P::UpdateStarted(_)) => {
                return Err(CheckStoreError::WriteAlreadyStarted);
            }
            _ => return Err(CheckStoreError::InvalidReceipt),
        };
        sqlx::query("INSERT INTO review_check_writes(operation_id,phase,run_id,check_operation,result,fence_worker,fence_attempt) VALUES($1,$2,$3,$4,$5,$6,$7)")
            .bind(lease.operation_id()).bind(name).bind(id.map(|id|id.get() as i64)).bind(lease.intent.check_operation()).bind(read::result(result)).bind(lease.worker).bind(lease.attempt).execute(&mut *tx).await?;
        sqlx::query(
            "UPDATE git_operations SET state='RUNNING',updated_at=clock_timestamp() WHERE id=$1",
        )
        .bind(lease.operation_id())
        .execute(&mut *tx)
        .await?;
        audit(
            &mut tx,
            lease.intent.source(),
            "REVIEW_CHECK_WRITE_STARTED",
            name,
        )
        .await?;
        lock_lease(&mut tx, lease).await?;
        tx.commit().await?;
        Ok(next)
    }
    /// Persist a verified identity without asserting policy or publication. The
    /// source App/head/operation and original CREATE marker must all match.
    pub async fn adopt(&self, lease: &CheckLease, run: &CheckRun) -> Result<(), CheckStoreError> {
        if !matches!(lease.intent.purpose(), CheckPurpose::Evaluation { .. })
            || !owned(&lease.intent, run)
        {
            return Err(CheckStoreError::InvalidReceipt);
        }
        let mut tx = self.transaction().await?;
        lock_proposal(&mut tx, lease).await?;
        lock_lease(&mut tx, lease).await?;
        binding(&mut tx, lease, false).await?;
        match phase(&mut tx, &lease.intent).await? {
            P::CreateStarted => (),
            P::Created(id) | P::UpdateStarted(id) if id == run.id => {
                tx.commit().await?;
                return Ok(());
            }
            _ => return Err(CheckStoreError::InvalidReceipt),
        }
        let s = lease.intent.source();
        sqlx::query("INSERT INTO review_check_runs(operation_id,organization_id,proposal_id,repository_id,remote_id,app_id,head_sha,run_id,fence_worker,fence_attempt) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)")
            .bind(s.operation).bind(s.organization).bind(s.proposal).bind(s.repository.repository).bind(s.repository.remote_id as i64).bind(s.app_id as i64).bind(s.head.as_str()).bind(run.id.get() as i64).bind(lease.worker).bind(lease.attempt).execute(&mut *tx).await?;
        audit(&mut tx, s, "REVIEW_CHECK_RUN_OBSERVED", "RECORDED").await?;
        lock_lease(&mut tx, lease).await?;
        tx.commit().await?;
        Ok(())
    }
    pub async fn complete(
        &self,
        lease: &CheckLease,
        observed: &[CheckRun],
        complete: bool,
    ) -> Result<(), CheckStoreError> {
        if !complete
            || !observed.iter().any(|run| {
                owned(&lease.intent, run) && run.state == CheckState::from(lease.intent.result())
            })
        {
            return Err(CheckStoreError::InvalidReceipt);
        }
        // A lost commit response may be observed by the original completing
        // frame without another provider call. Other attempts remain fenced out.
        if let Some(recorded) = sqlx::query_as::<_, Recorded>("SELECT x.run_id,x.check_operation,x.result FROM review_check_results x
            JOIN review_check_intents i ON i.operation_id=x.operation_id JOIN jobs j ON j.id=i.job_id
            JOIN git_operations o ON o.id=i.operation_id AND o.organization_id=i.organization_id
            WHERE i.operation_id=$1 AND i.organization_id=$2 AND j.id=$3 AND x.fence_worker=$4 AND x.fence_attempt=$5
              AND x.request_hash=$6 AND i.request_hash=$6 AND o.request_hash=$6
              AND j.kind='PUBLISH_REVIEW_CHECK' AND o.kind='REVIEW_CHECK' AND j.state='SUCCEEDED' AND o.state='SUCCEEDED'
              AND o.result_sha=i.head_sha AND j.result=jsonb_build_object('phase','REVIEW_CHECK_PUBLISHED','run_id',x.run_id,'result',x.result)")
            .bind(lease.operation_id()).bind(lease.intent.source().organization).bind(lease.job).bind(lease.worker).bind(lease.attempt).bind(lease.intent.request_hash())
            .fetch_optional(&self.pool).await? {
            let id = CheckRunId::new(recorded.run_id as u64).map_err(|_| CheckStoreError::InvalidRecord)?;
            if recorded.check_operation!=lease.intent.check_operation() || recorded.result!=read::result(lease.intent.result()) {
                return Err(CheckStoreError::InvalidRecord);
            }
            let mut conn = self.pool.acquire().await?;
            if phase(&mut conn, &lease.intent).await? != P::UpdateStarted(id) {
                return Err(CheckStoreError::InvalidRecord);
            }
            return if lease.intent.decide(observed, complete, P::UpdateStarted(id))==D::Published(id) { Ok(()) } else { Err(CheckStoreError::InvalidReceipt) };
        }
        self.check(lease).await?;
        let mut tx = self.transaction().await?;
        lock_proposal(&mut tx, lease).await?;
        lock_lease(&mut tx, lease).await?;
        current(&mut tx, lease).await?;
        let stage = phase(&mut tx, &lease.intent).await?;
        let D::Published(id) = lease.intent.decide(observed, complete, stage) else {
            return Err(CheckStoreError::InvalidReceipt);
        };
        let run = observed
            .iter()
            .find(|run| run.id == id)
            .ok_or(CheckStoreError::InvalidReceipt)?;
        let s = lease.intent.source();
        let audit_id = Uuid::new_v4();
        let event_id = Uuid::new_v4();
        let result = read::result(lease.intent.result());
        sqlx::query("INSERT INTO review_check_results(operation_id,run_id,check_operation,result,request_hash,fence_worker,fence_attempt,audit_id,event_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)")
            .bind(s.operation).bind(run.id.get() as i64).bind(run.operation).bind(result).bind(lease.intent.request_hash()).bind(lease.worker).bind(lease.attempt).bind(audit_id).bind(event_id).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO audit_events(id,organization_id,actor_id,action,entity_id,after_ref,operation_id,result) VALUES($1,$2,$3,'REVIEW_CHECK_PUBLISHED',$4,$5,$6,'PUBLISHED')")
            .bind(audit_id).bind(s.organization).bind(s.actor).bind(s.proposal).bind(run.id.get().to_string()).bind(s.operation).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO outbox(id,organization_id,event_type,entity_id,payload) VALUES($1,$2,'REVIEW_CHECK_PUBLISHED',$3,$4)")
            .bind(event_id).bind(s.organization).bind(s.proposal).bind(json!({"operation_id":s.operation,"snapshot_id":s.snapshot,"run_id":run.id.get(),"result":result})).execute(&mut *tx).await?;
        lock_lease(&mut tx, lease).await?;
        let changed=sqlx::query("UPDATE jobs SET state='SUCCEEDED',lease_owner=NULL,leased_until=NULL,result=jsonb_build_object('phase','REVIEW_CHECK_PUBLISHED','run_id',$2::bigint,'result',$3::text) WHERE id=$1 AND state='RUNNING' AND lease_owner=$4 AND attempts=$5 AND leased_until>clock_timestamp()")
            .bind(lease.job).bind(run.id.get() as i64).bind(result).bind(lease.worker).bind(lease.attempt).execute(&mut *tx).await?.rows_affected();
        if changed != 1 {
            return Err(CheckStoreError::LeaseLost);
        }
        sqlx::query("UPDATE git_operations SET state='SUCCEEDED',result_sha=expected_sha,error_code=NULL,updated_at=clock_timestamp() WHERE id=$1").bind(s.operation).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }
    /// Unknown effects remain UNKNOWN even after retry exhaustion. Cooldown is
    /// separate from ordinary job retry and is scoped to the exact connection.
    pub async fn defer(
        &self,
        lease: &CheckLease,
        code: &str,
        retry_after: Option<u32>,
        provider_cooldown: Option<u32>,
    ) -> Result<bool, CheckStoreError> {
        if code.is_empty()
            || code.len() > 80
            || !code
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
            || retry_after == Some(0)
            || provider_cooldown == Some(0)
        {
            return Err(CheckStoreError::Invalid);
        }
        let mut tx = self.transaction().await?;
        lock_proposal(&mut tx, lease).await?;
        lock_lease(&mut tx, lease).await?;
        let s = lease.intent.source();
        let started = has_write(&mut tx, s.operation).await?;
        let max: i32 = sqlx::query_scalar("SELECT max_attempts FROM jobs WHERE id=$1")
            .bind(lease.job)
            .fetch_one(&mut *tx)
            .await?;
        let retry = retry_after.is_some() && lease.attempt < max;
        if let Some(seconds) = provider_cooldown {
            backoff(&mut tx, s, seconds).await?;
        }
        audit(&mut tx, s, "REVIEW_CHECK_DEFERRED", code).await?;
        lock_lease(&mut tx, lease).await?;
        let changed=sqlx::query("UPDATE jobs SET state=$2,lease_owner=NULL,leased_until=NULL,available_at=clock_timestamp()+make_interval(secs=>$3),result=jsonb_build_object('phase',$4::text,'error_code',$5::text) WHERE id=$1 AND state='RUNNING' AND lease_owner=$6 AND attempts=$7 AND leased_until>clock_timestamp()")
            .bind(lease.job).bind(if retry {"QUEUED"}else{"FAILED"}).bind(f64::from(retry_after.unwrap_or(0))).bind(if started {"RECONCILIATION_REQUIRED"}else if retry {"RETRY_WAIT"}else{"FAILED"}).bind(code).bind(lease.worker).bind(lease.attempt).execute(&mut *tx).await?.rows_affected();
        if changed != 1 {
            return Err(CheckStoreError::LeaseLost);
        }
        sqlx::query("UPDATE git_operations SET state=$2,error_code=$3,updated_at=clock_timestamp() WHERE id=$1")
            .bind(s.operation).bind(if started {"UNKNOWN"}else if retry {"PENDING"}else{"FAILED"}).bind(code).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(retry)
    }
}

async fn lock_proposal(conn: &mut PgConnection, lease: &CheckLease) -> Result<(), CheckStoreError> {
    let s = lease.intent.source();
    let row: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM proposals WHERE organization_id=$1 AND id=$2 FOR UPDATE",
    )
    .bind(s.organization)
    .bind(s.proposal)
    .fetch_optional(conn)
    .await?;
    if row.is_none() {
        return Err(CheckStoreError::Stale);
    }
    Ok(())
}
async fn lock_lease(conn: &mut PgConnection, lease: &CheckLease) -> Result<(), CheckStoreError> {
    let s = lease.intent.source();
    let found:Option<Uuid>=sqlx::query_scalar("SELECT j.id FROM jobs j JOIN review_check_intents i ON i.job_id=j.id AND i.operation_id=j.operation_id AND i.organization_id=j.organization_id JOIN git_operations o ON o.id=i.operation_id AND o.organization_id=i.organization_id
        WHERE j.id=$1 AND i.operation_id=$2 AND i.organization_id=$3 AND j.kind='PUBLISH_REVIEW_CHECK' AND j.state='RUNNING' AND j.lease_owner=$4 AND j.attempts=$5 AND j.leased_until>clock_timestamp()
        AND j.payload=jsonb_build_object('operation_id',i.operation_id) AND i.request_hash=$6 AND o.kind='REVIEW_CHECK' AND o.state IN ('PENDING','RUNNING','UNKNOWN') AND o.request_hash=i.request_hash AND o.expected_sha=i.head_sha AND o.actor_id=i.actor_id AND o.proposal_id=i.proposal_id FOR UPDATE OF j,o")
        .bind(lease.job).bind(s.operation).bind(s.organization).bind(lease.worker).bind(lease.attempt).bind(lease.intent.request_hash()).fetch_optional(conn).await?;
    if found.is_none() {
        return Err(CheckStoreError::LeaseLost);
    }
    Ok(())
}
async fn binding(
    conn: &mut PgConnection,
    lease: &CheckLease,
    current: bool,
) -> Result<(), CheckStoreError> {
    let valid: bool = sqlx::query_scalar("SELECT review_check_lock_scope($1,$2)")
        .bind(lease.operation_id())
        .bind(current)
        .fetch_one(conn)
        .await?;
    if valid {
        Ok(())
    } else {
        Err(CheckStoreError::Stale)
    }
}
async fn current(conn: &mut PgConnection, lease: &CheckLease) -> Result<(), CheckStoreError> {
    binding(&mut *conn, lease, true).await?;
    if matches!(lease.intent.purpose(), CheckPurpose::Evaluation { .. }) {
        let s = lease.intent.source();
        let c = PgReviewReadiness::capture_in(conn, s.actor, s.proposal, s.snapshot, true, true)
            .await?;
        matches_capture(&c, s)?;
    }
    Ok(())
}
fn matches_capture(
    c: &CapturedReadiness,
    s: &CheckPublicationSource,
) -> Result<(), CheckStoreError> {
    authorized(c)?;
    available(c)?;
    let ctx = c.context.as_ref().ok_or(CheckStoreError::InvalidRecord)?;
    if ctx.actor != s.actor
        || ctx.organization != s.organization
        || ctx.proposal != s.proposal
        || ctx.connection != s.repository
        || ctx.branch != s.branch
        || ctx.version != s.proposal_version
        || c.app_id.and_then(|n| u64::try_from(n).ok()) != Some(s.app_id)
        || c.report.snapshot_id != s.snapshot
        || c.report.fingerprint != s.fingerprint
        || c.report.head_sha != s.head.as_str()
        || c.report.target_sha != s.target.as_str()
        || c.merge_base != s.merge_base.as_str()
        || c.report.policy_version != s.policy_version
        || c.report.configuration_version != s.configuration_version
        || c.evidence_digest != s.evidence_digest
        || c.authority_digest != s.authority_digest
    {
        return Err(CheckStoreError::Stale);
    }
    Ok(())
}
async fn has_write(conn: &mut PgConnection, op: Uuid) -> Result<bool, CheckStoreError> {
    Ok(
        sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM review_check_writes WHERE operation_id=$1)",
        )
        .bind(op)
        .fetch_one(conn)
        .await?,
    )
}
pub(super) async fn phase(
    conn: &mut PgConnection,
    intent: &CheckPublicationIntent,
) -> Result<P, CheckStoreError> {
    let writes=sqlx::query_as::<_,Write>("SELECT phase,run_id,check_operation,result FROM review_check_writes WHERE operation_id=$1 ORDER BY phase").bind(intent.source().operation).fetch_all(&mut *conn).await?;
    let bound = sqlx::query_as::<_, RunBinding>("SELECT run_id,organization_id,proposal_id,repository_id,remote_id,app_id,head_sha FROM review_check_runs WHERE operation_id=$1")
        .bind(intent.check_operation()).fetch_optional(conn).await?;
    let bound = if let Some(row) = bound {
        let s = intent.source();
        if row.organization_id != s.organization
            || row.proposal_id != s.proposal
            || row.repository_id != s.repository.repository
            || row.remote_id != s.repository.remote_id as i64
            || row.app_id != s.app_id as i64
            || row.head_sha != s.head.as_str()
        {
            return Err(CheckStoreError::InvalidRecord);
        }
        Some(CheckRunId::new(row.run_id as u64).map_err(|_| CheckStoreError::InvalidRecord)?)
    } else {
        None
    };
    let mut created = false;
    let mut updated = None;
    for w in writes {
        if w.check_operation != intent.check_operation() {
            return Err(CheckStoreError::InvalidRecord);
        }
        match w.phase.as_str() {
            "CREATE"
                if !created
                    && w.run_id.is_none()
                    && w.result == "PENDING"
                    && matches!(intent.purpose(), CheckPurpose::Evaluation { .. }) =>
            {
                created = true
            }
            "UPDATE" if updated.is_none() && w.result == read::result(intent.result()) => {
                updated = Some(
                    CheckRunId::new(w.run_id.ok_or(CheckStoreError::InvalidRecord)? as u64)
                        .map_err(|_| CheckStoreError::InvalidRecord)?,
                )
            }
            _ => return Err(CheckStoreError::InvalidRecord),
        }
    }
    match intent.purpose() {
        CheckPurpose::Evaluation { .. } => match (created, bound, updated) {
            (false, None, None) => Ok(P::NotStarted),
            (true, None, None) => Ok(P::CreateStarted),
            (true, Some(id), None) => Ok(P::Created(id)),
            (true, Some(id), Some(update)) if id == update => Ok(P::UpdateStarted(id)),
            _ => Err(CheckStoreError::InvalidRecord),
        },
        CheckPurpose::Invalidation { run, .. } if bound == Some(*run) && !created => {
            match updated {
                None => Ok(P::NotStarted),
                Some(id) if id == *run => Ok(P::UpdateStarted(id)),
                _ => Err(CheckStoreError::InvalidRecord),
            }
        }
        _ => Err(CheckStoreError::InvalidRecord),
    }
}
fn owned(intent: &CheckPublicationIntent, run: &CheckRun) -> bool {
    run.scope == intent.scope() && run.operation == intent.check_operation()
}
async fn audit(
    conn: &mut PgConnection,
    s: &CheckPublicationSource,
    action: &str,
    result: &str,
) -> Result<(), CheckStoreError> {
    sqlx::query("INSERT INTO audit_events(id,organization_id,actor_id,action,entity_id,after_ref,operation_id,result) VALUES($1,$2,$3,$4,$5,$6,$7,$8)")
        .bind(Uuid::new_v4()).bind(s.organization).bind(s.actor).bind(action).bind(s.proposal).bind(s.snapshot.to_string()).bind(s.operation).bind(result).execute(conn).await?;
    Ok(())
}
async fn cooldown(
    conn: &mut PgConnection,
    source: &CheckPublicationSource,
) -> Result<(), CheckStoreError> {
    connection_cooldown(conn, source.organization, &source.repository).await
}
pub(super) async fn connection_cooldown(
    conn: &mut PgConnection,
    org: Uuid,
    repository: &specspace_application::comparison_reader::RepositoryBinding,
) -> Result<(), CheckStoreError> {
    let remote = repository.remote_id.to_string();
    let install = repository.installation_id.to_string();
    if let Some(seconds) = provider_backoff::remaining(
        conn,
        &ConnectionKey {
            organization: org,
            repository: repository.repository,
            remote_id: &remote,
            installation_id: &install,
        },
    )
    .await?
    {
        return Err(CheckStoreError::Cooldown(seconds));
    }
    Ok(())
}
async fn backoff(
    conn: &mut PgConnection,
    s: &CheckPublicationSource,
    seconds: u32,
) -> Result<(), CheckStoreError> {
    let remote = s.repository.remote_id.to_string();
    let install = s.repository.installation_id.to_string();
    provider_backoff::defer(
        conn,
        &ConnectionKey {
            organization: s.organization,
            repository: s.repository.repository,
            remote_id: &remote,
            installation_id: &install,
        },
        seconds,
    )
    .await?;
    Ok(())
}
