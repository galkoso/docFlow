use super::*;
use specspace_application::review_pr_status::*;
type Timestamp = sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>;
fn time(t: Timestamp) -> String {
    t.to_rfc3339()
}
#[derive(FromRow)]
struct Header {
    fingerprint: String,
    head_sha: String,
    target_sha: String,
    current_for_sync: bool,
    binding_current: bool,
}
#[derive(FromRow)]
struct Publication {
    operation_id: Uuid,
    event_id: Option<Uuid>,
    delivered: Option<bool>,
    state: Option<String>,
    reason: Option<String>,
    next_at: Option<Timestamp>,
}
#[derive(FromRow)]
struct Work {
    operation_id: Uuid,
    state: String,
    operation_state: String,
    error_code: Option<String>,
    attempts: i32,
    max_attempts: i32,
    available_at: Timestamp,
    leased_until: Option<Timestamp>,
    has_write: bool,
    disposition: Option<String>,
}
#[derive(FromRow)]
struct Observation {
    operation_id: Uuid,
    snapshot_id: Uuid,
    pr_number: i64,
    url: String,
    observed_head: String,
    observed_target: String,
    created_at: Timestamp,
    disposition: String,
}
impl PgPrSync {
    /// Current organization readers may inspect stored evidence for one exact
    /// READY snapshot. No provider IO or job/dispatch mutation occurs in this GET.
    pub async fn status(
        &self,
        actor: Uuid,
        proposal: Uuid,
        snapshot: Uuid,
    ) -> Result<ReviewPrStatus, PrSyncStoreError> {
        if actor.is_nil() || proposal.is_nil() || snapshot.is_nil() {
            return Err(SubmissionError::NotFound.into());
        }
        let mut tx = self.pool.begin().await?;
        // One coherent read snapshot prevents a newly committed intent/result
        // being combined with an older transport phase from another statement.
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
            .execute(&mut *tx)
            .await?;
        sqlx::query("SET LOCAL lock_timeout='5s'")
            .execute(&mut *tx)
            .await?;
        sqlx::query("SET LOCAL statement_timeout='10s'")
            .execute(&mut *tx)
            .await?;
        let org = crate::review_snapshots::read_access(&mut tx, actor, proposal).await?;
        let header=sqlx::query_as::<_,Header>("SELECT h.fingerprint,h.head_sha,h.target_sha,pr_sync_snapshot_current($2,$3,$1) AS current_for_sync,(p.repository_id=h.repository_id AND p.branch=h.branch AND r.provider='GITHUB' AND r.default_branch='main' AND ltrim(r.remote_id,'0+')=h.remote_id AND ltrim(r.installation_id,'0+')=h.installation_id) AS binding_current FROM review_snapshots h JOIN proposals p ON p.id=h.proposal_id AND p.organization_id=h.organization_id JOIN repositories r ON r.id=p.repository_id AND r.organization_id=p.organization_id WHERE h.id=$1 AND h.organization_id=$2 AND h.proposal_id=$3 AND h.state='READY'")
            .bind(snapshot).bind(org).bind(proposal).fetch_optional(&mut *tx).await?.ok_or(SubmissionError::NotFound)?;
        let source=sqlx::query_as::<_,Publication>("SELECT x.operation_id,e.id AS event_id,e.sent_at IS NOT NULL AS delivered,d.state,d.reason,d.next_at FROM review_submission_results x JOIN review_submissions s ON s.operation_id=x.operation_id AND s.organization_id=x.organization_id AND s.proposal_id=x.proposal_id JOIN jobs j ON j.id=s.job_id AND j.kind='SUBMIT_REVIEW' AND j.state='SUCCEEDED' JOIN git_operations o ON o.id=s.operation_id AND o.organization_id=s.organization_id AND o.kind='SUBMIT' AND o.state='SUCCEEDED' LEFT JOIN pr_sync_intents i ON i.source_submission=x.operation_id LEFT JOIN LATERAL (SELECT e.id,e.sent_at FROM outbox e WHERE e.organization_id=x.organization_id AND e.entity_id=x.proposal_id AND e.event_type='REVIEW_SNAPSHOT_PUBLISHED' AND (e.id=i.source_event OR (i.operation_id IS NULL AND e.payload->>'operation_id'=x.operation_id::text)) ORDER BY e.created_at,e.id LIMIT 1) e ON true LEFT JOIN pr_sync_dispatch d ON d.event_id=e.id WHERE x.organization_id=$1 AND x.proposal_id=$2 AND x.snapshot_id=$3 AND j.operation_id=o.id AND j.organization_id=x.organization_id AND j.result=jsonb_build_object('phase','SNAPSHOT_READY','snapshot_id',x.snapshot_id) AND o.result_sha=$4 ORDER BY x.created_at DESC,x.operation_id DESC LIMIT 1")
            .bind(org).bind(proposal).bind(snapshot).bind(&header.head_sha).fetch_optional(&mut *tx).await?.ok_or(PrSyncStoreError::InvalidIntent)?;
        let work=sqlx::query_as::<_,Work>("SELECT i.operation_id,j.state,o.state AS operation_state,o.error_code,j.attempts,j.max_attempts,j.available_at,j.leased_until,w.operation_id IS NOT NULL AS has_write,r.disposition FROM pr_sync_intents i JOIN jobs j ON j.id=i.job_id AND j.operation_id=i.operation_id AND j.organization_id=i.organization_id JOIN git_operations o ON o.id=i.operation_id AND o.organization_id=i.organization_id LEFT JOIN pr_sync_writes w ON w.operation_id=i.operation_id LEFT JOIN pr_sync_results r ON r.operation_id=i.operation_id WHERE i.source_submission=$1 AND i.organization_id=$2 AND i.proposal_id=$3 AND i.snapshot_id=$4 AND j.kind='SYNC_REVIEW_PR' AND o.kind='PR_SYNC'")
            .bind(source.operation_id).bind(org).bind(proposal).bind(snapshot).fetch_optional(&mut *tx).await?;
        let uncertain:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pr_sync_intents i JOIN pr_sync_writes w ON w.operation_id=i.operation_id JOIN git_operations o ON o.id=i.operation_id WHERE i.organization_id=$1 AND i.proposal_id=$2 AND o.state IN ('RUNNING','UNKNOWN'))")
            .bind(org).bind(proposal).fetch_one(&mut *tx).await?;
        let observation = if header.binding_current {
            sqlx::query_as::<_,Observation>("SELECT i.operation_id,i.snapshot_id,b.pr_number,b.url,r.observed_head,r.observed_target,r.created_at,r.disposition FROM proposal_pull_requests b JOIN pr_sync_intents i ON i.proposal_id=b.proposal_id AND i.organization_id=b.organization_id AND i.repository_id=b.repository_id AND i.remote_id=b.remote_id AND i.installation_id=b.installation_id AND i.branch=b.branch JOIN pr_sync_results r ON r.operation_id=i.operation_id AND r.pr_number=b.pr_number AND r.provider_id=b.provider_id JOIN proposals p ON p.id=b.proposal_id AND p.organization_id=b.organization_id JOIN repositories c ON c.id=p.repository_id AND c.organization_id=p.organization_id WHERE b.organization_id=$1 AND b.proposal_id=$2 AND b.repository_id=p.repository_id AND b.branch=p.branch AND ltrim(c.remote_id,'0+')=b.remote_id AND ltrim(c.installation_id,'0+')=b.installation_id AND c.default_branch='main' AND c.provider='GITHUB' ORDER BY r.created_at DESC,i.operation_id DESC LIMIT 1")
                .bind(org).bind(proposal).fetch_optional(&mut *tx).await?
        } else {
            None
        };
        let now: Timestamp = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&mut *tx)
            .await?;
        let admin: bool = sqlx::query_scalar(
            "SELECT role='ADMIN' FROM memberships WHERE organization_id=$1 AND user_id=$2",
        )
        .bind(org)
        .bind(actor)
        .fetch_one(&mut *tx)
        .await?;
        // A later submission may reuse this header while an earlier intent for
        // the same snapshot is stopped. Keep latest-publication work separate
        // from the latest intent eligible for explicit operator recovery.
        let recovery_work: Option<(String, String, bool, bool)> = if admin && header.binding_current
        {
            sqlx::query_as("SELECT j.state,o.state,EXISTS(SELECT 1 FROM pr_sync_writes w WHERE w.operation_id=i.operation_id),EXISTS(SELECT 1 FROM pr_sync_results x WHERE x.operation_id=i.operation_id) FROM pr_sync_intents i JOIN jobs j ON j.id=i.job_id JOIN git_operations o ON o.id=i.operation_id WHERE i.organization_id=$1 AND i.proposal_id=$2 AND i.snapshot_id=$3 ORDER BY i.created_at DESC,i.operation_id DESC LIMIT 1")
                .bind(org).bind(proposal).bind(snapshot).fetch_optional(&mut *tx).await?
        } else {
            None
        };
        let recovery = recovery_work
            .filter(|(state, _, _, resolved)| state == "FAILED" && !resolved)
            .and_then(|(_, operation_state, has_write, _)| {
                use specspace_application::pr_recovery::PrRecoveryMode;
                if has_write && operation_state == "UNKNOWN" {
                    Some(PrRecoveryMode::ObserveStarted)
                } else if !has_write && operation_state == "FAILED" && header.current_for_sync {
                    Some(PrRecoveryMode::RetryUnstarted)
                } else {
                    None
                }
            });
        let dispatch = dispatch(&source, work.is_some())?;
        let sync = work
            .as_ref()
            .map(|w| sync(w, &header, uncertain, observation.as_ref(), now))
            .transpose()?;
        let pr = observation.map(|p| ReviewPrObservation {
            number: p.pr_number.to_string(),
            url: p.url,
            snapshot_id: p.snapshot_id,
            head_sha: p.observed_head,
            target_sha: p.observed_target,
            recorded_at: time(p.created_at),
            superseded: p.disposition == "SUPERSEDED",
        });
        tx.commit().await?;
        Ok(ReviewPrStatus {
            proposal_id: proposal,
            snapshot_id: snapshot,
            fingerprint: header.fingerprint,
            head_sha: header.head_sha,
            target_sha: header.target_sha,
            current_for_sync: header.current_for_sync,
            binding_current: header.binding_current,
            read_at: time(now),
            dispatch,
            sync,
            uncertain_write: uncertain,
            recovery,
            pr,
        })
    }
}
fn dispatch(p: &Publication, has_work: bool) -> Result<ReviewPrDispatchStatus, PrSyncStoreError> {
    let phase = match p.state.as_deref() {
        Some("WAITING") => ReviewPrDispatchPhase::Waiting,
        Some("ENQUEUED") => ReviewPrDispatchPhase::Enqueued,
        Some("SKIPPED") => ReviewPrDispatchPhase::Skipped,
        Some("REJECTED") => ReviewPrDispatchPhase::Rejected,
        None if has_work => ReviewPrDispatchPhase::Enqueued,
        None if p.event_id.is_some() && p.delivered != Some(true) => {
            ReviewPrDispatchPhase::Awaiting
        }
        None => ReviewPrDispatchPhase::Untracked,
        _ => return Err(PrSyncStoreError::InvalidIntent),
    };
    if has_work && phase != ReviewPrDispatchPhase::Enqueued
        || phase == ReviewPrDispatchPhase::Waiting && p.next_at.is_none()
    {
        return Err(PrSyncStoreError::InvalidIntent);
    }
    Ok(ReviewPrDispatchStatus {
        phase,
        reason: p
            .reason
            .as_deref()
            .filter(|v| !matches!(*v, "QUEUED" | "EXISTING_INTENT"))
            .map(reason),
        next_attempt_at: if phase == ReviewPrDispatchPhase::Waiting {
            p.next_at.map(time)
        } else {
            None
        },
    })
}
fn sync(
    w: &Work,
    h: &Header,
    uncertain: bool,
    last: Option<&Observation>,
    now: Timestamp,
) -> Result<ReviewPrSyncStatus, PrSyncStoreError> {
    let phase = match w.disposition.as_deref() {
        Some("SYNCED")
            if h.current_for_sync
                && !uncertain
                && last.is_some_and(|p| {
                    p.operation_id == w.operation_id
                        && p.observed_head == h.head_sha
                        && p.observed_target == h.target_sha
                }) =>
        {
            ReviewPrSyncPhase::ObservedSynced
        }
        Some("SYNCED" | "SUPERSEDED") => ReviewPrSyncPhase::Superseded,
        Some(_) => return Err(PrSyncStoreError::InvalidIntent),
        None if w.operation_state == "UNKNOWN"
            || w.has_write && w.state == "FAILED"
            || w.has_write && w.state == "RUNNING" && w.leased_until.is_some_and(|t| t <= now) =>
        {
            ReviewPrSyncPhase::Reconciling
        }
        None if w.operation_state == "FAILED" && !w.has_write && w.state == "FAILED" => {
            ReviewPrSyncPhase::Failed
        }
        None if w.state == "QUEUED" && w.available_at > now => ReviewPrSyncPhase::RetryWait,
        None if w.state == "QUEUED" => ReviewPrSyncPhase::Queued,
        None if w.state == "RUNNING" => ReviewPrSyncPhase::Observing,
        _ => return Err(PrSyncStoreError::InvalidIntent),
    };
    let automatic = w.disposition.is_none()
        && (w.state == "QUEUED" || w.state == "RUNNING")
        && (w.attempts < w.max_attempts
            || w.state == "RUNNING" && w.leased_until.is_some_and(|t| t > now));
    let next_attempt_at = if w.state == "QUEUED" && w.available_at > now {
        Some(time(w.available_at))
    } else {
        None
    };
    let r = if !h.binding_current {
        Some(ReviewPrReason::BindingChanged)
    } else {
        w.error_code.as_deref().map(reason)
    };
    Ok(ReviewPrSyncStatus {
        phase,
        reason: r,
        automatic,
        next_attempt_at,
    })
}
fn reason(value: &str) -> ReviewPrReason {
    match value {
        "QUEUED" | "EXISTING_INTENT" => ReviewPrReason::Other,
        "PR_SYNC_BUSY" | "PR_CONTEXT_BUSY" => ReviewPrReason::Busy,
        "ADMISSION_QUOTA" => ReviewPrReason::Quota,
        "AUTHORITY_REQUIRED" | "PR_FORBIDDEN" => ReviewPrReason::AuthorityRequired,
        "REVIEW_SETUP_REQUIRED" | "PR_SETUP_REQUIRED" | "PR_PROVIDER_UNAUTHORIZED" => {
            ReviewPrReason::SetupRequired
        }
        "SNAPSHOT_SUPERSEDED" | "PR_CONTEXT_CHANGED" | "PR_REFS_CHANGED" => {
            ReviewPrReason::SourceChanged
        }
        "PR_BINDING_CHANGED" => ReviewPrReason::BindingChanged,
        "PUBLICATION_INVALID" | "UNBACKED_DELIVERY" => ReviewPrReason::InvalidPublication,
        "PROJECTION_INVALID" => ReviewPrReason::InvalidProjection,
        "PR_PROVIDER_RATE_LIMIT" => ReviewPrReason::ProviderRateLimit,
        "PR_PROVIDER_UNAVAILABLE"
        | "PR_CONNECTION_UNAVAILABLE"
        | "PR_AUTHORIZATION_UNAVAILABLE"
        | "PR_SYNC_TIMEOUT"
        | "PR_SYNC_PUBLICATION_TIMEOUT" => ReviewPrReason::ProviderUnavailable,
        "PR_PROVIDER_REJECTED" => ReviewPrReason::ProviderRejected,
        "PR_IDENTITY_CONFLICT" => ReviewPrReason::Conflict,
        "PR_RESULT_UNKNOWN" | "PR_SYNC_ATTEMPTS_EXHAUSTED" => ReviewPrReason::UncertainResult,
        "PR_METADATA_NOT_OBSERVED" | "PR_OBSERVATION_PENDING" | "PR_PROVIDER_REFS_UNSETTLED" => {
            ReviewPrReason::ObservationPending
        }
        _ => ReviewPrReason::Other,
    }
}
