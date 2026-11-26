//! Current server readiness to atomic durable admission. Provider IO never holds
//! a transaction; a reservation is not a published check or merge authority.
use crate::review_readiness::{
    CapturedReadiness, FreshReadinessError, PgReviewReadiness, ReadinessError,
};
use specspace_application::{
    comparison_reader::BoundProviderFactory,
    review_check_publication::{CheckPublicationIntent, CheckPublicationSource},
};
use specspace_git_adapter::CommitSha;
use sqlx::{PgConnection, PgPool};
use std::sync::Arc;
use uuid::Uuid;
mod invalidation;
mod journal;
mod read;
mod reservation;
pub use journal::{CheckLease, CheckObservation};

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
    LeaseLost,
    WriteAlreadyStarted,
    InvalidReceipt,
    Cooldown(u32),
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
        journal::connection_cooldown(
            &mut conn,
            org,
            &initial
                .context
                .as_ref()
                .ok_or(CheckStoreError::InvalidRecord)?
                .connection,
        )
        .await?;
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
        journal::connection_cooldown(&mut tx, org, &intent.source().repository).await?;
        let job_id = reservation::reserve(&mut tx, &intent).await?;
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
