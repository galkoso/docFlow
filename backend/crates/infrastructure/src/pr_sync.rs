//! Atomic outbox admission only. No provider IO or mutation lease is granted by
//! this journal; queued PR jobs are excluded from generic workers/completion.
use crate::{
    review_authorization,
    review_submissions::{self, SubmissionError},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use specspace_application::{
    comparison_reader::RepositoryBinding,
    pr_sync::{PrProjection, PrSiteOrigin, PrSyncError, PrSyncIntent, PrSyncSource},
    proposal::{ProposalLink, UiImpact},
    review_context::ReviewExpectation,
};
use specspace_domain::BranchName;
use specspace_git_adapter::CommitSha;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

#[derive(Debug, PartialEq, Eq)]
pub enum PrSyncStoreError {
    Access(SubmissionError),
    InvalidEvent,
    InvalidIntent,
    Stale,
    Busy,
    QuotaExceeded,
    Projection(PrSyncError),
    Unavailable,
}
impl From<sqlx::Error> for PrSyncStoreError {
    fn from(error: sqlx::Error) -> Self {
        if error.as_database_error().and_then(|e| e.constraint()) == Some("proposal_active_pr_sync")
        {
            Self::Busy
        } else {
            Self::Unavailable
        }
    }
}
impl From<SubmissionError> for PrSyncStoreError {
    fn from(error: SubmissionError) -> Self {
        Self::Access(error)
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicationEvent {
    snapshot_id: Uuid,
    operation_id: Uuid,
    pr_sync_required: bool,
    approvals_granted: bool,
    activated: bool,
}
#[derive(FromRow)]
struct Event {
    payload: Value,
    delivered: bool,
}
#[derive(FromRow)]
struct Header {
    repository_id: Uuid,
    head_sha: String,
    target_sha: String,
    merge_base_sha: String,
    configuration_version: i64,
    policy_version: i64,
    fingerprint: String,
    evidence_digest: String,
    branch: String,
    remote_id: String,
    installation_id: String,
    current_version: i64,
    current_number: i64,
    current_title: String,
    links: Value,
}
#[derive(FromRow)]
struct Frozen {
    operation_id: Uuid,
    job_id: Uuid,
    source_event: Uuid,
    organization_id: Uuid,
    proposal_id: Uuid,
    repository_id: Uuid,
    actor_id: Uuid,
    snapshot_id: Uuid,
    branch: String,
    remote_id: String,
    installation_id: String,
    head_sha: String,
    target_sha: String,
    merge_base_sha: String,
    proposal_version: i64,
    configuration_version: i64,
    policy_version: i64,
    fingerprint: String,
    evidence_digest: String,
    authority_digest: String,
    request_hash: String,
    title: String,
    body: String,
}
/// Server-only receipt. Not a synchronization/provider success or approval.
pub struct PrSyncReservation {
    pub operation_id: Uuid,
    pub job_id: Uuid,
    pub source_event: Uuid,
    pub replayed: bool,
    intent: PrSyncIntent,
}
impl PrSyncReservation {
    pub fn intent(&self) -> &PrSyncIntent {
        &self.intent
    }
}
fn number(value: &str) -> Result<u64, PrSyncStoreError> {
    value
        .parse::<u64>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or(PrSyncStoreError::InvalidIntent)
}
fn version(value: i64) -> Result<u64, PrSyncStoreError> {
    u64::try_from(value)
        .ok()
        .filter(|n| *n > 0)
        .ok_or(PrSyncStoreError::InvalidIntent)
}
fn sha(value: String) -> Result<CommitSha, PrSyncStoreError> {
    CommitSha::parse(value).map_err(|_| PrSyncStoreError::InvalidIntent)
}
impl Frozen {
    fn restore(self) -> Result<PrSyncReservation, PrSyncStoreError> {
        let source = PrSyncSource {
            organization: self.organization_id,
            proposal: self.proposal_id,
            actor: self.actor_id,
            operation: self.operation_id,
            snapshot: self.snapshot_id,
            repository: RepositoryBinding {
                repository: self.repository_id,
                remote_id: number(&self.remote_id)?,
                installation_id: number(&self.installation_id)?,
            },
            branch: BranchName::parse(self.branch).map_err(|_| PrSyncStoreError::InvalidIntent)?,
            head: sha(self.head_sha)?,
            target: sha(self.target_sha)?,
            merge_base: sha(self.merge_base_sha)?,
            proposal_version: self.proposal_version,
            configuration_version: version(self.configuration_version)?,
            policy_version: version(self.policy_version)?,
            fingerprint: self.fingerprint,
            evidence_digest: self.evidence_digest,
            authority_digest: self.authority_digest,
        };
        let intent = PrSyncIntent::restore(source, self.title, self.body, &self.request_hash)
            .map_err(|_| PrSyncStoreError::InvalidIntent)?;
        Ok(PrSyncReservation {
            operation_id: self.operation_id,
            job_id: self.job_id,
            source_event: self.source_event,
            replayed: true,
            intent,
        })
    }
}
pub struct PgPrSync {
    pool: PgPool,
}
impl PgPrSync {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// `actor` is the publication's original human actor, resolved server-side
    /// by the eventual dispatcher. Current edit authority is required on replay.
    /// An event marked sent means delivered into this journal, not synced to Git.
    pub async fn reserve(
        &self,
        actor: Uuid,
        proposal: Uuid,
        event_id: Uuid,
        site: &PrSiteOrigin,
    ) -> Result<PrSyncReservation, PrSyncStoreError> {
        if actor.is_nil() || proposal.is_nil() || event_id.is_nil() {
            return Err(PrSyncStoreError::InvalidEvent);
        }
        let mut tx = self.pool.begin().await?;
        sqlx::query("SET LOCAL lock_timeout='5s'")
            .execute(&mut *tx)
            .await?;
        sqlx::query("SET LOCAL statement_timeout='10s'")
            .execute(&mut *tx)
            .await?;
        let org = review_submissions::access(&mut tx, actor, proposal, true).await?;
        let event = sqlx::query_as::<_,Event>("SELECT payload,sent_at IS NOT NULL AS delivered FROM outbox WHERE id=$1 AND organization_id=$2 AND entity_id=$3 AND event_type='REVIEW_SNAPSHOT_PUBLISHED' FOR UPDATE")
            .bind(event_id).bind(org).bind(proposal).fetch_optional(&mut *tx).await?.ok_or(PrSyncStoreError::InvalidEvent)?;
        if let Some(old) = sqlx::query_as::<_,Frozen>("SELECT i.* FROM pr_sync_intents i WHERE source_event=$1 AND organization_id=$2 AND proposal_id=$3")
            .bind(event_id).bind(org).bind(proposal).fetch_optional(&mut *tx).await? {
            if old.actor_id != actor { return Err(SubmissionError::Forbidden.into()); }
            let receipt = old.restore()?;
            tx.commit().await?;
            return Ok(receipt);
        }
        if event.delivered {
            return Err(PrSyncStoreError::InvalidEvent);
        }
        let event: PublicationEvent =
            serde_json::from_value(event.payload).map_err(|_| PrSyncStoreError::InvalidEvent)?;
        if event.snapshot_id.is_nil()
            || event.operation_id.is_nil()
            || !event.pr_sync_required
            || event.approvals_granted
        {
            return Err(PrSyncStoreError::InvalidEvent);
        }
        let original: Option<Uuid> = sqlx::query_scalar("SELECT s.actor_id FROM review_submission_results x JOIN review_submissions s ON s.operation_id=x.operation_id AND s.organization_id=x.organization_id AND s.proposal_id=x.proposal_id JOIN git_operations o ON o.id=s.operation_id AND o.organization_id=s.organization_id JOIN jobs j ON j.id=s.job_id AND j.operation_id=o.id AND j.organization_id=o.organization_id JOIN review_snapshots h ON h.id=x.snapshot_id AND h.organization_id=x.organization_id AND h.proposal_id=x.proposal_id WHERE x.operation_id=$1 AND x.organization_id=$2 AND x.proposal_id=$3 AND x.snapshot_id=$4 AND x.activated=$5 AND h.state='READY' AND o.kind='SUBMIT' AND o.state='SUCCEEDED' AND o.result_sha=h.head_sha AND j.kind='SUBMIT_REVIEW' AND j.state='SUCCEEDED' AND j.result=jsonb_build_object('phase','SNAPSHOT_READY','snapshot_id',h.id)")
            .bind(event.operation_id).bind(org).bind(proposal).bind(event.snapshot_id).bind(event.activated).fetch_optional(&mut *tx).await?;
        let original = original.ok_or(PrSyncStoreError::InvalidEvent)?;
        if original != actor {
            return Err(SubmissionError::Forbidden.into());
        }
        let header = sqlx::query_as::<_,Header>("SELECT h.*,p.version AS current_version,p.number AS current_number,p.title AS current_title,p.links FROM review_snapshots h JOIN proposals p ON p.active_review_snapshot_id=h.id AND p.organization_id=h.organization_id AND p.id=h.proposal_id JOIN repositories r ON r.id=p.repository_id AND r.organization_id=p.organization_id JOIN organizations g ON g.id=p.organization_id WHERE h.id=$1 AND h.organization_id=$2 AND h.proposal_id=$3 AND h.state='READY' AND p.state IN ('IN_REVIEW','APPROVED','CHANGES_REQUESTED') AND p.repository_id=h.repository_id AND p.head_sha=h.head_sha AND p.target_sha=h.target_sha AND p.base_sha=h.recorded_base_sha AND p.branch=h.branch AND p.review_configuration_version=h.configuration_version AND g.policy_version=h.policy_version AND r.provider='GITHUB' AND r.default_branch='main' AND ltrim(r.remote_id,'0+')=h.remote_id AND ltrim(r.installation_id,'0+')=h.installation_id")
            .bind(event.snapshot_id).bind(org).bind(proposal).fetch_optional(&mut *tx).await?.ok_or(PrSyncStoreError::Stale)?;
        let expectation = ReviewExpectation {
            head: sha(header.head_sha.clone())?,
            target: sha(header.target_sha.clone())?,
            proposal_version: header.current_version,
            configuration_version: version(header.configuration_version)?,
            policy_version: version(header.policy_version)?,
        };
        let scope = review_authorization::context_in(&mut tx, actor, proposal, &expectation)
            .await
            .map_err(SubmissionError::from)?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!("pr-sync-admission:{org}"))
            .execute(&mut *tx)
            .await?;
        let busy: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM git_operations WHERE organization_id=$1 AND proposal_id=$2 AND kind='PR_SYNC' AND state IN ('PENDING','RUNNING','UNKNOWN'))")
            .bind(org).bind(proposal).fetch_one(&mut *tx).await?;
        if busy {
            return Err(PrSyncStoreError::Busy);
        }
        let (actor_active,org_active):(i64,i64)=sqlx::query_as("SELECT count(*) FILTER(WHERE actor_id=$2),count(*) FROM git_operations WHERE organization_id=$1 AND kind='PR_SYNC' AND state IN ('PENDING','RUNNING','UNKNOWN')")
            .bind(org).bind(actor).fetch_one(&mut *tx).await?;
        let (actor_recent,org_recent):(i64,i64)=sqlx::query_as("SELECT count(*) FILTER(WHERE actor_id=$2),count(*) FROM pr_sync_intents WHERE organization_id=$1 AND created_at>statement_timestamp()-interval '10 minutes'")
            .bind(org).bind(actor).fetch_one(&mut *tx).await?;
        if actor_active >= 2 || org_active >= 12 || actor_recent >= 10 || org_recent >= 60 {
            return Err(PrSyncStoreError::QuotaExceeded);
        }
        let links: Vec<ProposalLink> =
            serde_json::from_value(header.links).map_err(|_| PrSyncStoreError::InvalidIntent)?;
        // Include projection metadata absent from the review scope digest. A
        // future worker must compare this current digest before provider writes.
        let authority_digest = format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&json!([
                    "specspace-pr-authority-v1",
                    scope.authority_digest(),
                    header.current_number,
                    &header.current_title,
                    &links
                ]))
                .map_err(|_| PrSyncStoreError::InvalidIntent)?
            )
        );
        let operation = Uuid::new_v4();
        let job = Uuid::new_v4();
        let source = PrSyncSource {
            organization: org,
            proposal,
            actor,
            operation,
            snapshot: event.snapshot_id,
            repository: scope.comparison.connection.clone(),
            branch: scope.comparison.branch.clone(),
            head: expectation.head,
            target: expectation.target,
            merge_base: sha(header.merge_base_sha)?,
            proposal_version: header.current_version,
            configuration_version: expectation.configuration_version,
            policy_version: expectation.policy_version,
            fingerprint: header.fingerprint,
            evidence_digest: header.evidence_digest,
            authority_digest,
        };
        let projection = PrProjection {
            number: header.current_number,
            title: header.current_title,
            description: scope.review.description,
            ui_impact: match scope.review.ui_impact {
                specspace_domain::policy::UiImpact::Yes => UiImpact::Yes,
                specspace_domain::policy::UiImpact::No => UiImpact::No,
                specspace_domain::policy::UiImpact::Undecided => UiImpact::Undecided,
            },
            ui_impact_reason: scope.review.ui_impact_reason,
            open_questions: None,
            links,
        };
        let intent = PrSyncIntent::prepare(source, projection, site)
            .map_err(PrSyncStoreError::Projection)?;
        let s = intent.source();
        if s.repository.repository != header.repository_id
            || s.repository.remote_id != number(&header.remote_id)?
            || s.repository.installation_id != number(&header.installation_id)?
            || s.branch.as_str() != header.branch
        {
            return Err(PrSyncStoreError::Stale);
        }
        sqlx::query("INSERT INTO git_operations(id,organization_id,proposal_id,actor_id,kind,idempotency_key,request_hash,expected_sha) VALUES($1,$2,$3,$4,'PR_SYNC',$5,$6,$7)")
            .bind(operation).bind(org).bind(proposal).bind(actor).bind(event_id.to_string()).bind(intent.request_hash()).bind(s.head.as_str()).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO jobs(id,organization_id,operation_id,kind,payload,max_attempts) VALUES($1,$2,$3,'SYNC_REVIEW_PR',jsonb_build_object('operation_id',$3::uuid),8)")
            .bind(job).bind(org).bind(operation).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO pr_sync_intents(operation_id,job_id,source_event,source_submission,organization_id,proposal_id,repository_id,actor_id,snapshot_id,branch,remote_id,installation_id,head_sha,target_sha,merge_base_sha,proposal_version,configuration_version,policy_version,fingerprint,evidence_digest,authority_digest,request_hash,title,body) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23,$24)")
            .bind(operation).bind(job).bind(event_id).bind(event.operation_id).bind(org).bind(proposal).bind(s.repository.repository).bind(actor).bind(s.snapshot).bind(s.branch.as_str()).bind(s.repository.remote_id.to_string()).bind(s.repository.installation_id.to_string())
            .bind(s.head.as_str()).bind(s.target.as_str()).bind(s.merge_base.as_str()).bind(s.proposal_version).bind(header.configuration_version).bind(header.policy_version).bind(&s.fingerprint).bind(&s.evidence_digest).bind(&s.authority_digest).bind(intent.request_hash()).bind(intent.content().title()).bind(intent.content().body()).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO audit_events(id,organization_id,actor_id,action,entity_id,after_ref,operation_id,result) VALUES($1,$2,$3,'PR_SYNC_RESERVED',$4,$5,$6,'QUEUED')")
            .bind(Uuid::new_v4()).bind(org).bind(actor).bind(proposal).bind(s.snapshot.to_string()).bind(operation).execute(&mut *tx).await?;
        sqlx::query("UPDATE outbox SET sent_at=clock_timestamp() WHERE id=$1 AND sent_at IS NULL")
            .bind(event_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(PrSyncReservation {
            operation_id: operation,
            job_id: job,
            source_event: event_id,
            replayed: false,
            intent,
        })
    }
}
