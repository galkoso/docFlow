//! Lease-fenced CREATE journal. Recovered mutations are observed, never blindly
//! repeated. No transaction in this module spans a provider call.
use specspace_domain::BranchName;
use specspace_git_adapter::CommitSha;
use sqlx::{FromRow, PgConnection, PgPool};
use uuid::Uuid;

#[derive(Debug, PartialEq, Eq)]
pub enum ProvisionError {
    Unavailable,
    InvalidLease,
    InvalidJob,
    LeaseLost,
    AccessRevoked,
}
impl From<sqlx::Error> for ProvisionError {
    fn from(_: sqlx::Error) -> Self {
        Self::Unavailable
    }
}

#[derive(Clone, Copy)]
pub enum FailureCertainty {
    ObserveJournal,
    DefinitelyNotApplied,
}

#[derive(FromRow)]
struct RawPlan {
    operation_id: Uuid,
    repository_id: Uuid,
    proposal_id: Uuid,
    actor_id: Uuid,
    owner_id: Uuid,
    team_id: Uuid,
    branch: String,
    expected_sha: Option<String>,
    remote_id: String,
    installation_id: String,
    operation_state: String,
    proposal_state: String,
}
struct Plan {
    operation: Uuid,
    repository: Uuid,
    proposal: Uuid,
    actor: Uuid,
    owner: Uuid,
    team: Uuid,
    branch: BranchName,
    source: CommitSha,
    remote_id: String,
    installation_id: String,
    reconcile: bool,
}
impl TryFrom<RawPlan> for Plan {
    type Error = ProvisionError;
    fn try_from(raw: RawPlan) -> Result<Self, Self::Error> {
        if raw.proposal_state != "PROVISIONING"
            || !matches!(
                raw.operation_state.as_str(),
                "PENDING" | "RUNNING" | "UNKNOWN"
            )
        {
            return Err(ProvisionError::InvalidJob);
        }
        Ok(Self {
            operation: raw.operation_id,
            repository: raw.repository_id,
            proposal: raw.proposal_id,
            actor: raw.actor_id,
            owner: raw.owner_id,
            team: raw.team_id,
            branch: BranchName::parse(raw.branch).map_err(|_| ProvisionError::InvalidJob)?,
            source: CommitSha::parse(raw.expected_sha.ok_or(ProvisionError::InvalidJob)?)
                .map_err(|_| ProvisionError::InvalidJob)?,
            remote_id: raw.remote_id,
            installation_id: raw.installation_id,
            reconcile: raw.operation_state != "PENDING",
        })
    }
}
pub struct ProvisionLease {
    pub id: Uuid,
    pub attempts: i32,
    organization: Uuid,
    worker: Uuid,
    plan: Plan,
}
impl ProvisionLease {
    pub fn repository(&self) -> Uuid {
        self.plan.repository
    }
    pub fn proposal(&self) -> Uuid {
        self.plan.proposal
    }
    pub fn branch(&self) -> &BranchName {
        &self.plan.branch
    }
    pub fn source(&self) -> &CommitSha {
        &self.plan.source
    }
    pub fn needs_reconciliation(&self) -> bool {
        self.plan.reconcile
    }
}

pub struct PgProvisioning {
    pool: PgPool,
}
impl PgProvisioning {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
    pub async fn claim(
        &self,
        worker: Uuid,
        seconds: i32,
    ) -> Result<Option<ProvisionLease>, ProvisionError> {
        if !(1..=3600).contains(&seconds) {
            return Err(ProvisionError::InvalidLease);
        }
        // Exhaustion is terminal and visible, including an uncertain ref. Never
        // convert an expired mutation back to a fresh PENDING operation.
        sqlx::query("WITH stale AS (SELECT j.id,coalesce(o.state IN ('RUNNING','UNKNOWN'),false) AS uncertain FROM jobs j LEFT JOIN git_operations o ON o.id=j.operation_id AND o.organization_id=j.organization_id WHERE j.kind='CREATE_PROPOSAL' AND j.attempts>=j.max_attempts AND (j.state='QUEUED' OR (j.state='RUNNING' AND j.leased_until<=clock_timestamp())) ORDER BY j.created_at,j.id FOR UPDATE OF j SKIP LOCKED LIMIT 100), failed AS (UPDATE jobs j SET state='FAILED',lease_owner=NULL,leased_until=NULL,result=jsonb_build_object('error_code',CASE WHEN s.uncertain THEN 'CREATE_RESULT_UNKNOWN' ELSE 'PROVISIONING_ATTEMPTS_EXHAUSTED' END,'phase',CASE WHEN s.uncertain THEN 'RECONCILIATION_REQUIRED' ELSE 'PREFLIGHT' END) FROM stale s WHERE j.id=s.id RETURNING j.organization_id,j.operation_id), ops AS (UPDATE git_operations o SET state=CASE WHEN o.state='PENDING' THEN 'FAILED' ELSE 'UNKNOWN' END,error_code=CASE WHEN o.state='PENDING' THEN 'PROVISIONING_ATTEMPTS_EXHAUSTED' ELSE 'CREATE_RESULT_UNKNOWN' END,updated_at=clock_timestamp() FROM failed f WHERE o.id=f.operation_id AND o.organization_id=f.organization_id AND o.kind='CREATE' AND o.state IN ('PENDING','RUNNING','UNKNOWN') RETURNING o.*) INSERT INTO audit_events(id,organization_id,actor_id,action,entity_id,operation_id,result) SELECT gen_random_uuid(),organization_id,actor_id,'PROPOSAL_PROVISIONING_EXHAUSTED',proposal_id,id,state FROM ops WHERE proposal_id IS NOT NULL")
            .execute(&self.pool).await?;
        let mut tx = self.pool.begin().await?;
        let job = sqlx::query_as::<_, crate::LeasedJob>("WITH candidate AS (SELECT id FROM jobs WHERE kind='CREATE_PROPOSAL' AND attempts<max_attempts AND ((state='QUEUED' AND available_at<=clock_timestamp()) OR (state='RUNNING' AND leased_until<=clock_timestamp())) ORDER BY created_at,id FOR UPDATE SKIP LOCKED LIMIT 1) UPDATE jobs SET state='RUNNING',lease_owner=$1,leased_until=clock_timestamp()+make_interval(secs=>$2),heartbeat_at=clock_timestamp(),attempts=attempts+1 FROM candidate WHERE jobs.id=candidate.id RETURNING jobs.id,organization_id,operation_id,kind,payload,attempts")
            .bind(worker).bind(f64::from(seconds)).fetch_optional(&mut *tx).await?;
        let Some(job) = job else {
            return Ok(None);
        };
        let raw = sqlx::query_as::<_, RawPlan>("SELECT o.id AS operation_id,p.repository_id,p.id AS proposal_id,o.actor_id,p.owner_id,p.team_id,p.branch,o.expected_sha,r.remote_id,r.installation_id,o.state AS operation_state,p.state AS proposal_state FROM jobs j JOIN git_operations o ON o.id=j.operation_id AND o.organization_id=j.organization_id AND o.kind='CREATE' JOIN proposals p ON p.id=o.proposal_id AND p.organization_id=o.organization_id JOIN repositories r ON r.id=p.repository_id AND r.organization_id=p.organization_id WHERE j.id=$1 AND j.organization_id=$2 AND j.payload=jsonb_build_object('proposal_id',p.id,'repository_id',p.repository_id,'expected_source_sha',o.expected_sha,'remote_id',r.remote_id,'installation_id',r.installation_id) AND r.provider='GITHUB' AND r.default_branch='main' FOR UPDATE OF o,p")
            .bind(job.id).bind(job.organization_id).fetch_optional(&mut *tx).await?;
        let plan = raw
            .ok_or(ProvisionError::InvalidJob)
            .and_then(Plan::try_from);
        let plan = match plan {
            Ok(plan) => plan,
            Err(error) => {
                sqlx::query("UPDATE jobs SET state='FAILED',result=jsonb_build_object('error_code','INVALID_PROVISIONING_JOB'),lease_owner=NULL,leased_until=NULL WHERE id=$1")
                    .bind(job.id).execute(&mut *tx).await?;
                tx.commit().await?;
                return Err(error);
            }
        };
        if plan.reconcile {
            sqlx::query("UPDATE git_operations SET state='UNKNOWN',updated_at=clock_timestamp() WHERE id=$1")
                .bind(plan.operation).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(Some(ProvisionLease {
            id: job.id,
            attempts: job.attempts,
            organization: job.organization_id,
            worker,
            plan,
        }))
    }

    pub async fn heartbeat(
        &self,
        lease: &ProvisionLease,
        seconds: i32,
    ) -> Result<(), ProvisionError> {
        if !(1..=3600).contains(&seconds) {
            return Err(ProvisionError::InvalidLease);
        }
        let n = sqlx::query("UPDATE jobs SET leased_until=clock_timestamp()+make_interval(secs=>$4),heartbeat_at=clock_timestamp() WHERE id=$1 AND organization_id=$2 AND lease_owner=$3 AND kind='CREATE_PROPOSAL' AND state='RUNNING' AND leased_until>clock_timestamp()")
            .bind(lease.id).bind(lease.organization).bind(lease.worker).bind(f64::from(seconds)).execute(&self.pool).await?.rows_affected();
        fenced(n)
    }
    pub async fn authorize(&self, lease: &ProvisionLease) -> Result<(), ProvisionError> {
        let mut connection = self.pool.acquire().await?;
        eligible(&mut connection, lease).await
    }
    pub async fn begin_mutation(&self, lease: &ProvisionLease) -> Result<(), ProvisionError> {
        let mut tx = self.pool.begin().await?;
        lock_job(&mut tx, lease).await?;
        eligible(&mut tx, lease).await?;
        let n = sqlx::query("UPDATE git_operations SET state='RUNNING',updated_at=clock_timestamp() WHERE id=$1 AND organization_id=$2 AND state='PENDING'")
            .bind(lease.plan.operation).bind(lease.organization).execute(&mut *tx).await?.rows_affected();
        if n != 1 {
            return Err(ProvisionError::InvalidJob);
        }
        audit(&mut tx, lease, "BRANCH_CREATE_STARTED", "RUNNING").await?;
        // Fence again at commit boundary, including time spent waiting on ACL locks.
        lock_job(&mut tx, lease).await?;
        tx.commit().await?;
        Ok(())
    }
    pub async fn complete(
        &self,
        lease: &ProvisionLease,
        actual: &CommitSha,
    ) -> Result<(), ProvisionError> {
        if actual != lease.source() {
            return Err(ProvisionError::InvalidJob);
        }
        let mut tx = self.pool.begin().await?;
        lock_job(&mut tx, lease).await?;
        eligible(&mut tx, lease).await?;
        let n = sqlx::query("UPDATE git_operations SET state='SUCCEEDED',result_sha=$3,error_code=NULL,updated_at=clock_timestamp() WHERE id=$1 AND organization_id=$2 AND state IN ('RUNNING','UNKNOWN')")
            .bind(lease.plan.operation).bind(lease.organization).bind(actual.as_str()).execute(&mut *tx).await?.rows_affected();
        if n != 1 {
            return Err(ProvisionError::InvalidJob);
        }
        let n = sqlx::query("UPDATE proposals SET state='DRAFT',base_sha=$3,head_sha=$3,target_sha=$3,version=version+1,updated_at=clock_timestamp() WHERE id=$1 AND organization_id=$2 AND state='PROVISIONING'")
            .bind(lease.plan.proposal).bind(lease.organization).bind(actual.as_str()).execute(&mut *tx).await?.rows_affected();
        if n != 1 {
            return Err(ProvisionError::InvalidJob);
        }
        let result = serde_json::json!({"proposal_id":lease.plan.proposal,"branch":lease.branch().as_str(),"head_sha":actual.as_str(),"phase":"READY","reconciled":lease.needs_reconciliation()});
        let n = sqlx::query("UPDATE jobs SET state='SUCCEEDED',result=$4,lease_owner=NULL,leased_until=NULL WHERE id=$1 AND organization_id=$2 AND lease_owner=$3 AND state='RUNNING' AND leased_until>clock_timestamp()")
            .bind(lease.id).bind(lease.organization).bind(lease.worker).bind(&result).execute(&mut *tx).await?.rows_affected();
        fenced(n)?;
        audit(
            &mut tx,
            lease,
            "PROPOSAL_CREATED",
            if lease.needs_reconciliation() {
                "RECONCILED"
            } else {
                "SUCCEEDED"
            },
        )
        .await?;
        sqlx::query("INSERT INTO outbox(id,organization_id,event_type,entity_id,payload) VALUES($1,$2,'PROPOSAL_CREATED',$3,$4)")
            .bind(Uuid::new_v4()).bind(lease.organization).bind(lease.plan.proposal).bind(result).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }
    /// Retry is allowed for safe preflight failures or read-only reconciliation.
    /// A started mutation becomes UNKNOWN unless the provider definitely rejected it.
    pub async fn record_failure(
        &self,
        lease: &ProvisionLease,
        code: &'static str,
        retry_after: Option<u32>,
        certainty: FailureCertainty,
    ) -> Result<bool, ProvisionError> {
        if retry_after.is_some_and(|seconds| !(1..=86400).contains(&seconds)) {
            return Err(ProvisionError::InvalidLease);
        }
        let mut tx = self.pool.begin().await?;
        let (attempts, max_attempts) = lock_job(&mut tx, lease).await?;
        let state: String = sqlx::query_scalar(
            "SELECT state FROM git_operations WHERE id=$1 AND organization_id=$2 FOR UPDATE",
        )
        .bind(lease.plan.operation)
        .bind(lease.organization)
        .fetch_one(&mut *tx)
        .await?;
        if !matches!(state.as_str(), "PENDING" | "RUNNING" | "UNKNOWN") {
            return Err(ProvisionError::InvalidJob);
        }
        let uncertain = state == "UNKNOWN"
            || (state == "RUNNING" && matches!(certainty, FailureCertainty::ObserveJournal));
        let retry = retry_after.is_some() && attempts < max_attempts;
        let next = if uncertain {
            "UNKNOWN"
        } else if retry {
            "PENDING"
        } else {
            "FAILED"
        };
        sqlx::query("UPDATE git_operations SET state=$3,error_code=$4,updated_at=clock_timestamp() WHERE id=$1 AND organization_id=$2")
            .bind(lease.plan.operation).bind(lease.organization).bind(next).bind(code).execute(&mut *tx).await?;
        let n = sqlx::query("UPDATE jobs SET state=$4,result=$5,available_at=clock_timestamp()+make_interval(secs=>$6),lease_owner=NULL,leased_until=NULL WHERE id=$1 AND organization_id=$2 AND lease_owner=$3 AND state='RUNNING' AND leased_until>clock_timestamp()")
            .bind(lease.id).bind(lease.organization).bind(lease.worker).bind(if retry { "QUEUED" } else { "FAILED" })
            .bind(serde_json::json!({"error_code":code,"phase":if uncertain { "RECONCILIATION_REQUIRED" } else { "PREFLIGHT" },"proposal_id":lease.plan.proposal}))
            .bind(f64::from(retry_after.unwrap_or(1))).execute(&mut *tx).await?.rows_affected();
        fenced(n)?;
        audit(&mut tx, lease, "PROPOSAL_PROVISIONING_FAILED", next).await?;
        tx.commit().await?;
        Ok(retry)
    }
}

fn fenced(n: u64) -> Result<(), ProvisionError> {
    if n == 1 {
        Ok(())
    } else {
        Err(ProvisionError::LeaseLost)
    }
}
async fn lock_job(
    connection: &mut PgConnection,
    lease: &ProvisionLease,
) -> Result<(i32, i32), ProvisionError> {
    sqlx::query_as("SELECT attempts,max_attempts FROM jobs WHERE id=$1 AND organization_id=$2 AND lease_owner=$3 AND kind='CREATE_PROPOSAL' AND state='RUNNING' AND leased_until>clock_timestamp() FOR UPDATE")
        .bind(lease.id).bind(lease.organization).bind(lease.worker).fetch_optional(connection).await?.ok_or(ProvisionError::LeaseLost)
}
async fn eligible(
    connection: &mut PgConnection,
    lease: &ProvisionLease,
) -> Result<(), ProvisionError> {
    let row: Option<Uuid> = sqlx::query_scalar("SELECT p.id FROM proposals p JOIN repositories r ON r.id=p.repository_id AND r.organization_id=p.organization_id JOIN teams t ON t.id=p.team_id AND t.organization_id=p.organization_id JOIN memberships ma ON ma.organization_id=p.organization_id AND ma.user_id=p.author_id JOIN users a ON a.id=ma.user_id JOIN memberships mo ON mo.organization_id=p.organization_id AND mo.user_id=p.owner_id JOIN users u ON u.id=mo.user_id JOIN git_operations o ON o.proposal_id=p.id AND o.organization_id=p.organization_id WHERE p.id=$1 AND p.organization_id=$2 AND p.repository_id=$3 AND p.author_id=$4 AND p.owner_id=$5 AND p.team_id=$6 AND p.branch=$7 AND p.state='PROVISIONING' AND o.id=$8 AND o.kind='CREATE' AND o.actor_id=$4 AND o.expected_sha=$9 AND r.remote_id=$10 AND r.installation_id=$11 AND r.provider='GITHUB' AND r.default_branch='main' AND a.status='ACTIVE' AND ma.role IN ('CONTRIBUTOR','REVIEWER','MAINTAINER','ADMIN') AND u.status='ACTIVE' AND mo.role IN ('CONTRIBUTOR','REVIEWER','MAINTAINER','ADMIN') FOR SHARE OF p,r,t,ma,a,mo,u,o")
        .bind(lease.plan.proposal).bind(lease.organization).bind(lease.plan.repository).bind(lease.plan.actor)
        .bind(lease.plan.owner).bind(lease.plan.team).bind(lease.branch().as_str()).bind(lease.plan.operation).bind(lease.source().as_str()).bind(&lease.plan.remote_id).bind(&lease.plan.installation_id)
        .fetch_optional(connection).await?;
    if row.is_some() {
        Ok(())
    } else {
        Err(ProvisionError::AccessRevoked)
    }
}
async fn audit(
    connection: &mut PgConnection,
    lease: &ProvisionLease,
    action: &'static str,
    result: &'static str,
) -> Result<(), ProvisionError> {
    sqlx::query("INSERT INTO audit_events(id,organization_id,actor_id,action,entity_id,before_ref,after_ref,operation_id,result) VALUES($1,$2,$3,$4,$5,$6,$6,$7,$8)")
        .bind(Uuid::new_v4()).bind(lease.organization).bind(lease.plan.actor).bind(action).bind(lease.plan.proposal).bind(lease.source().as_str()).bind(lease.plan.operation).bind(result).execute(connection).await?;
    Ok(())
}
