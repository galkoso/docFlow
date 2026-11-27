//! Independent snapshot-publication delivery. Claims/receipts are short DB
//! transactions; readiness provider reads hold no transaction or delivery lock.
use super::*;
use crate::provider_backoff::{self, ConnectionKey};
use specspace_application::{book::BookError, comparison_reader::ComparisonReadError};
use specspace_git_adapter::GitError;
use sqlx::FromRow;
use std::time::Duration;
use tokio::sync::watch;

#[derive(Debug, PartialEq, Eq)]
pub enum CheckDispatchWork {
    Idle,
    Enqueued(Uuid, Uuid),
    Deferred(Uuid),
    Skipped(Uuid),
    Rejected(Uuid),
    LeaseLost(Uuid),
}
#[derive(Clone, FromRow)]
pub(super) struct DispatchLease {
    pub event: Uuid,
    pub organization: Uuid,
    pub proposal: Uuid,
    pub worker: Uuid,
    pub attempt: i32,
}
pub struct ReviewCheckDispatcher {
    journal: Arc<PgReviewChecks>,
    worker: Uuid,
}
struct Outcome {
    state: &'static str,
    reason: &'static str,
    delay: Option<u32>,
    cooldown: Option<u32>,
}
impl Outcome {
    fn wait(reason: &'static str, delay: u32) -> Self {
        Self {
            state: "WAITING",
            reason,
            delay: Some(delay.max(1)),
            cooldown: None,
        }
    }
    fn terminal(state: &'static str, reason: &'static str) -> Self {
        Self {
            state,
            reason,
            delay: None,
            cooldown: None,
        }
    }
}
impl ReviewCheckDispatcher {
    pub fn new(journal: Arc<PgReviewChecks>) -> Self {
        Self {
            journal,
            worker: Uuid::new_v4(),
        }
    }
    pub async fn run_once(&self) -> Result<CheckDispatchWork, CheckStoreError> {
        let Some(lease) = claim(&self.journal, self.worker).await? else {
            return Ok(CheckDispatchWork::Idle);
        };
        let work = tokio::time::timeout(Duration::from_secs(90), self.process(&lease)).await;
        let outcome = match work {
            Ok(Ok(Some(job))) => return Ok(CheckDispatchWork::Enqueued(lease.event, job)),
            Ok(Ok(None)) => Outcome::terminal("REJECTED", "PUBLICATION_INVALID"),
            Ok(Err(CheckStoreError::LeaseLost)) => {
                return Ok(CheckDispatchWork::LeaseLost(lease.event));
            }
            Ok(Err(e)) => classify(e),
            Err(_) => Outcome::wait("EVALUATION_TIMEOUT", 30),
        };
        finish(&self.journal, &lease, outcome).await
    }
    async fn process(&self, lease: &DispatchLease) -> Result<Option<Uuid>, CheckStoreError> {
        let mut conn = self.journal.pool.acquire().await?;
        let Some((actor, snapshot)) = publication(&mut conn, lease).await? else {
            return Ok(None);
        };
        let current: bool = sqlx::query_scalar("SELECT pr_sync_snapshot_current($1,$2,$3)")
            .bind(lease.organization)
            .bind(lease.proposal)
            .bind(snapshot)
            .fetch_one(&mut *conn)
            .await?;
        if !current {
            return Err(CheckStoreError::Stale);
        }
        drop(conn);
        let receipt = self
            .journal
            .evaluate_inner(actor, lease.proposal, snapshot, lease.event, Some(lease))
            .await?;
        Ok(Some(receipt.job_id))
    }
    pub async fn run(&self, mut stop: watch::Receiver<bool>) {
        loop {
            if *stop.borrow() {
                break;
            }
            let result = tokio::select! {biased;_=stop.wait_for(|s|*s)=>break,v=self.run_once()=>v};
            match result {
                Ok(CheckDispatchWork::Idle) => {}
                Ok(work) => tracing::info!(?work, "review check publication event processed"),
                Err(error) => tracing::warn!(?error, "review check dispatch unavailable"),
            }
            tokio::select! {_=stop.wait_for(|s|*s)=>break,_=tokio::time::sleep(Duration::from_secs(2))=>{}}
        }
    }
}
async fn claim(
    journal: &PgReviewChecks,
    worker: Uuid,
) -> Result<Option<DispatchLease>, CheckStoreError> {
    let mut tx = journal.transaction().await?;
    let candidate:Option<(Uuid,Uuid,Uuid)>=sqlx::query_as("SELECT e.id,e.organization_id,e.entity_id FROM outbox e LEFT JOIN review_check_dispatch d ON d.event_id=e.id WHERE e.event_type='REVIEW_SNAPSHOT_PUBLISHED' AND (d.event_id IS NULL OR (d.state='WAITING' AND d.next_at<=clock_timestamp()) OR (d.state='RUNNING' AND d.leased_until<=clock_timestamp())) ORDER BY coalesce(d.next_at,d.leased_until,e.created_at),e.created_at,e.id LIMIT 1")
        .fetch_optional(&mut *tx).await?;
    let Some((event, organization, proposal)) = candidate else {
        tx.commit().await?;
        return Ok(None);
    };
    let locked: bool =
        sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!("review-check-dispatch:{event}"))
            .fetch_one(&mut *tx)
            .await?;
    if !locked {
        tx.commit().await?;
        return Ok(None);
    }
    let prior:Option<(String,i32,bool)>=sqlx::query_as("SELECT state,attempts,(state='WAITING' AND next_at<=clock_timestamp()) OR (state='RUNNING' AND leased_until<=clock_timestamp()) FROM review_check_dispatch WHERE event_id=$1 FOR UPDATE")
        .bind(event).fetch_optional(&mut *tx).await?;
    let existed = prior.is_some();
    let attempt = match prior {
        Some((_, n, true)) => n.saturating_add(1),
        Some(_) => {
            tx.commit().await?;
            return Ok(None);
        }
        None => 1,
    };
    if attempt == i32::MAX {
        return Err(CheckStoreError::InvalidRecord);
    }
    let lease = DispatchLease {
        event,
        organization,
        proposal,
        worker,
        attempt,
    };
    let audit = Uuid::new_v4();
    if existed {
        sqlx::query("UPDATE review_check_dispatch SET state='RUNNING',reason='CLAIMED',attempts=$2,lease_owner=$3,leased_until=clock_timestamp()+interval '120 seconds',next_at=NULL,operation_id=NULL,audit_id=$4,updated_at=clock_timestamp() WHERE event_id=$1")
            .bind(event).bind(attempt).bind(worker).bind(audit).execute(&mut *tx).await?;
    } else {
        sqlx::query("INSERT INTO review_check_dispatch(event_id,organization_id,proposal_id,state,reason,attempts,lease_owner,leased_until,audit_id) VALUES($1,$2,$3,'RUNNING','CLAIMED',$4,$5,clock_timestamp()+interval '120 seconds',$6)")
            .bind(event).bind(organization).bind(proposal).bind(attempt).bind(worker).bind(audit).execute(&mut *tx).await?;
    }
    audit_in(&mut tx, &lease, audit, "RUNNING", "CLAIMED", None).await?;
    tx.commit().await?;
    Ok(Some(lease))
}
async fn publication(
    conn: &mut PgConnection,
    lease: &DispatchLease,
) -> Result<Option<(Uuid, Uuid)>, CheckStoreError> {
    let row:Option<(Option<Uuid>,Option<Uuid>)>=sqlx::query_as("SELECT CASE WHEN octet_length(payload::text)<=8192 THEN pr_sync_publication_actor(id) END,CASE WHEN payload->>'snapshot_id'~'^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$' THEN (payload->>'snapshot_id')::uuid END FROM outbox WHERE id=$1 AND organization_id=$2 AND entity_id=$3 AND event_type='REVIEW_SNAPSHOT_PUBLISHED'")
        .bind(lease.event).bind(lease.organization).bind(lease.proposal).fetch_optional(conn).await?;
    Ok(match row {
        Some((Some(actor), Some(snapshot))) => Some((actor, snapshot)),
        _ => None,
    })
}
pub(super) async fn fence(
    conn: &mut PgConnection,
    lease: &DispatchLease,
) -> Result<(), CheckStoreError> {
    let found:Option<Uuid>=sqlx::query_scalar("SELECT event_id FROM review_check_dispatch WHERE event_id=$1 AND organization_id=$2 AND proposal_id=$3 AND state='RUNNING' AND lease_owner=$4 AND attempts=$5 AND leased_until>clock_timestamp() FOR SHARE")
        .bind(lease.event).bind(lease.organization).bind(lease.proposal).bind(lease.worker).bind(lease.attempt).fetch_optional(conn).await?;
    if found.is_none() {
        return Err(CheckStoreError::LeaseLost);
    }
    Ok(())
}
pub(super) async fn enqueued_in(
    conn: &mut PgConnection,
    lease: &DispatchLease,
) -> Result<(), CheckStoreError> {
    fence(conn, lease).await?;
    let audit = Uuid::new_v4();
    let changed=sqlx::query("UPDATE review_check_dispatch SET state='ENQUEUED',reason='QUEUED',lease_owner=NULL,leased_until=NULL,next_at=NULL,operation_id=$1,audit_id=$2,updated_at=clock_timestamp() WHERE event_id=$1 AND state='RUNNING' AND lease_owner=$3 AND attempts=$4 AND leased_until>clock_timestamp()")
        .bind(lease.event).bind(audit).bind(lease.worker).bind(lease.attempt).execute(&mut *conn).await?.rows_affected();
    if changed != 1 {
        return Err(CheckStoreError::LeaseLost);
    }
    audit_in(conn, lease, audit, "ENQUEUED", "QUEUED", Some(lease.event)).await
}
async fn finish(
    journal: &PgReviewChecks,
    lease: &DispatchLease,
    mut outcome: Outcome,
) -> Result<CheckDispatchWork, CheckStoreError> {
    let mut tx = journal.transaction().await?;
    let row:Option<(String,bool)>=sqlx::query_as("SELECT state,coalesce(lease_owner=$2 AND attempts=$3 AND leased_until>clock_timestamp(),false) FROM review_check_dispatch WHERE event_id=$1 FOR UPDATE")
        .bind(lease.event).bind(lease.worker).bind(lease.attempt).fetch_optional(&mut *tx).await?;
    if row.as_ref().is_some_and(|(state, _)| state == "ENQUEUED") {
        let receipt = read::load(&mut tx, lease.organization, lease.event)
            .await?
            .ok_or(CheckStoreError::InvalidRecord)?;
        if receipt.source_event != Some(lease.event)
            || receipt.intent.source().proposal != lease.proposal
        {
            return Err(CheckStoreError::InvalidRecord);
        }
        tx.commit().await?;
        return Ok(CheckDispatchWork::Enqueued(lease.event, receipt.job_id));
    }
    if !row.is_some_and(|(state, owned)| state == "RUNNING" && owned) {
        tx.commit().await?;
        return Ok(CheckDispatchWork::LeaseLost(lease.event));
    }
    // A read may have gone stale while the event was being processed. Only
    // superseded backed publications may receive a terminal skip receipt.
    if outcome.state == "SKIPPED" {
        if let Some((_, snapshot)) = publication(&mut tx, lease).await? {
            let current: bool = sqlx::query_scalar("SELECT pr_sync_snapshot_current($1,$2,$3)")
                .bind(lease.organization)
                .bind(lease.proposal)
                .bind(snapshot)
                .fetch_one(&mut *tx)
                .await?;
            if current {
                outcome = Outcome::wait("CONTEXT_CHANGED", 30);
            }
        } else {
            outcome = Outcome::terminal("REJECTED", "PUBLICATION_INVALID");
        }
    }
    if let Some(seconds) = outcome.cooldown
        && let Some((_, snapshot)) = publication(&mut tx, lease).await?
    {
        let(repository,remote,install):(Uuid,String,String)=sqlx::query_as("SELECT repository_id,remote_id,installation_id FROM review_snapshots WHERE organization_id=$1 AND proposal_id=$2 AND id=$3")
            .bind(lease.organization).bind(lease.proposal).bind(snapshot).fetch_one(&mut *tx).await?;
        let remote = remote
            .parse::<u64>()
            .map_err(|_| CheckStoreError::InvalidRecord)?
            .to_string();
        let install = install
            .parse::<u64>()
            .map_err(|_| CheckStoreError::InvalidRecord)?
            .to_string();
        provider_backoff::defer(
            &mut tx,
            &ConnectionKey {
                organization: lease.organization,
                repository,
                remote_id: &remote,
                installation_id: &install,
            },
            seconds,
        )
        .await?;
    }
    let audit = Uuid::new_v4();
    let changed=sqlx::query("UPDATE review_check_dispatch SET state=$2,reason=$3,next_at=CASE WHEN $4::double precision IS NULL THEN NULL ELSE clock_timestamp()+make_interval(secs=>$4) END,lease_owner=NULL,leased_until=NULL,audit_id=$5,updated_at=clock_timestamp() WHERE event_id=$1 AND state='RUNNING' AND lease_owner=$6 AND attempts=$7 AND leased_until>clock_timestamp()")
        .bind(lease.event).bind(outcome.state).bind(outcome.reason).bind(outcome.delay.map(f64::from)).bind(audit).bind(lease.worker).bind(lease.attempt).execute(&mut *tx).await?.rows_affected();
    if changed != 1 {
        return Err(CheckStoreError::LeaseLost);
    }
    audit_in(&mut tx, lease, audit, outcome.state, outcome.reason, None).await?;
    let result = match outcome.state {
        "SKIPPED" => CheckDispatchWork::Skipped(lease.event),
        "REJECTED" => CheckDispatchWork::Rejected(lease.event),
        _ => CheckDispatchWork::Deferred(lease.event),
    };
    tx.commit().await?;
    Ok(result)
}
async fn audit_in(
    conn: &mut PgConnection,
    lease: &DispatchLease,
    id: Uuid,
    state: &str,
    reason: &str,
    operation: Option<Uuid>,
) -> Result<(), CheckStoreError> {
    sqlx::query("INSERT INTO audit_events(id,organization_id,action,entity_id,after_ref,operation_id,result) VALUES($1,$2,'CHECK_OUTBOX_DISPATCH',$3,$4,$5,$6)")
        .bind(id).bind(lease.organization).bind(lease.proposal).bind(lease.event.to_string()).bind(operation).bind(format!("{state}:{reason}:{}",lease.attempt)).execute(conn).await?;
    Ok(())
}
fn classify(error: CheckStoreError) -> Outcome {
    match error {
        CheckStoreError::Stale => Outcome::terminal("SKIPPED", "SNAPSHOT_SUPERSEDED"),
        CheckStoreError::Forbidden | CheckStoreError::NotFound => {
            Outcome::wait("AUTHORITY_REQUIRED", 300)
        }
        CheckStoreError::NotConfigured => Outcome::wait("SETUP_REQUIRED", 300),
        CheckStoreError::Busy => Outcome::wait("CHECK_LANE_BUSY", 30),
        CheckStoreError::Quota => Outcome::wait("ADMISSION_QUOTA", 60),
        CheckStoreError::Cooldown(n) => Outcome::wait("PROVIDER_COOLDOWN", n),
        CheckStoreError::IdempotencyMismatch => Outcome::terminal("REJECTED", "OPERATION_CONFLICT"),
        CheckStoreError::Fresh(e) => fresh(e),
        _ => Outcome::wait("JOURNAL_UNAVAILABLE", 30),
    }
}
fn fresh(error: FreshReadinessError) -> Outcome {
    match error {
        FreshReadinessError::Provider(e)
        | FreshReadinessError::Connection(ComparisonReadError::Provider(e)) => provider(e),
        FreshReadinessError::Connection(ComparisonReadError::Source(BookError::Provider(e))) => {
            provider(e)
        }
        FreshReadinessError::Connection(ComparisonReadError::Source(
            BookError::RepositoryNotConfigured,
        )) => Outcome::wait("SETUP_REQUIRED", 300),
        FreshReadinessError::ContextChanged
        | FreshReadinessError::HeadChanged
        | FreshReadinessError::TargetChanged
        | FreshReadinessError::MergeBaseChanged => Outcome::wait("SOURCE_CHANGED", 30),
        _ => Outcome::wait("READINESS_UNAVAILABLE", 30),
    }
}
fn provider(error: GitError) -> Outcome {
    match error {
        GitError::RateLimited {
            retry_after_seconds,
        } => throttle(retry_after_seconds),
        GitError::ResultUnknownWithRetryAfter {
            retry_after_seconds,
        } => throttle(Some(retry_after_seconds)),
        GitError::Unauthorized => Outcome::wait("PROVIDER_AUTHORITY_REQUIRED", 300),
        _ => Outcome::wait("PROVIDER_UNAVAILABLE", 30),
    }
}
fn throttle(seconds: Option<u64>) -> Outcome {
    let seconds = u32::try_from(seconds.unwrap_or(60).max(1)).unwrap_or(u32::MAX);
    let mut outcome = Outcome::wait("PROVIDER_RATE_LIMITED", seconds);
    outcome.cooldown = Some(seconds);
    outcome
}
