//! Current server readiness to atomic durable admission. Provider IO never holds
//! a transaction; a reservation is not a published check or merge authority.
use crate::review_readiness::{
    CapturedReadiness, FreshReadinessError, PgReviewReadiness, ReadinessError,
};
use serde_json::json;
use specspace_application::{
    comparison_reader::BoundProviderFactory,
    review_check_publication::{CheckPublicationIntent, CheckPublicationSource},
};
use specspace_git_adapter::CommitSha;
use sqlx::{PgConnection, PgPool};
use std::sync::Arc;
use uuid::Uuid;
mod read;

#[derive(Debug, PartialEq, Eq)]
pub enum CheckStoreError {
    Invalid,
    NotFound,
    Forbidden,
    NotConfigured,
    Stale,
    Busy,
    Quota,
    IdempotencyMismatch,
    InvalidRecord,
    Fresh(FreshReadinessError),
    Unavailable,
}
impl From<sqlx::Error> for CheckStoreError {
    fn from(error: sqlx::Error) -> Self {
        match error.as_database_error().and_then(|e| e.constraint()) {
            Some("proposal_active_review_check") => Self::Busy,
            Some("git_operations_pkey") => Self::IdempotencyMismatch,
            _ => Self::Unavailable,
        }
    }
}
impl From<ReadinessError> for CheckStoreError {
    fn from(error: ReadinessError) -> Self {
        match error {
            ReadinessError::Invalid => Self::Invalid,
            ReadinessError::NotFound => Self::NotFound,
            ReadinessError::Unavailable => Self::Unavailable,
        }
    }
}
impl From<FreshReadinessError> for CheckStoreError {
    fn from(error: FreshReadinessError) -> Self {
        match error {
            FreshReadinessError::Stored(error) => error.into(),
            other => Self::Fresh(other),
        }
    }
}
pub struct CheckReservation {
    pub job_id: Uuid,
    pub replayed: bool,
    intent: CheckPublicationIntent,
}
impl CheckReservation {
    pub fn intent(&self) -> &CheckPublicationIntent {
        &self.intent
    }
    pub fn operation_id(&self) -> Uuid {
        self.intent.source().operation
    }
}
pub struct PgReviewChecks {
    pool: PgPool,
    readiness: PgReviewReadiness,
    source: Arc<dyn BoundProviderFactory>,
}
impl PgReviewChecks {
    pub fn new(pool: PgPool, source: Arc<dyn BoundProviderFactory>) -> Self {
        Self {
            readiness: PgReviewReadiness::new(pool.clone()),
            pool,
            source,
        }
    }

    /// Caller supplies only identities, never a conclusion, App ID or proof.
    /// Exact retries observe the original reservation after current authority.
    pub async fn evaluate(
        &self,
        actor: Uuid,
        proposal: Uuid,
        snapshot: Uuid,
        operation: Uuid,
    ) -> Result<CheckReservation, CheckStoreError> {
        if [actor, proposal, snapshot, operation]
            .iter()
            .any(Uuid::is_nil)
        {
            return Err(CheckStoreError::Invalid);
        }
        let initial = self
            .readiness
            .capture(actor, proposal, snapshot, true)
            .await?;
        authorized(&initial)?;
        let org = initial
            .context
            .as_ref()
            .ok_or(CheckStoreError::InvalidRecord)?
            .organization;
        let mut conn = self.pool.acquire().await?;
        if let Some(replay) =
            read::evaluation(&mut conn, org, actor, proposal, snapshot, operation).await?
        {
            return Ok(replay);
        }
        available(&initial)?;
        admission_limits(&mut conn, org, actor, proposal, operation).await?;
        drop(conn);

        let fresh = self
            .readiness
            .fresh_capture(actor, proposal, snapshot, self.source.as_ref())
            .await?;
        authorized(&fresh)?;
        available(&fresh)?;
        if !same_scope(&initial, &fresh) {
            return Err(CheckStoreError::Stale);
        }
        let intent = intent(&fresh, operation)?;
        let mut tx = self.pool.begin().await?;
        sqlx::query("SET LOCAL lock_timeout='5s'")
            .execute(&mut *tx)
            .await?;
        sqlx::query("SET LOCAL statement_timeout='10s'")
            .execute(&mut *tx)
            .await?;
        sqlx::query("SET LOCAL application_name='specspace-check-admission'")
            .execute(&mut *tx)
            .await?;
        // Shared writer order: proposal, organization quota, actor/reviewers.
        let found: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM proposals WHERE organization_id=$1 AND id=$2 FOR UPDATE",
        )
        .bind(org)
        .bind(proposal)
        .fetch_optional(&mut *tx)
        .await?;
        if found.is_none() {
            return Err(CheckStoreError::NotFound);
        }
        sqlx::query("SELECT id FROM organizations WHERE id=$1 FOR UPDATE")
            .bind(org)
            .execute(&mut *tx)
            .await?;
        let locked =
            PgReviewReadiness::capture_in(&mut tx, actor, proposal, snapshot, true, true).await?;
        authorized(&locked)?;
        // A concurrent identical admission wins once; never re-render its proof.
        if let Some(replay) =
            read::evaluation(&mut tx, org, actor, proposal, snapshot, operation).await?
        {
            tx.commit().await?;
            return Ok(replay);
        }
        available(&locked)?;
        if !same_scope(&fresh, &locked) || fresh.authority_digest != locked.authority_digest {
            return Err(CheckStoreError::Stale);
        }
        admission_limits(&mut tx, org, actor, proposal, operation).await?;
        let job_id = Uuid::new_v4();
        let audit = Uuid::new_v4();
        let event = Uuid::new_v4();
        let s = intent.source();
        sqlx::query("INSERT INTO git_operations(id,organization_id,proposal_id,actor_id,kind,idempotency_key,request_hash,expected_sha) VALUES($1,$2,$3,$4,'REVIEW_CHECK',$1::text,$5,$6)")
            .bind(operation).bind(org).bind(proposal).bind(actor).bind(intent.request_hash()).bind(s.head.as_str()).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO jobs(id,organization_id,operation_id,kind,payload,max_attempts) VALUES($1,$2,$3,'PUBLISH_REVIEW_CHECK',jsonb_build_object('operation_id',$3::uuid),8)")
            .bind(job_id).bind(org).bind(operation).execute(&mut *tx).await?;
        let report_digest = match intent.purpose() {
            specspace_application::review_check_publication::CheckPurpose::Evaluation {
                report_digest,
            } => report_digest,
            _ => return Err(CheckStoreError::InvalidRecord),
        };
        sqlx::query("INSERT INTO review_check_intents(operation_id,job_id,organization_id,proposal_id,repository_id,actor_id,snapshot_id,
            remote_id,installation_id,app_id,branch,head_sha,target_sha,merge_base_sha,proposal_version,configuration_version,policy_version,
            fingerprint,evidence_digest,authority_digest,request_hash,purpose,result,report_digest,audit_id,event_id)
            VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,'EVALUATION',$22,$23,$24,$25)")
            .bind(operation).bind(job_id).bind(org).bind(proposal).bind(s.repository.repository).bind(actor).bind(snapshot)
            .bind(s.repository.remote_id as i64).bind(s.repository.installation_id as i64).bind(s.app_id as i64).bind(s.branch.as_str()).bind(s.head.as_str()).bind(s.target.as_str()).bind(s.merge_base.as_str())
            .bind(s.proposal_version).bind(s.configuration_version).bind(s.policy_version).bind(&s.fingerprint).bind(&s.evidence_digest).bind(&s.authority_digest).bind(intent.request_hash())
            .bind(read::result(intent.result())).bind(report_digest).bind(audit).bind(event).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO audit_events(id,organization_id,actor_id,action,entity_id,after_ref,operation_id,result) VALUES($1,$2,$3,'REVIEW_CHECK_RESERVED',$4,$5,$6,'QUEUED')")
            .bind(audit).bind(org).bind(actor).bind(proposal).bind(snapshot.to_string()).bind(operation).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO outbox(id,organization_id,event_type,entity_id,payload) VALUES($1,$2,'REVIEW_CHECK_QUEUED',$3,$4)")
            .bind(event).bind(org).bind(proposal).bind(json!({"operation_id":operation,"snapshot_id":snapshot,"purpose":"EVALUATION","result":read::result(intent.result())})).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(CheckReservation {
            job_id,
            replayed: false,
            intent,
        })
    }
}
fn authorized(c: &CapturedReadiness) -> Result<(), CheckStoreError> {
    if c.publication_authorized {
        Ok(())
    } else {
        Err(CheckStoreError::Forbidden)
    }
}
fn available(c: &CapturedReadiness) -> Result<(), CheckStoreError> {
    if c.app_id.is_none() {
        return Err(CheckStoreError::NotConfigured);
    }
    if !c.report.current_for_metadata
        || !c.context.as_ref().is_some_and(|ctx| {
            matches!(
                ctx.state.as_str(),
                "IN_REVIEW" | "CHANGES_REQUESTED" | "APPROVED"
            )
        })
    {
        return Err(CheckStoreError::Stale);
    }
    Ok(())
}
fn same_scope(a: &CapturedReadiness, b: &CapturedReadiness) -> bool {
    a.context == b.context
        && a.app_id == b.app_id
        && a.merge_base == b.merge_base
        && a.evidence_digest == b.evidence_digest
        && a.report.fingerprint == b.report.fingerprint
        && a.report.head_sha == b.report.head_sha
        && a.report.target_sha == b.report.target_sha
        && a.report.policy_version == b.report.policy_version
        && a.report.configuration_version == b.report.configuration_version
}
fn intent(
    c: &CapturedReadiness,
    operation: Uuid,
) -> Result<CheckPublicationIntent, CheckStoreError> {
    let ctx = c.context.as_ref().ok_or(CheckStoreError::InvalidRecord)?;
    let r = &c.report;
    let source = CheckPublicationSource {
        organization: ctx.organization,
        actor: ctx.actor,
        proposal: ctx.proposal,
        operation,
        snapshot: r.snapshot_id,
        repository: ctx.connection.clone(),
        app_id: u64::try_from(c.app_id.ok_or(CheckStoreError::NotConfigured)?)
            .map_err(|_| CheckStoreError::InvalidRecord)?,
        branch: ctx.branch.clone(),
        head: CommitSha::parse(&r.head_sha).map_err(|_| CheckStoreError::InvalidRecord)?,
        target: CommitSha::parse(&r.target_sha).map_err(|_| CheckStoreError::InvalidRecord)?,
        merge_base: CommitSha::parse(&c.merge_base).map_err(|_| CheckStoreError::InvalidRecord)?,
        proposal_version: ctx.version,
        configuration_version: r.configuration_version,
        policy_version: r.policy_version,
        fingerprint: r.fingerprint.clone(),
        evidence_digest: c.evidence_digest.clone(),
        authority_digest: c.authority_digest.clone(),
    };
    CheckPublicationIntent::from_readiness(source, r).map_err(|_| CheckStoreError::InvalidRecord)
}
async fn admission_limits(
    conn: &mut PgConnection,
    org: Uuid,
    actor: Uuid,
    proposal: Uuid,
    operation: Uuid,
) -> Result<(), CheckStoreError> {
    let busy: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM git_operations WHERE organization_id=$1 AND proposal_id=$2 AND kind='REVIEW_CHECK' AND id<>$3 AND state IN ('PENDING','RUNNING','UNKNOWN'))")
        .bind(org).bind(proposal).bind(operation).fetch_one(&mut *conn).await?;
    if busy {
        return Err(CheckStoreError::Busy);
    }
    let (actor_count,total): (i64,i64) = sqlx::query_as("SELECT count(*) FILTER(WHERE actor_id=$2),count(*) FROM review_check_intents WHERE organization_id=$1 AND operation_id<>$3 AND created_at>clock_timestamp()-interval '10 minutes'")
        .bind(org).bind(actor).bind(operation).fetch_one(conn).await?;
    if actor_count >= 60 || total >= 600 {
        return Err(CheckStoreError::Quota);
    }
    Ok(())
}
