use super::*;
use crate::review_submissions::SubmissionError;
use specspace_application::{
    official_review::{OfficialReviewEntry, OfficialReviewHistory},
    review_configuration::ReviewArea,
};

#[derive(FromRow)]
struct Header {
    fingerprint: String,
    head_sha: String,
    target_sha: String,
    policy_version: i64,
    configuration_version: i64,
    recorded_base_sha: String,
    merge_base_sha: String,
    metadata: Value,
    author_id: Uuid,
}
#[derive(FromRow)]
struct Entry {
    id: Uuid,
    reviewer_id: Uuid,
    reviewer_name: String,
    domain: String,
    decision: String,
    explanation: String,
    created_at: sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>,
    effective: bool,
    reviewer_eligible: bool,
}
fn area(value: &str) -> Result<ReviewArea, OfficialReviewError> {
    match value {
        "PRODUCT" => Ok(ReviewArea::Product),
        "DEVELOPMENT" => Ok(ReviewArea::Development),
        "DESIGN" => Ok(ReviewArea::Design),
        _ => Err(OfficialReviewError::Unavailable),
    }
}
impl PgOfficialReviews {
    pub async fn history(
        &self,
        actor: Uuid,
        proposal: Uuid,
        snapshot: Uuid,
        after: Option<Uuid>,
    ) -> Result<OfficialReviewHistory, OfficialReviewError> {
        if actor.is_nil()
            || proposal.is_nil()
            || snapshot.is_nil()
            || after.is_some_and(|v| v.is_nil())
        {
            return Err(OfficialReviewError::Invalid);
        }
        let mut tx = self.transaction().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
            .execute(&mut *tx)
            .await?;
        let org = crate::review_snapshots::read_access(&mut tx, actor, proposal)
            .await
            .map_err(|e| match e {
                SubmissionError::NotFound => OfficialReviewError::NotFound,
                _ => OfficialReviewError::Unavailable,
            })?;
        let h=sqlx::query_as::<_,Header>("SELECT h.fingerprint,h.head_sha,h.target_sha,h.policy_version,h.configuration_version,h.recorded_base_sha,h.merge_base_sha,h.metadata,p.author_id FROM review_snapshots h JOIN proposals p ON p.id=h.proposal_id AND p.organization_id=h.organization_id WHERE h.organization_id=$1 AND h.proposal_id=$2 AND h.id=$3 AND h.state='READY'").bind(org).bind(proposal).bind(snapshot).fetch_optional(&mut *tx).await?.ok_or(OfficialReviewError::NotFound)?;
        let current: bool = sqlx::query_scalar("SELECT pr_sync_snapshot_current($1,$2,$3)")
            .bind(org)
            .bind(proposal)
            .bind(snapshot)
            .fetch_one(&mut *tx)
            .await?;
        let (role, qualified): (String, Vec<String>) = sqlx::query_as(
            "SELECT role,review_domains FROM memberships WHERE organization_id=$1 AND user_id=$2",
        )
        .bind(org)
        .bind(actor)
        .fetch_one(&mut *tx)
        .await?;
        let mut domains = Vec::new();
        if current
            && h.author_id != actor
            && h.metadata
                .pointer("/context/author_id")
                .and_then(Value::as_str)
                != Some(&actor.to_string())
            && matches!(role.as_str(), "REVIEWER" | "MAINTAINER" | "ADMIN")
        {
            for item in h
                .metadata
                .get("assignments")
                .and_then(Value::as_array)
                .ok_or(OfficialReviewError::Unavailable)?
            {
                if item.get("reviewer_id").and_then(Value::as_str) == Some(&actor.to_string()) {
                    let domain = item
                        .get("domain")
                        .and_then(Value::as_str)
                        .ok_or(OfficialReviewError::Unavailable)?;
                    if qualified.iter().any(|v| v == domain) {
                        domains.push(area(domain)?);
                    }
                }
            }
        }
        let mut unviewed_domains = Vec::new();
        let mut approve_domains = Vec::new();
        for &domain in &domains {
            let coverage = crate::review_coverage::read(
                &mut tx,
                crate::review_coverage::CoverageScope {
                    organization: org,
                    proposal,
                    snapshot,
                    actor,
                    domain,
                    fingerprint: &h.fingerprint,
                },
                0,
                1,
            )
            .await
            .map_err(|_| OfficialReviewError::Unavailable)?;
            if coverage.has_unviewed() {
                unviewed_domains.push(domain);
            }
            let approvals = crate::review_approval_coverage::read(
                &mut tx,
                crate::review_coverage::CoverageScope {
                    organization: org,
                    proposal,
                    snapshot,
                    actor,
                    domain,
                    fingerprint: &h.fingerprint,
                },
            )
            .await?;
            if approvals.complete() {
                approve_domains.push(domain);
            }
        }
        let unviewed_content_warning = domains.is_empty() || !unviewed_domains.is_empty();
        let before = if let Some(id) = after {
            sqlx::query_scalar::<_,i64>("SELECT revision FROM official_reviews WHERE id=$1 AND organization_id=$2 AND proposal_id=$3 AND snapshot_id=$4").bind(id).bind(org).bind(proposal).bind(snapshot).fetch_optional(&mut *tx).await?.ok_or(OfficialReviewError::Invalid)?
        } else {
            i64::MAX
        };
        let mut rows=sqlx::query_as::<_,Entry>("SELECT r.id,r.reviewer_id,left(u.display_name,200) AS reviewer_name,r.domain,r.decision,r.explanation,r.created_at,coalesce(x.review_id=r.id,false) AS effective,coalesce(u.status='ACTIVE' AND m.role IN ('REVIEWER','MAINTAINER','ADMIN') AND r.domain=ANY(m.review_domains) AND p.author_id<>r.reviewer_id AND h.metadata#>>'{context,author_id}'<>r.reviewer_id::text,false) AS reviewer_eligible FROM official_reviews r JOIN users u ON u.id=r.reviewer_id JOIN proposals p ON p.id=r.proposal_id AND p.organization_id=r.organization_id JOIN review_snapshots h ON h.id=r.snapshot_id LEFT JOIN memberships m ON m.organization_id=r.organization_id AND m.user_id=r.reviewer_id LEFT JOIN official_review_effective x ON x.organization_id=r.organization_id AND x.proposal_id=r.proposal_id AND x.snapshot_id=r.snapshot_id AND x.reviewer_id=r.reviewer_id AND x.domain=r.domain WHERE r.organization_id=$1 AND r.proposal_id=$2 AND r.snapshot_id=$3 AND r.revision<$4 ORDER BY r.revision DESC LIMIT 21")
            .bind(org).bind(proposal).bind(snapshot).bind(before).fetch_all(&mut *tx).await?;
        let more = rows.len() > 20;
        rows.truncate(20);
        let next_after = if more {
            rows.last().map(|v| v.id)
        } else {
            None
        };
        let entries = rows
            .into_iter()
            .map(|r| {
                Ok(OfficialReviewEntry {
                    id: r.id,
                    reviewer_id: r.reviewer_id,
                    reviewer_name: r.reviewer_name,
                    domain: area(&r.domain)?,
                    decision: match r.decision.as_str() {
                        "APPROVE" => ReviewDecision::Approve,
                        "APPROVE_ALL" => ReviewDecision::ApproveAll,
                        "REQUEST_CHANGES" => ReviewDecision::RequestChanges,
                        "COMMENT" => ReviewDecision::Comment,
                        _ => return Err(OfficialReviewError::Unavailable),
                    },
                    explanation: r.explanation,
                    recorded_at: r.created_at.to_rfc3339(),
                    effective: r.effective,
                    reviewer_eligible: r.reviewer_eligible,
                    valid_for_stored_context: current && r.effective && r.reviewer_eligible,
                })
            })
            .collect::<Result<Vec<_>, OfficialReviewError>>()?;
        let now: sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc> =
            sqlx::query_scalar("SELECT clock_timestamp()")
                .fetch_one(&mut *tx)
                .await?;
        let available = !domains.is_empty()
            && h.recorded_base_sha == h.target_sha
            && h.merge_base_sha == h.target_sha
            && matches!(
                h.metadata
                    .pointer("/summary/ui_impact")
                    .and_then(Value::as_str),
                Some("YES" | "NO")
            );
        if !available {
            approve_domains.clear();
        }
        tx.commit().await?;
        Ok(OfficialReviewHistory {
            proposal_id: proposal,
            snapshot_id: snapshot,
            fingerprint: h.fingerprint,
            head_sha: h.head_sha,
            target_sha: h.target_sha,
            policy_version: h.policy_version,
            configuration_version: h.configuration_version,
            current_for_metadata: current,
            approve_all_available: available,
            approve_domains,
            domains,
            unviewed_content_warning,
            unviewed_domains,
            read_at: now.to_rfc3339(),
            entries,
            next_after,
        })
    }
}
