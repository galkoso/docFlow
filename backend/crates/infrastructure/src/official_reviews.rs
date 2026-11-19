//! Atomic official decisions with current role/assignment checks before and
//! after bounded provider reads. No transaction spans provider IO.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use specspace_application::{
    comparison_reader::{BoundProviderFactory, RepositoryBinding},
    official_review::{OfficialReviewRecorded, OfficialReviewRequest, ReviewDecision},
};
use specspace_domain::BranchName;
use sqlx::{FromRow, PgConnection, PgPool, Postgres, Transaction};
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;
mod admission;
mod history;

#[derive(Debug, PartialEq, Eq)]
pub enum OfficialReviewError {
    NotFound,
    Forbidden,
    Invalid,
    Stale,
    BaseOutdated,
    UiUndecided,
    IdempotencyMismatch,
    Quota,
    ProviderUnavailable,
    ProviderRateLimited(u32),
    Capacity,
    Unavailable,
}
impl From<sqlx::Error> for OfficialReviewError {
    fn from(_: sqlx::Error) -> Self {
        Self::Unavailable
    }
}
#[derive(FromRow, PartialEq, Eq)]
pub(crate) struct Scope {
    pub(crate) organization_id: Uuid,
    pub(crate) repository_id: Uuid,
    pub(crate) remote_id: String,
    pub(crate) installation_id: String,
    branch: String,
    pub(crate) fingerprint: String,
    head_sha: String,
    target_sha: String,
    recorded_base_sha: String,
    merge_base_sha: String,
    policy_version: i64,
    configuration_version: i64,
    pub(crate) current: bool,
    role: String,
    review_domains: Vec<String>,
    author_id: Uuid,
    metadata: Value,
}
#[derive(FromRow)]
struct Recorded {
    id: Uuid,
    request_id: Uuid,
    proposal_id: Uuid,
    snapshot_id: Uuid,
    reviewer_id: Uuid,
    request_hash: String,
    domain: String,
    decision: String,
    head_sha: String,
    target_sha: String,
    policy_version: i64,
    configuration_version: i64,
    created_at: sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>,
}
impl Recorded {
    fn dto(self, replayed: bool) -> Result<OfficialReviewRecorded, OfficialReviewError> {
        use specspace_application::review_configuration::ReviewArea;
        Ok(OfficialReviewRecorded {
            id: self.id,
            request_id: self.request_id,
            proposal_id: self.proposal_id,
            snapshot_id: self.snapshot_id,
            reviewer_id: self.reviewer_id,
            domain: match self.domain.as_str() {
                "PRODUCT" => ReviewArea::Product,
                "DEVELOPMENT" => ReviewArea::Development,
                "DESIGN" => ReviewArea::Design,
                _ => return Err(OfficialReviewError::Unavailable),
            },
            decision: match self.decision.as_str() {
                "APPROVE_ALL" => ReviewDecision::ApproveAll,
                "REQUEST_CHANGES" => ReviewDecision::RequestChanges,
                "COMMENT" => ReviewDecision::Comment,
                _ => return Err(OfficialReviewError::Unavailable),
            },
            head_sha: self.head_sha,
            target_sha: self.target_sha,
            policy_version: self.policy_version,
            configuration_version: self.configuration_version,
            recorded_at: self.created_at.to_rfc3339(),
            replayed,
        })
    }
}
async fn scope(
    conn: &mut PgConnection,
    actor: Uuid,
    proposal: Uuid,
    snapshot: Uuid,
    request: &OfficialReviewRequest,
) -> Result<Scope, OfficialReviewError> {
    reviewer_scope(conn, actor, proposal, snapshot, request.domain).await
}
pub(crate) async fn reviewer_scope(
    conn: &mut PgConnection,
    actor: Uuid,
    proposal: Uuid,
    snapshot: Uuid,
    domain: specspace_application::review_configuration::ReviewArea,
) -> Result<Scope, OfficialReviewError> {
    let mut row=sqlx::query_as::<_,Scope>("SELECT p.organization_id,h.repository_id,h.remote_id,h.installation_id,h.branch,h.fingerprint,h.head_sha,h.target_sha,h.recorded_base_sha,h.merge_base_sha,h.policy_version,h.configuration_version,false AS current,m.role,m.review_domains,p.author_id,h.metadata FROM proposals p JOIN review_snapshots h ON h.organization_id=p.organization_id AND h.proposal_id=p.id JOIN repositories r ON r.id=p.repository_id AND r.organization_id=p.organization_id JOIN organizations o ON o.id=p.organization_id JOIN memberships m ON m.organization_id=p.organization_id JOIN users u ON u.id=m.user_id WHERE p.id=$1 AND h.id=$2 AND h.state='READY' AND u.id=$3 AND u.status='ACTIVE' FOR UPDATE OF p FOR SHARE OF h,r,o,m,u")
        .bind(proposal).bind(snapshot).bind(actor).fetch_optional(&mut *conn).await?.ok_or(OfficialReviewError::NotFound)?;
    if !matches!(row.role.as_str(), "REVIEWER" | "MAINTAINER" | "ADMIN")
        || !row.review_domains.iter().any(|d| d == domain.as_str())
        || row.author_id == actor
        || row
            .metadata
            .pointer("/context/author_id")
            .and_then(Value::as_str)
            == Some(&actor.to_string())
        || !row
            .metadata
            .get("assignments")
            .and_then(Value::as_array)
            .is_some_and(|a| {
                a.iter().any(|v| {
                    v.get("reviewer_id").and_then(Value::as_str) == Some(&actor.to_string())
                        && v.get("domain").and_then(Value::as_str) == Some(domain.as_str())
                })
            })
    {
        return Err(OfficialReviewError::Forbidden);
    }
    row.current = sqlx::query_scalar("SELECT pr_sync_snapshot_current($1,$2,$3)")
        .bind(row.organization_id)
        .bind(proposal)
        .bind(snapshot)
        .fetch_one(&mut *conn)
        .await?;
    Ok(row)
}
fn current(scope: &Scope, request: &OfficialReviewRequest) -> Result<(), OfficialReviewError> {
    if !scope.current || scope.fingerprint != request.expected_fingerprint {
        return Err(OfficialReviewError::Stale);
    }
    if request.decision == ReviewDecision::ApproveAll {
        if scope.recorded_base_sha != scope.target_sha || scope.merge_base_sha != scope.target_sha {
            return Err(OfficialReviewError::BaseOutdated);
        }
        if !matches!(
            scope
                .metadata
                .pointer("/summary/ui_impact")
                .and_then(Value::as_str),
            Some("YES" | "NO")
        ) {
            return Err(OfficialReviewError::UiUndecided);
        }
    }
    Ok(())
}
async fn replay(
    conn: &mut PgConnection,
    org: Uuid,
    actor: Uuid,
    proposal: Uuid,
    snapshot: Uuid,
    request: Uuid,
    hash: &str,
) -> Result<Option<OfficialReviewRecorded>, OfficialReviewError> {
    let old = sqlx::query_as::<_, Recorded>(
        "SELECT * FROM official_reviews WHERE organization_id=$1 AND request_id=$2",
    )
    .bind(org)
    .bind(request)
    .fetch_optional(conn)
    .await?;
    if let Some(old) = old {
        if old.proposal_id != proposal
            || old.snapshot_id != snapshot
            || old.reviewer_id != actor
            || old.request_hash != hash
        {
            return Err(OfficialReviewError::IdempotencyMismatch);
        }
        return Ok(Some(old.dto(true)?));
    }
    Ok(None)
}
pub struct PgOfficialReviews {
    pool: PgPool,
    slots: Arc<tokio::sync::Semaphore>,
}
impl PgOfficialReviews {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            slots: Arc::new(tokio::sync::Semaphore::new(8)),
        }
    }
    async fn transaction(&self) -> Result<Transaction<'_, Postgres>, OfficialReviewError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SET LOCAL lock_timeout='5s'")
            .execute(&mut *tx)
            .await?;
        sqlx::query("SET LOCAL statement_timeout='10s'")
            .execute(&mut *tx)
            .await?;
        Ok(tx)
    }
    pub async fn submit(
        &self,
        actor: Uuid,
        proposal: Uuid,
        snapshot: Uuid,
        mut request: OfficialReviewRequest,
        factory: &dyn BoundProviderFactory,
    ) -> Result<OfficialReviewRecorded, OfficialReviewError> {
        request
            .normalize()
            .map_err(|_| OfficialReviewError::Invalid)?;
        if actor.is_nil() || proposal.is_nil() || snapshot.is_nil() {
            return Err(OfficialReviewError::Invalid);
        }
        let hash = format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&json!([
                    "official-review-v1",
                    actor,
                    proposal,
                    snapshot,
                    request.expected_fingerprint,
                    request.domain,
                    request.decision,
                    request.explanation
                ]))
                .expect("bounded serializable review")
            )
        );
        let mut tx = self.transaction().await?;
        let before = scope(&mut tx, actor, proposal, snapshot, &request).await?;
        if let Some(old) = replay(
            &mut tx,
            before.organization_id,
            actor,
            proposal,
            snapshot,
            request.request_id,
            &hash,
        )
        .await?
        {
            tx.commit().await?;
            return Ok(old);
        }
        current(&before, &request)?;
        let _permit = self
            .slots
            .try_acquire()
            .map_err(|_| OfficialReviewError::Capacity)?;
        admission::admit(&mut tx, &before, actor, proposal, snapshot, &request, &hash).await?;
        tx.commit().await?;
        let number = |v: &str| {
            v.parse::<u64>()
                .ok()
                .filter(|n| *n > 0)
                .ok_or(OfficialReviewError::Unavailable)
        };
        let binding = RepositoryBinding {
            repository: before.repository_id,
            remote_id: number(&before.remote_id)?,
            installation_id: number(&before.installation_id)?,
        };
        let branch =
            BranchName::parse(&before.branch).map_err(|_| OfficialReviewError::Unavailable)?;
        let observed = tokio::time::timeout(Duration::from_secs(15), async {
            let provider = factory
                .provider_bound(&binding)
                .await
                .map_err(|e| match e {
                    specspace_application::comparison_reader::ComparisonReadError::Provider(
                        error,
                    )
                    | specspace_application::comparison_reader::ComparisonReadError::Source(
                        specspace_application::book::BookError::Provider(error),
                    ) => admission::provider_error(error),
                    _ => OfficialReviewError::ProviderUnavailable,
                })?;
            for _ in 0..2 {
                let head = provider
                    .branch_head(&branch)
                    .await
                    .map_err(admission::provider_error)?;
                let target = provider
                    .main_head()
                    .await
                    .map_err(admission::provider_error)?;
                if head.as_str() != before.head_sha || target.as_str() != before.target_sha {
                    return Err(OfficialReviewError::Stale);
                }
            }
            Ok(())
        })
        .await
        .unwrap_or(Err(OfficialReviewError::ProviderUnavailable));
        if let Err(error) = observed {
            if let OfficialReviewError::ProviderRateLimited(seconds) = error {
                admission::backoff(&self.pool, &before, seconds).await?;
            }
            admission::failure(
                &self.pool,
                &before,
                actor,
                proposal,
                snapshot,
                request.request_id,
                &error,
            )
            .await?;
            return Err(error);
        }
        let mut tx = self.transaction().await?;
        let after = scope(&mut tx, actor, proposal, snapshot, &request).await?;
        if let Some(old) = replay(
            &mut tx,
            after.organization_id,
            actor,
            proposal,
            snapshot,
            request.request_id,
            &hash,
        )
        .await?
        {
            tx.commit().await?;
            return Ok(old);
        }
        current(&after, &request)?;
        if before.organization_id != after.organization_id
            || before.repository_id != after.repository_id
            || before.remote_id != after.remote_id
            || before.installation_id != after.installation_id
            || before.branch != after.branch
        {
            return Err(OfficialReviewError::Stale);
        }
        // Cross-proposal actor/org admission counts are serialized by this lock.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('official-review-quota:' || $1::uuid::text,0))")
            .bind(after.organization_id)
            .execute(&mut *tx)
            .await?;
        if let Some(old) = replay(
            &mut tx,
            after.organization_id,
            actor,
            proposal,
            snapshot,
            request.request_id,
            &hash,
        )
        .await?
        {
            tx.commit().await?;
            return Ok(old);
        }
        let allowed:bool=sqlx::query_scalar("SELECT count(*)<200 AND count(*) FILTER(WHERE reviewer_id=$2)<30 FROM official_reviews WHERE organization_id=$1 AND created_at>clock_timestamp()-interval '10 minutes'").bind(after.organization_id).bind(actor).fetch_one(&mut *tx).await?;
        if !allowed {
            return Err(OfficialReviewError::Quota);
        }
        let (id, audit, event) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let row=sqlx::query_as::<_,Recorded>("INSERT INTO official_reviews(id,organization_id,proposal_id,snapshot_id,reviewer_id,request_id,request_hash,domain,decision,explanation,fingerprint,head_sha,target_sha,policy_version,configuration_version,audit_id,event_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17) RETURNING *")
            .bind(id).bind(after.organization_id).bind(proposal).bind(snapshot).bind(actor).bind(request.request_id).bind(&hash).bind(request.domain.as_str()).bind(request.decision.as_str()).bind(&request.explanation).bind(&after.fingerprint).bind(&after.head_sha).bind(&after.target_sha).bind(after.policy_version).bind(after.configuration_version).bind(audit).bind(event).fetch_one(&mut *tx).await?;
        if request.decision != ReviewDecision::Comment {
            sqlx::query("INSERT INTO official_review_effective(organization_id,proposal_id,snapshot_id,reviewer_id,domain,review_id) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(organization_id,proposal_id,snapshot_id,reviewer_id,domain) DO UPDATE SET review_id=excluded.review_id")
                .bind(after.organization_id).bind(proposal).bind(snapshot).bind(actor).bind(request.domain.as_str()).bind(id).execute(&mut *tx).await?;
            if request.decision == ReviewDecision::RequestChanges {
                sqlx::query("UPDATE proposals SET state='CHANGES_REQUESTED',updated_at=clock_timestamp() WHERE id=$1").bind(proposal).execute(&mut *tx).await?;
            } else {
                // Formal approval state waits for the full policy evaluator.
                let blocking:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM official_review_effective x JOIN official_reviews r ON r.id=x.review_id JOIN memberships m ON m.organization_id=x.organization_id AND m.user_id=x.reviewer_id JOIN users u ON u.id=m.user_id WHERE x.organization_id=$1 AND x.proposal_id=$2 AND x.snapshot_id=$3 AND r.decision='REQUEST_CHANGES' AND u.status='ACTIVE' AND m.role IN ('REVIEWER','MAINTAINER','ADMIN') AND x.domain=ANY(m.review_domains))").bind(after.organization_id).bind(proposal).bind(snapshot).fetch_one(&mut *tx).await?;
                if !blocking {
                    sqlx::query("UPDATE proposals SET state='IN_REVIEW',updated_at=clock_timestamp() WHERE id=$1 AND state='CHANGES_REQUESTED'").bind(proposal).execute(&mut *tx).await?;
                }
            }
        }
        sqlx::query("INSERT INTO audit_events(id,organization_id,actor_id,action,entity_id,after_ref,result) VALUES($1,$2,$3,'OFFICIAL_REVIEW_RECORDED',$4,$5,$6)").bind(audit).bind(after.organization_id).bind(actor).bind(proposal).bind(id.to_string()).bind(request.decision.as_str()).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO outbox(id,organization_id,entity_id,event_type,payload) VALUES($1,$2,$3,'REVIEW_DECISION_RECORDED',$4)").bind(event).bind(after.organization_id).bind(proposal).bind(json!({"review_id":id,"snapshot_id":snapshot,"head_sha":after.head_sha,"target_sha":after.target_sha,"decision":request.decision})).execute(&mut *tx).await?;
        tx.commit().await?;
        row.dto(false)
    }
}
