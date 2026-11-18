use super::*;
use specspace_application::{
    pr_sync::{PrBinding, PrSyncDecision, PrWritePhase},
    review_context::ReviewContextError,
};
use specspace_git_adapter::pull_requests::{PullRequest, PullRequestNumber, PullRequestState};
use sqlx::{PgConnection, Postgres, Transaction};

#[derive(Clone)]
pub struct PrSyncLease {
    intent: PrSyncIntent,
    job: Uuid,
    worker: Uuid,
    attempt: i32,
}
impl PrSyncLease {
    pub fn intent(&self) -> &PrSyncIntent {
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
pub struct PrSyncObservation {
    pub phase: PrWritePhase,
    pub binding: Option<PrBinding>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrSyncResolution {
    Synced,
    Superseded,
}
impl PrSyncResolution {
    fn as_str(self) -> &'static str {
        match self {
            Self::Synced => "SYNCED",
            Self::Superseded => "SUPERSEDED",
        }
    }
}
#[derive(FromRow)]
struct Binding {
    repository_id: Uuid,
    remote_id: String,
    installation_id: String,
    branch: String,
    pr_number: i64,
    provider_id: String,
    node_id: String,
    url: String,
}
impl Binding {
    fn identity(&self, source: &PrSyncSource) -> Result<PrBinding, PrSyncStoreError> {
        if self.repository_id != source.repository.repository
            || number(&self.remote_id)? != source.repository.remote_id
            || number(&self.installation_id)? != source.repository.installation_id
            || self.branch != source.branch.as_str()
        {
            return Err(PrSyncStoreError::Stale);
        }
        Ok(PrBinding {
            number: PullRequestNumber::new(self.pr_number as u64)
                .map_err(|_| PrSyncStoreError::InvalidIntent)?,
            provider_id: number(&self.provider_id)?,
        })
    }
}
async fn phase(conn: &mut PgConnection, op: Uuid) -> Result<PrWritePhase, PrSyncStoreError> {
    let kind: Option<String> =
        sqlx::query_scalar("SELECT kind FROM pr_sync_writes WHERE operation_id=$1")
            .bind(op)
            .fetch_optional(conn)
            .await?;
    match kind.as_deref() {
        None => Ok(PrWritePhase::NotStarted),
        Some("CREATE") => Ok(PrWritePhase::CreateStarted),
        Some("UPDATE") => Ok(PrWritePhase::UpdateStarted),
        _ => Err(PrSyncStoreError::InvalidIntent),
    }
}
async fn binding(
    conn: &mut PgConnection,
    s: &PrSyncSource,
) -> Result<Option<Binding>, PrSyncStoreError> {
    let result=sqlx::query_as::<_,Binding>("SELECT * FROM proposal_pull_requests WHERE proposal_id=$1 AND organization_id=$2 FOR SHARE")
        .bind(s.proposal).bind(s.organization).fetch_optional(conn).await?;
    if let Some(b) = &result {
        b.identity(s)?;
    }
    Ok(result)
}
async fn lock_proposal(conn: &mut PgConnection, s: &PrSyncSource) -> Result<(), PrSyncStoreError> {
    let found: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM proposals WHERE id=$1 AND organization_id=$2 FOR UPDATE",
    )
    .bind(s.proposal)
    .bind(s.organization)
    .fetch_optional(conn)
    .await?;
    if found.is_none() {
        return Err(PrSyncStoreError::Stale);
    }
    Ok(())
}
async fn lock_lease(conn: &mut PgConnection, lease: &PrSyncLease) -> Result<(), PrSyncStoreError> {
    let s = lease.intent.source();
    let valid:Option<Uuid>=sqlx::query_scalar("SELECT j.id FROM jobs j JOIN git_operations o ON o.id=j.operation_id AND o.organization_id=j.organization_id JOIN pr_sync_intents i ON i.operation_id=o.id AND i.job_id=j.id AND i.organization_id=o.organization_id WHERE j.id=$1 AND o.id=$2 AND o.organization_id=$3 AND j.kind='SYNC_REVIEW_PR' AND j.state='RUNNING' AND j.lease_owner=$4 AND j.attempts=$5 AND j.leased_until>clock_timestamp() AND j.payload=jsonb_build_object('operation_id',o.id) AND o.kind='PR_SYNC' AND o.state IN ('PENDING','RUNNING','UNKNOWN') AND o.actor_id=$6 AND o.proposal_id=$7 AND o.request_hash=$8 AND i.request_hash=$8 AND o.expected_sha=$9 FOR UPDATE OF j,o")
        .bind(lease.job).bind(s.operation).bind(s.organization).bind(lease.worker).bind(lease.attempt).bind(s.actor).bind(s.proposal).bind(lease.intent.request_hash()).bind(s.head.as_str()).fetch_optional(conn).await?;
    if valid.is_none() {
        return Err(PrSyncStoreError::LeaseLost);
    }
    Ok(())
}
async fn current_binding(
    conn: &mut PgConnection,
    s: &PrSyncSource,
) -> Result<(), PrSyncStoreError> {
    let valid:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM proposals p JOIN repositories r ON r.id=p.repository_id AND r.organization_id=p.organization_id WHERE p.id=$1 AND p.organization_id=$2 AND p.repository_id=$3 AND p.branch=$4 AND r.provider='GITHUB' AND r.default_branch='main' AND ltrim(r.remote_id,'0+')=$5 AND ltrim(r.installation_id,'0+')=$6)")
        .bind(s.proposal).bind(s.organization).bind(s.repository.repository).bind(s.branch.as_str()).bind(s.repository.remote_id.to_string()).bind(s.repository.installation_id.to_string()).fetch_one(conn).await?;
    if !valid {
        return Err(PrSyncStoreError::Stale);
    }
    Ok(())
}
async fn authorize(conn: &mut PgConnection, lease: &PrSyncLease) -> Result<(), PrSyncStoreError> {
    let s = lease.intent.source();
    if review_submissions::access(conn, s.actor, s.proposal, true).await? != s.organization {
        return Err(PrSyncStoreError::Stale);
    }
    current_binding(conn, s).await?;
    let expectation = ReviewExpectation {
        head: s.head.clone(),
        target: s.target.clone(),
        proposal_version: s.proposal_version,
        configuration_version: s.configuration_version,
        policy_version: s.policy_version,
    };
    let scope = review_authorization::context_in(conn, s.actor, s.proposal, &expectation)
        .await
        .map_err(SubmissionError::from)?;
    let (active,target,number,title,links):(Option<Uuid>,Option<String>,i64,String,Value)=sqlx::query_as("SELECT active_review_snapshot_id,target_sha,number,title,links FROM proposals WHERE id=$1 AND organization_id=$2")
        .bind(s.proposal).bind(s.organization).fetch_one(conn).await?;
    let links: Vec<ProposalLink> =
        serde_json::from_value(links).map_err(|_| PrSyncStoreError::InvalidIntent)?;
    if active != Some(s.snapshot)
        || target.as_deref() != Some(s.target.as_str())
        || scope.comparison.connection != s.repository
        || scope.comparison.branch != s.branch
        || authority_digest(&scope.authority_digest(), number, &title, &links) != s.authority_digest
    {
        return Err(PrSyncStoreError::Stale);
    }
    Ok(())
}
async fn observe_authority(
    conn: &mut PgConnection,
    lease: &PrSyncLease,
) -> Result<(), PrSyncStoreError> {
    let s = lease.intent.source();
    if review_submissions::access(conn, s.actor, s.proposal, true).await? != s.organization {
        return Err(PrSyncStoreError::Stale);
    }
    current_binding(conn, s).await
}
async fn audit(
    conn: &mut PgConnection,
    s: &PrSyncSource,
    action: &str,
    result: &str,
) -> Result<(), PrSyncStoreError> {
    sqlx::query("INSERT INTO audit_events(id,organization_id,actor_id,action,entity_id,after_ref,operation_id,result) VALUES($1,$2,$3,$4,$5,$6,$7,$8)")
        .bind(Uuid::new_v4()).bind(s.organization).bind(s.actor).bind(action).bind(s.proposal).bind(s.snapshot.to_string()).bind(s.operation).bind(result).execute(conn).await?;
    Ok(())
}
impl PgPrSync {
    async fn transaction(&self) -> Result<Transaction<'_, Postgres>, PrSyncStoreError> {
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
    ) -> Result<Option<PrSyncLease>, PrSyncStoreError> {
        if worker.is_nil() || !(5..=300).contains(&seconds) {
            return Err(PrSyncStoreError::InvalidQuery);
        }
        let mut tx = self.transaction().await?;
        // Exhaustion ends automatic observation, never the uncertainty of a write.
        let exhausted:Vec<(Uuid,Uuid)>=sqlx::query_as("SELECT j.id,o.id FROM jobs j JOIN git_operations o ON o.id=j.operation_id WHERE j.kind='SYNC_REVIEW_PR' AND o.kind='PR_SYNC' AND o.state IN ('PENDING','RUNNING','UNKNOWN') AND j.attempts>=j.max_attempts AND (j.state='QUEUED' OR (j.state='RUNNING' AND j.leased_until<=clock_timestamp())) ORDER BY j.created_at,j.id FOR UPDATE OF j SKIP LOCKED LIMIT 20").fetch_all(&mut *tx).await?;
        for (job, op) in exhausted {
            let started = phase(&mut tx, op).await? != PrWritePhase::NotStarted;
            let receipt =
                sqlx::query_as::<_, Frozen>("SELECT * FROM pr_sync_intents WHERE operation_id=$1")
                    .bind(op)
                    .fetch_one(&mut *tx)
                    .await?
                    .restore()?;
            audit(
                &mut tx,
                receipt.intent.source(),
                "PR_SYNC_EXHAUSTED",
                if started {
                    "RECONCILIATION_REQUIRED"
                } else {
                    "FAILED"
                },
            )
            .await?;
            sqlx::query("UPDATE jobs SET state='FAILED',lease_owner=NULL,leased_until=NULL,result=jsonb_build_object('phase',$2::text,'error_code','PR_SYNC_ATTEMPTS_EXHAUSTED') WHERE id=$1")
                .bind(job).bind(if started {"RECONCILIATION_REQUIRED"}else{"FAILED"}).execute(&mut *tx).await?;
            sqlx::query("UPDATE git_operations SET state=$2,error_code='PR_SYNC_ATTEMPTS_EXHAUSTED',updated_at=clock_timestamp() WHERE id=$1")
                .bind(op).bind(if started {"UNKNOWN"}else{"FAILED"}).execute(&mut *tx).await?;
        }
        let claimed:Option<(Uuid,i32)>=sqlx::query_as("WITH candidate AS (SELECT j.id FROM jobs j JOIN pr_sync_intents i ON i.job_id=j.id JOIN git_operations o ON o.id=i.operation_id WHERE j.kind='SYNC_REVIEW_PR' AND o.kind='PR_SYNC' AND o.state IN ('PENDING','RUNNING','UNKNOWN') AND j.attempts<j.max_attempts AND j.available_at<=clock_timestamp() AND (j.state='QUEUED' OR (j.state='RUNNING' AND j.leased_until<=clock_timestamp())) ORDER BY j.created_at,j.id FOR UPDATE OF j SKIP LOCKED LIMIT 1) UPDATE jobs j SET state='RUNNING',lease_owner=$1,leased_until=clock_timestamp()+make_interval(secs=>$2),heartbeat_at=clock_timestamp(),attempts=attempts+1,result=jsonb_build_object('phase','OBSERVING') FROM candidate c WHERE j.id=c.id RETURNING j.id,j.attempts")
            .bind(worker).bind(f64::from(seconds)).fetch_optional(&mut *tx).await?;
        let Some((job, attempt)) = claimed else {
            tx.commit().await?;
            return Ok(None);
        };
        let receipt = sqlx::query_as::<_, Frozen>("SELECT * FROM pr_sync_intents WHERE job_id=$1")
            .bind(job)
            .fetch_one(&mut *tx)
            .await?
            .restore()?;
        let lease = PrSyncLease {
            intent: receipt.intent,
            job,
            worker,
            attempt,
        };
        if phase(&mut tx, lease.operation_id()).await? != PrWritePhase::NotStarted {
            sqlx::query("UPDATE git_operations SET state='UNKNOWN',updated_at=clock_timestamp() WHERE id=$1 AND state='RUNNING'").bind(lease.operation_id()).execute(&mut *tx).await?;
        }
        lock_lease(&mut tx, &lease).await?;
        tx.commit().await?;
        Ok(Some(lease))
    }
    pub async fn check(&self, lease: &PrSyncLease) -> Result<PrSyncObservation, PrSyncStoreError> {
        let mut tx = self.transaction().await?;
        let s = lease.intent.source();
        lock_proposal(&mut tx, s).await?;
        lock_lease(&mut tx, lease).await?;
        authorize(&mut tx, lease).await?;
        let result = PrSyncObservation {
            phase: phase(&mut tx, s.operation).await?,
            binding: binding(&mut tx, s)
                .await?
                .map(|b| b.identity(s))
                .transpose()?,
        };
        lock_lease(&mut tx, lease).await?;
        tx.commit().await?;
        Ok(result)
    }
    /// Read-only recovery may inspect the original bound repository after the
    /// proposal advances/closes. It cannot authorize another write or approval.
    pub async fn observe(
        &self,
        lease: &PrSyncLease,
    ) -> Result<PrSyncObservation, PrSyncStoreError> {
        let mut tx = self.transaction().await?;
        let s = lease.intent.source();
        lock_proposal(&mut tx, s).await?;
        lock_lease(&mut tx, lease).await?;
        observe_authority(&mut tx, lease).await?;
        let result = PrSyncObservation {
            phase: phase(&mut tx, s.operation).await?,
            binding: binding(&mut tx, s)
                .await?
                .map(|b| b.identity(s))
                .transpose()?,
        };
        lock_lease(&mut tx, lease).await?;
        tx.commit().await?;
        Ok(result)
    }
    pub async fn heartbeat(
        &self,
        lease: &PrSyncLease,
        seconds: i32,
    ) -> Result<(), PrSyncStoreError> {
        if !(5..=300).contains(&seconds) {
            return Err(PrSyncStoreError::InvalidQuery);
        }
        let mut tx = self.transaction().await?;
        lock_proposal(&mut tx, lease.intent.source()).await?;
        lock_lease(&mut tx, lease).await?;
        if phase(&mut tx, lease.operation_id()).await? == PrWritePhase::NotStarted {
            authorize(&mut tx, lease).await?;
        } else {
            observe_authority(&mut tx, lease).await?;
        }
        let changed=sqlx::query("UPDATE jobs SET leased_until=clock_timestamp()+make_interval(secs=>$2),heartbeat_at=clock_timestamp() WHERE id=$1 AND lease_owner=$3 AND attempts=$4 AND state='RUNNING' AND leased_until>clock_timestamp()")
            .bind(lease.job).bind(f64::from(seconds)).bind(lease.worker).bind(lease.attempt).execute(&mut *tx).await?.rows_affected();
        if changed != 1 {
            return Err(PrSyncStoreError::LeaseLost);
        }
        tx.commit().await?;
        Ok(())
    }
    /// Caller supplies complete bounded discovery and freshly checked provider
    /// refs. The journal independently rechecks local scope and durable PR binding.
    pub async fn start_write(
        &self,
        lease: &PrSyncLease,
        observed: &[PullRequest],
    ) -> Result<PrWritePhase, PrSyncStoreError> {
        let mut tx = self.transaction().await?;
        let s = lease.intent.source();
        lock_proposal(&mut tx, s).await?;
        lock_lease(&mut tx, lease).await?;
        authorize(&mut tx, lease).await?;
        if phase(&mut tx, s.operation).await? != PrWritePhase::NotStarted {
            return Err(PrSyncStoreError::WriteAlreadyStarted);
        }
        let bound = binding(&mut tx, s)
            .await?
            .map(|b| b.identity(s))
            .transpose()?;
        let (kind, number, id, result) =
            match lease
                .intent
                .decide(observed, bound, PrWritePhase::NotStarted)
            {
                PrSyncDecision::CreateDraft => ("CREATE", None, None, PrWritePhase::CreateStarted),
                PrSyncDecision::UpdateMetadata(number) => {
                    let b = bound.ok_or(PrSyncStoreError::InvalidReceipt)?;
                    (
                        "UPDATE",
                        Some(number.get() as i64),
                        Some(b.provider_id.to_string()),
                        PrWritePhase::UpdateStarted,
                    )
                }
                _ => return Err(PrSyncStoreError::InvalidReceipt),
            };
        sqlx::query("INSERT INTO pr_sync_writes(operation_id,kind,pr_number,provider_id,fence_worker,fence_attempt) VALUES($1,$2,$3,$4,$5,$6)")
            .bind(s.operation).bind(kind).bind(number).bind(id).bind(lease.worker).bind(lease.attempt).execute(&mut *tx).await?;
        sqlx::query("UPDATE git_operations SET state='RUNNING',updated_at=clock_timestamp() WHERE id=$1 AND state='PENDING'").bind(s.operation).execute(&mut *tx).await?;
        audit(&mut tx, s, "PR_SYNC_WRITE_STARTED", kind).await?;
        lock_lease(&mut tx, lease).await?;
        tx.commit().await?;
        Ok(result)
    }
    /// Safe cleanup after lost authority does not grant publication authority.
    /// Started writes remain UNKNOWN, including after automatic retries exhaust.
    pub async fn defer(
        &self,
        lease: &PrSyncLease,
        code: &str,
        retry_after: Option<u32>,
    ) -> Result<bool, PrSyncStoreError> {
        if code.is_empty()
            || code.len() > 100
            || !code
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
            || retry_after.is_some_and(|s| !(1..=86400).contains(&s))
        {
            return Err(PrSyncStoreError::InvalidQuery);
        }
        let mut tx = self.transaction().await?;
        let s = lease.intent.source();
        lock_proposal(&mut tx, s).await?;
        lock_lease(&mut tx, lease).await?;
        let started = phase(&mut tx, s.operation).await? != PrWritePhase::NotStarted;
        let max: i32 = sqlx::query_scalar("SELECT max_attempts FROM jobs WHERE id=$1")
            .bind(lease.job)
            .fetch_one(&mut *tx)
            .await?;
        let retry = retry_after.is_some() && lease.attempt < max;
        audit(&mut tx, s, "PR_SYNC_DEFERRED", code).await?;
        lock_lease(&mut tx, lease).await?;
        let changed=sqlx::query("UPDATE jobs SET state=$2,lease_owner=NULL,leased_until=NULL,available_at=clock_timestamp()+make_interval(secs=>$3),result=jsonb_build_object('phase',$4::text,'error_code',$5::text) WHERE id=$1 AND state='RUNNING' AND lease_owner=$6 AND attempts=$7 AND leased_until>clock_timestamp()")
            .bind(lease.job).bind(if retry {"QUEUED"}else{"FAILED"}).bind(f64::from(retry_after.unwrap_or(0)))
            .bind(if started {"RECONCILIATION_REQUIRED"}else if retry {"RETRY_WAIT"}else{"FAILED"}).bind(code).bind(lease.worker).bind(lease.attempt).execute(&mut *tx).await?.rows_affected();
        if changed != 1 {
            return Err(PrSyncStoreError::LeaseLost);
        }
        sqlx::query("UPDATE git_operations SET state=$2,error_code=$3,updated_at=clock_timestamp() WHERE id=$1")
            .bind(s.operation).bind(if started {"UNKNOWN"}else if retry {"PENDING"}else{"FAILED"}).bind(code).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(retry)
    }
    /// Only an exact owned metadata observation can resolve a write. A changed
    /// proposal resolves as SUPERSEDED while retaining the provider PR identity.
    /// The worker must recheck provider refs around its IO; DB checks cannot
    /// atomically assert external refs or prove a provider observation is fresh.
    pub async fn complete(
        &self,
        lease: &PrSyncLease,
        pr: &PullRequest,
    ) -> Result<PrSyncResolution, PrSyncStoreError> {
        let s = lease.intent.source();
        if pr.repository_id != s.repository.remote_id
            || pr.branch != s.branch
            || pr.id == 0
            || pr.state != PullRequestState::Open
            || !pr.draft
            || pr.title != lease.intent.content().title()
            || pr.body != lease.intent.content().body()
        {
            return Err(PrSyncStoreError::InvalidReceipt);
        }
        let mut tx = self.transaction().await?;
        lock_proposal(&mut tx, s).await?;
        lock_lease(&mut tx, lease).await?;
        if review_submissions::access(&mut tx, s.actor, s.proposal, true).await? != s.organization {
            return Err(PrSyncStoreError::Stale);
        }
        current_binding(&mut tx, s).await?;
        let bound = binding(&mut tx, s).await?;
        if let Some(b) = &bound {
            if b.identity(s)?
                != (PrBinding {
                    number: pr.number,
                    provider_id: pr.id,
                })
                || b.node_id != pr.node_id
                || b.url != pr.url
            {
                return Err(PrSyncStoreError::InvalidReceipt);
            }
        } else if phase(&mut tx, s.operation).await? != PrWritePhase::CreateStarted {
            return Err(PrSyncStoreError::InvalidReceipt);
        }
        let current = match authorize(&mut tx, lease).await {
            Ok(()) => true,
            Err(
                PrSyncStoreError::Stale
                | PrSyncStoreError::Access(SubmissionError::Context(
                    ReviewContextError::Stale
                    | ReviewContextError::StateChanged
                    | ReviewContextError::OwnerNotEligible
                    | ReviewContextError::Busy,
                )),
            ) => false,
            Err(error) => return Err(error),
        };
        let resolution = if current && pr.head == s.head && pr.target == s.target {
            PrSyncResolution::Synced
        } else {
            PrSyncResolution::Superseded
        };
        if bound.is_none() {
            sqlx::query("INSERT INTO proposal_pull_requests(proposal_id,organization_id,repository_id,remote_id,installation_id,branch,pr_number,provider_id,node_id,url,created_operation) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)")
                .bind(s.proposal).bind(s.organization).bind(s.repository.repository).bind(s.repository.remote_id.to_string()).bind(s.repository.installation_id.to_string()).bind(s.branch.as_str()).bind(pr.number.get() as i64).bind(pr.id.to_string()).bind(&pr.node_id).bind(&pr.url).bind(s.operation).execute(&mut *tx).await?;
        }
        sqlx::query("INSERT INTO pr_sync_results(operation_id,disposition,pr_number,provider_id,observed_head,observed_target,request_hash,fence_worker,fence_attempt) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)")
            .bind(s.operation).bind(resolution.as_str()).bind(pr.number.get() as i64).bind(pr.id.to_string()).bind(pr.head.as_str()).bind(pr.target.as_str()).bind(lease.intent.request_hash()).bind(lease.worker).bind(lease.attempt).execute(&mut *tx).await?;
        audit(&mut tx, s, "PR_SYNC_RESOLVED", resolution.as_str()).await?;
        lock_lease(&mut tx, lease).await?;
        let changed=sqlx::query("UPDATE jobs SET state=$2,lease_owner=NULL,leased_until=NULL,result=jsonb_build_object('phase',$3::text,'snapshot_id',$4::uuid,'pr_number',$5::bigint) WHERE id=$1 AND state='RUNNING' AND lease_owner=$6 AND attempts=$7 AND leased_until>clock_timestamp()")
            .bind(lease.job).bind(if resolution==PrSyncResolution::Synced {"SUCCEEDED"}else{"CANCELLED"}).bind(resolution.as_str()).bind(s.snapshot).bind(pr.number.get() as i64).bind(lease.worker).bind(lease.attempt).execute(&mut *tx).await?.rows_affected();
        if changed != 1 {
            return Err(PrSyncStoreError::LeaseLost);
        }
        sqlx::query("UPDATE git_operations SET state='SUCCEEDED',result_sha=$2,error_code=NULL,updated_at=clock_timestamp() WHERE id=$1").bind(s.operation).bind(pr.head.as_str()).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(resolution)
    }
}
