//! Transactional publication-event transport. This module performs no provider
//! IO, grants no approvals, and never describes journal delivery as PR success.
use super::*;
use specspace_application::review_context::ReviewContextError;
use std::{sync::Arc, time::Duration};
use tokio::sync::watch;

#[derive(Debug, PartialEq, Eq)]
pub enum PrDispatchWork {
    Idle,
    Enqueued(Uuid, Uuid),
    Deferred(Uuid),
    Skipped(Uuid),
    Rejected(Uuid),
}
#[derive(Clone, Copy, FromRow)]
struct Envelope {
    id: Uuid,
    organization_id: Uuid,
    entity_id: Uuid,
}
#[derive(FromRow)]
struct Prior {
    state: String,
    reason: String,
    operation_id: Option<Uuid>,
}
struct Outcome {
    state: &'static str,
    reason: &'static str,
    operation: Option<Uuid>,
    delay: Option<u32>,
}
impl Outcome {
    fn enqueued(operation: Uuid) -> Self {
        Self {
            state: "ENQUEUED",
            reason: "QUEUED",
            operation: Some(operation),
            delay: None,
        }
    }
    fn waiting(reason: &'static str, delay: u32) -> Self {
        Self {
            state: "WAITING",
            reason,
            operation: None,
            delay: Some(delay),
        }
    }
    fn skipped() -> Self {
        Self {
            state: "SKIPPED",
            reason: "SNAPSHOT_SUPERSEDED",
            operation: None,
            delay: None,
        }
    }
    fn rejected(reason: &'static str) -> Self {
        Self {
            state: "REJECTED",
            reason,
            operation: None,
            delay: None,
        }
    }
}
pub struct PrSyncDispatcher {
    journal: Arc<PgPrSync>,
    site: PrSiteOrigin,
}
impl PrSyncDispatcher {
    pub fn new(journal: Arc<PgPrSync>, site: PrSiteOrigin) -> Self {
        Self { journal, site }
    }
    pub async fn run_once(&self) -> Result<PrDispatchWork, PrSyncStoreError> {
        self.journal.dispatch_once(&self.site).await
    }
    pub async fn run(&self, mut stop: watch::Receiver<bool>) {
        loop {
            if *stop.borrow() {
                break;
            }
            let result = tokio::select! {r=self.run_once()=>r,_=stop.wait_for(|s|*s)=>break};
            match result {
                Ok(PrDispatchWork::Idle) => {}
                Ok(result) => tracing::info!(?result, "PR publication event processed"),
                Err(error) => tracing::warn!(?error, "PR publication dispatch unavailable"),
            }
            tokio::select! {_=tokio::time::sleep(Duration::from_secs(2))=>{},_=stop.wait_for(|s|*s)=>break}
        }
    }
}
impl PgPrSync {
    pub(super) async fn record_enqueued_in(
        &self,
        conn: &mut PgConnection,
        receipt: &PrSyncReservation,
    ) -> Result<(), PrSyncStoreError> {
        let s = receipt.intent.source();
        record(
            conn,
            &Envelope {
                id: receipt.source_event,
                organization_id: s.organization,
                entity_id: s.proposal,
            },
            &Outcome::enqueued(receipt.operation_id),
        )
        .await
    }
    async fn dispatch_once(&self, site: &PrSiteOrigin) -> Result<PrDispatchWork, PrSyncStoreError> {
        let mut tx = self.transaction().await?;
        let candidate:Option<Envelope>=sqlx::query_as("SELECT e.id,e.organization_id,e.entity_id FROM outbox e LEFT JOIN pr_sync_dispatch d ON d.event_id=e.id WHERE e.event_type='REVIEW_SNAPSHOT_PUBLISHED' AND e.sent_at IS NULL AND (d.event_id IS NULL OR (d.state='WAITING' AND d.next_at<=clock_timestamp())) ORDER BY coalesce(d.next_at,e.created_at),e.created_at,e.id LIMIT 1")
            .fetch_optional(&mut *tx).await?;
        let Some(e) = candidate else {
            tx.commit().await?;
            return Ok(PrDispatchWork::Idle);
        };
        let claimed: bool =
            sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(hashtextextended($1,0))")
                .bind(format!("pr-outbox-dispatch:{}", e.id))
                .fetch_one(&mut *tx)
                .await?;
        if !claimed {
            tx.commit().await?;
            return Ok(PrDispatchWork::Idle);
        }
        // Every admission/dispatcher locks proposal before event. No nested
        // transaction holds an event lock while waiting for the proposal.
        sqlx::query("SELECT id FROM proposals WHERE id=$1 AND organization_id=$2 FOR UPDATE")
            .bind(e.entity_id)
            .bind(e.organization_id)
            .fetch_optional(&mut *tx)
            .await?;
        let row:Option<(Uuid,Uuid,String,Value,bool)>=sqlx::query_as("SELECT organization_id,entity_id,event_type,CASE WHEN octet_length(payload::text)<=8192 THEN payload ELSE 'null'::jsonb END,sent_at IS NOT NULL FROM outbox WHERE id=$1 FOR UPDATE")
            .bind(e.id).fetch_optional(&mut *tx).await?;
        let Some((org, proposal, kind, payload, delivered)) = row else {
            tx.commit().await?;
            return Ok(PrDispatchWork::Idle);
        };
        if org != e.organization_id
            || proposal != e.entity_id
            || kind != "REVIEW_SNAPSHOT_PUBLISHED"
        {
            tx.commit().await?;
            return Ok(PrDispatchWork::Idle);
        }
        let prior: Option<Prior> = sqlx::query_as(
            "SELECT state,reason,operation_id FROM pr_sync_dispatch WHERE event_id=$1 FOR UPDATE",
        )
        .bind(e.id)
        .fetch_optional(&mut *tx)
        .await?;
        if prior.as_ref().is_some_and(|p| p.state != "WAITING") {
            tx.commit().await?;
            return Ok(PrDispatchWork::Idle);
        }
        // Parsing is bounded; full backing checks also require canonical UUIDs,
        // exact flags and a real succeeded source publication/operation/job.
        let parsed = serde_json::from_value::<PublicationEvent>(payload).ok();
        let actor: Option<Uuid> = if parsed.is_some() {
            sqlx::query_scalar("SELECT pr_sync_publication_actor($1)")
                .bind(e.id)
                .fetch_one(&mut *tx)
                .await?
        } else {
            None
        };
        let existing: Option<(Uuid, Uuid)> = if actor.is_some() {
            sqlx::query_as("SELECT i.operation_id,i.job_id FROM pr_sync_intents i JOIN outbox e ON e.id=$1 WHERE i.organization_id=e.organization_id AND i.proposal_id=e.entity_id AND i.source_submission::text=e.payload->>'operation_id'")
                .bind(e.id).fetch_optional(&mut *tx).await?
        } else {
            None
        };
        let (outcome, job) = if let Some((op, job)) = existing {
            (Outcome::enqueued(op), Some(job))
        } else if actor.is_none() {
            (Outcome::rejected("PUBLICATION_INVALID"), None)
        } else if delivered {
            (Outcome::rejected("UNBACKED_DELIVERY"), None)
        } else {
            let input = parsed.ok_or(PrSyncStoreError::InvalidEvent)?;
            let current: bool = sqlx::query_scalar("SELECT pr_sync_snapshot_current($1,$2,$3)")
                .bind(org)
                .bind(proposal)
                .bind(input.snapshot_id)
                .fetch_one(&mut *tx)
                .await?;
            if !current {
                (Outcome::skipped(), None)
            } else {
                sqlx::query("SAVEPOINT pr_outbox_admission")
                    .execute(&mut *tx)
                    .await?;
                match self
                    .reserve_in(
                        &mut tx,
                        actor.ok_or(PrSyncStoreError::InvalidEvent)?,
                        proposal,
                        e.id,
                        site,
                    )
                    .await
                {
                    Ok(receipt) => {
                        sqlx::query("RELEASE SAVEPOINT pr_outbox_admission")
                            .execute(&mut *tx)
                            .await?;
                        (
                            Outcome::enqueued(receipt.operation_id),
                            Some(receipt.job_id),
                        )
                    }
                    Err(error) => {
                        // Roll back partial inserts and release inner scope locks
                        // while retaining the outer proposal/event envelope locks.
                        sqlx::query("ROLLBACK TO SAVEPOINT pr_outbox_admission")
                            .execute(&mut *tx)
                            .await?;
                        (classify(error)?, None)
                    }
                }
            }
        };
        record(&mut tx, &e, &outcome).await?;
        let result = match outcome.state {
            "ENQUEUED" => {
                PrDispatchWork::Enqueued(e.id, job.ok_or(PrSyncStoreError::InvalidIntent)?)
            }
            "SKIPPED" => PrDispatchWork::Skipped(e.id),
            "REJECTED" => PrDispatchWork::Rejected(e.id),
            _ => PrDispatchWork::Deferred(e.id),
        };
        tx.commit().await?;
        Ok(result)
    }
}
async fn record(
    conn: &mut PgConnection,
    e: &Envelope,
    outcome: &Outcome,
) -> Result<(), PrSyncStoreError> {
    let prior: Option<Prior> = sqlx::query_as(
        "SELECT state,reason,operation_id FROM pr_sync_dispatch WHERE event_id=$1 FOR UPDATE",
    )
    .bind(e.id)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some(p) = &prior
        && p.state != "WAITING"
    {
        if p.state == "ENQUEUED"
            && outcome.state == "ENQUEUED"
            && p.operation_id == outcome.operation
        {
            return Ok(());
        }
        return Err(PrSyncStoreError::InvalidEvent);
    }
    sqlx::query("INSERT INTO pr_sync_dispatch(event_id,organization_id,proposal_id,state,reason,attempts,next_at,operation_id) VALUES($1,$2,$3,$4,$5,1,CASE WHEN $6::double precision IS NULL THEN NULL ELSE clock_timestamp()+make_interval(secs=>$6) END,$7) ON CONFLICT(event_id) DO UPDATE SET state=excluded.state,reason=excluded.reason,attempts=least(pr_sync_dispatch.attempts::bigint+1,2147483647),next_at=excluded.next_at,operation_id=excluded.operation_id,updated_at=clock_timestamp()")
        .bind(e.id).bind(e.organization_id).bind(e.entity_id).bind(outcome.state).bind(outcome.reason).bind(outcome.delay.map(f64::from)).bind(outcome.operation).execute(&mut *conn).await?;
    if prior
        .as_ref()
        .is_none_or(|p| p.state != outcome.state || p.reason != outcome.reason)
    {
        sqlx::query("INSERT INTO audit_events(id,organization_id,action,entity_id,after_ref,operation_id,result) VALUES($1,$2,'PR_OUTBOX_DISPATCH',$3,$4,$5,$6)")
            .bind(Uuid::new_v4()).bind(e.organization_id).bind(e.entity_id).bind(e.id.to_string()).bind(outcome.operation).bind(format!("{}:{}",outcome.state,outcome.reason)).execute(&mut *conn).await?;
    }
    if outcome.state != "WAITING" {
        sqlx::query("UPDATE outbox SET sent_at=coalesce(sent_at,clock_timestamp()) WHERE id=$1")
            .bind(e.id)
            .execute(conn)
            .await?;
    }
    Ok(())
}
fn classify(error: PrSyncStoreError) -> Result<Outcome, PrSyncStoreError> {
    match error {
        PrSyncStoreError::Busy | PrSyncStoreError::Access(SubmissionError::Busy) => {
            Ok(Outcome::waiting("PR_SYNC_BUSY", 30))
        }
        PrSyncStoreError::QuotaExceeded => Ok(Outcome::waiting("ADMISSION_QUOTA", 60)),
        PrSyncStoreError::Access(SubmissionError::Forbidden | SubmissionError::NotFound) => {
            Ok(Outcome::waiting("AUTHORITY_REQUIRED", 300))
        }
        PrSyncStoreError::Stale
        | PrSyncStoreError::Access(SubmissionError::Context(
            ReviewContextError::Stale | ReviewContextError::StateChanged,
        )) => Ok(Outcome::skipped()),
        PrSyncStoreError::Access(SubmissionError::Context(
            ReviewContextError::OwnerNotEligible | ReviewContextError::InvalidConfiguration,
        )) => Ok(Outcome::waiting("REVIEW_SETUP_REQUIRED", 300)),
        PrSyncStoreError::InvalidEvent => Ok(Outcome::rejected("PUBLICATION_INVALID")),
        PrSyncStoreError::InvalidIntent | PrSyncStoreError::Projection(_) => {
            Ok(Outcome::rejected("PROJECTION_INVALID"))
        }
        error => Err(error),
    }
}
