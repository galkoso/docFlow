use super::*;
use serde_json::Value;
use specspace_application::{
    review_configuration::ReviewArea,
    review_readiness::ReviewReadiness,
    review_snapshot::{ReviewSnapshotMetadata, ReviewSnapshotUiImpact},
};
use specspace_domain::{
    policy::*,
    review_check::{self, Conclusion, Gate, Issue},
};
use sqlx::PgPool;

#[derive(Debug, PartialEq, Eq)]
pub enum ReadinessError {
    Invalid,
    NotFound,
    Unavailable,
}
impl From<sqlx::Error> for ReadinessError {
    fn from(_: sqlx::Error) -> Self {
        Self::Unavailable
    }
}
pub struct PgReviewReadiness {
    pool: PgPool,
}

#[derive(FromRow)]
struct Header {
    fingerprint: String,
    plan_fingerprint: String,
    head_sha: String,
    target_sha: String,
    recorded_base_sha: String,
    merge_base_sha: String,
    repository_id: Uuid,
    branch: String,
    policy_version: i64,
    configuration_version: i64,
    metadata: Value,
    file_count: i32,
    actual_files: i64,
    media_count: i32,
    actual_media: i64,
    media_validation_version: i32,
    author_id: Uuid,
    current_policy: i64,
    current_head: Option<String>,
    current_target: Option<String>,
    state: String,
    current: bool,
}
#[derive(FromRow)]
struct Member {
    user_id: Uuid,
    active: bool,
    role: String,
    review_domains: Vec<String>,
}
pub(super) fn domain(value: ReviewArea) -> ReviewDomain {
    match value {
        ReviewArea::Product => ReviewDomain::Product,
        ReviewArea::Development => ReviewDomain::Development,
        ReviewArea::Design => ReviewDomain::Design,
    }
}
fn member(value: Member, org: Uuid) -> Result<Membership, ReadinessError> {
    let role = match value.role.as_str() {
        "READER" => Role::Reader,
        "CONTRIBUTOR" => Role::Contributor,
        "REVIEWER" => Role::Reviewer,
        "MAINTAINER" => Role::Maintainer,
        "ADMIN" => Role::Admin,
        _ => return Err(ReadinessError::Unavailable),
    };
    let review_domains = value
        .review_domains
        .iter()
        .map(|value| match value.as_str() {
            "PRODUCT" => Ok(ReviewDomain::Product),
            "DEVELOPMENT" => Ok(ReviewDomain::Development),
            "DESIGN" => Ok(ReviewDomain::Design),
            _ => Err(ReadinessError::Unavailable),
        })
        .collect::<Result<_, _>>()?;
    Ok(Membership {
        organization_id: org,
        user_id: value.user_id,
        active: value.active,
        role,
        review_domains,
    })
}
fn state(value: &str) -> Result<ProposalState, ReadinessError> {
    Ok(match value {
        "PROVISIONING" => ProposalState::Provisioning,
        "DRAFT" => ProposalState::Draft,
        "IN_REVIEW" => ProposalState::InReview,
        "CHANGES_REQUESTED" => ProposalState::ChangesRequested,
        "APPROVED" => ProposalState::Approved,
        "MERGING" => ProposalState::Merging,
        "MERGED" => ProposalState::Merged,
        "CLOSED" => ProposalState::Closed,
        _ => return Err(ReadinessError::Unavailable),
    })
}
fn metadata(h: &Header, proposal: Uuid) -> Result<ReviewSnapshotMetadata, ReadinessError> {
    let m: ReviewSnapshotMetadata =
        serde_json::from_value(h.metadata.clone()).map_err(|_| ReadinessError::Unavailable)?;
    let s = &m.summary;
    let ui_matches = matches!(
        (&s.ui_impact, &m.context.ui_impact),
        (ReviewSnapshotUiImpact::Yes, ReviewSnapshotUiImpact::Yes)
            | (ReviewSnapshotUiImpact::No, ReviewSnapshotUiImpact::No)
            | (
                ReviewSnapshotUiImpact::Undecided,
                ReviewSnapshotUiImpact::Undecided
            )
    );
    if !ui_matches
        || m.schema_version != 1
        || m.assignments.len() > 200
        || m.context.author_id.is_nil()
        || s.proposal_id != proposal
        || s.repository_id != h.repository_id
        || s.branch != h.branch
        || s.fingerprint != h.plan_fingerprint
        || s.head_sha != h.head_sha
        || s.target_sha != h.target_sha
        || s.recorded_base_sha != h.recorded_base_sha
        || s.merge_base_sha != h.merge_base_sha
        || i64::try_from(s.policy_version).ok() != Some(h.policy_version)
        || i64::try_from(s.configuration_version).ok() != Some(h.configuration_version)
        || i32::try_from(s.changed_files).ok() != Some(h.file_count)
        || i32::try_from(s.media_checks_required).ok() != Some(h.media_count)
        || m.validation.findings.len() > 1000
        || m.validation.findings_total < m.validation.findings.len()
        || s.content_findings_total != m.validation.findings_total
    {
        return Err(ReadinessError::Unavailable);
    }
    Ok(m)
}

fn coverage(values: impl Iterator<Item = Verification>) -> Verification {
    let mut outcome = Verification::Passed;
    let mut any = false;
    for value in values {
        any = true;
        match value {
            Verification::Unknown => outcome = Verification::Unknown,
            Verification::Failed => return Verification::Failed,
            Verification::Pending if outcome == Verification::Passed => {
                outcome = Verification::Pending
            }
            Verification::Pending => (),
            Verification::Passed => (),
        }
    }
    if any { outcome } else { Verification::Pending }
}
impl PgReviewReadiness {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Reads current stored authority and immutable evidence in one transaction.
    /// This report performs no provider IO, state transition or publication.
    pub async fn stored(
        &self,
        actor: Uuid,
        proposal: Uuid,
        snapshot: Uuid,
    ) -> Result<ReviewReadiness, ReadinessError> {
        if [actor, proposal, snapshot].iter().any(Uuid::is_nil) {
            return Err(ReadinessError::Invalid);
        }
        let mut tx = self.pool.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
            .execute(&mut *tx)
            .await?;
        sqlx::query("SET LOCAL lock_timeout='5s'")
            .execute(&mut *tx)
            .await?;
        sqlx::query("SET LOCAL statement_timeout='10s'")
            .execute(&mut *tx)
            .await?;
        let org = crate::review_snapshots::read_access(&mut tx, actor, proposal)
            .await
            .map_err(|e| match e {
                crate::review_submissions::SubmissionError::NotFound => ReadinessError::NotFound,
                _ => ReadinessError::Unavailable,
            })?;
        let h = sqlx::query_as::<_, Header>(
            "SELECT h.fingerprint,h.plan_fingerprint,h.head_sha,h.target_sha,h.recorded_base_sha,h.merge_base_sha,h.repository_id,h.branch,
               h.policy_version,h.configuration_version,h.metadata,h.file_count,h.media_count,h.media_validation_version,
               (SELECT count(*) FROM review_snapshot_files f WHERE f.organization_id=h.organization_id AND f.proposal_id=h.proposal_id AND f.snapshot_id=h.id) AS actual_files,
               (SELECT count(*) FROM review_snapshot_media m WHERE m.organization_id=h.organization_id AND m.proposal_id=h.proposal_id AND m.snapshot_id=h.id) AS actual_media,
               p.author_id,o.policy_version AS current_policy,p.head_sha AS current_head,p.target_sha AS current_target,p.state,
               pr_sync_snapshot_current(h.organization_id,h.proposal_id,h.id) AS current
             FROM review_snapshots h JOIN proposals p ON p.organization_id=h.organization_id AND p.id=h.proposal_id
             JOIN organizations o ON o.id=p.organization_id
             WHERE h.organization_id=$1 AND h.proposal_id=$2 AND h.id=$3 AND h.state='READY'"
        ).bind(org).bind(proposal).bind(snapshot).fetch_optional(&mut *tx).await?.ok_or(ReadinessError::NotFound)?;
        let m = metadata(&h, proposal)?;
        let ids: Vec<_> = m.assignments.iter().map(|a| a.reviewer_id).collect();
        let members = sqlx::query_as::<_, Member>("SELECT m.user_id,u.status='ACTIVE' AS active,m.role,m.review_domains FROM memberships m JOIN users u ON u.id=m.user_id WHERE m.organization_id=$1 AND m.user_id=ANY($2) ORDER BY m.user_id")
            .bind(org).bind(ids).fetch_all(&mut *tx).await?.into_iter().map(|m| member(m, org)).collect::<Result<Vec<_>, _>>()?;
        let assignments: Vec<_> = m
            .assignments
            .iter()
            .map(|a| Assignment {
                reviewer_id: a.reviewer_id,
                domain: domain(a.domain),
            })
            .collect();
        let required: Vec<_> = m
            .assignments
            .iter()
            .filter(|a| a.required)
            .map(|a| Assignment {
                reviewer_id: a.reviewer_id,
                domain: domain(a.domain),
            })
            .collect();
        let decisions = super::decisions::read(&mut tx, org, proposal, snapshot).await?;
        let reviews: Vec<_> = decisions
            .iter()
            .filter_map(|d| d.policy_review(snapshot))
            .collect();
        let snapshot_context = Snapshot {
            id: snapshot,
            head_sha: h.head_sha.clone(),
            target_sha: h.target_sha.clone(),
            policy_version: u64::try_from(h.policy_version)
                .map_err(|_| ReadinessError::Unavailable)?,
        };
        let approvals = ApprovalContext {
            organization_id: org,
            author_id: h.author_id,
            snapshot: &snapshot_context,
            current_head: h.current_head.as_deref().unwrap_or(""),
            current_target: h.current_target.as_deref().unwrap_or(""),
            policy_version: u64::try_from(h.current_policy)
                .map_err(|_| ReadinessError::Unavailable)?,
            ui_impact: match m.summary.ui_impact {
                ReviewSnapshotUiImpact::Yes => UiImpact::Yes,
                ReviewSnapshotUiImpact::No => UiImpact::No,
                ReviewSnapshotUiImpact::Undecided => UiImpact::Undecided,
            },
            memberships: &members,
            assignments: &assignments,
            reviews: &reviews,
        };
        let content = if m.validation.blockers_total > 0 || h.file_count == 0 {
            Verification::Failed
        } else if !m.validation.complete || i64::from(h.file_count) != h.actual_files {
            Verification::Unknown
        } else {
            Verification::Passed
        };
        let media = if i64::from(h.media_count) == h.actual_media
            && u32::try_from(h.media_validation_version).ok()
                == Some(specspace_document_engine::media::VALIDATION_VERSION)
        {
            Verification::Passed
        } else {
            Verification::Unknown
        };
        let mut evaluation = review_check::evaluate(&review_check::Context {
            approvals: &approvals,
            required_assignments: &required,
            proposal_state: state(&h.state)?,
            active_snapshot: h.current,
            base_updated: if h.recorded_base_sha == h.target_sha && h.merge_base_sha == h.target_sha
            {
                Verification::Passed
            } else {
                Verification::Pending
            },
            content_validation: content,
            media_validation: media,
            section_coverage: coverage(decisions.iter().filter_map(|d| d.positive_coverage())),
            blocking_threads_resolved: super::discussions(&mut tx, org, proposal)
                .await?
                .verification(),
            blocking_questions_resolved: super::questions(&mut tx, org, proposal)
                .await?
                .verification(),
        });
        evaluation.issues.push(Issue {
            gate: Gate::Evidence,
            conclusion: Conclusion::Pending,
            code: "PROVIDER_REFS_UNVERIFIED",
        });
        if evaluation.conclusion == Conclusion::Success {
            evaluation.conclusion = Conclusion::Pending;
        }
        let now: sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc> =
            sqlx::query_scalar("SELECT clock_timestamp()")
                .fetch_one(&mut *tx)
                .await?;
        tx.commit().await?;
        Ok(ReviewReadiness {
            actor_id: actor,
            proposal_id: proposal,
            snapshot_id: snapshot,
            fingerprint: h.fingerprint,
            head_sha: h.head_sha,
            target_sha: h.target_sha,
            policy_version: h.policy_version,
            configuration_version: h.configuration_version,
            current_for_metadata: h.current,
            provider_refs_verified: false,
            read_at: now.to_rfc3339(),
            evaluation: evaluation.into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn section_coverage_needs_positive_evidence_and_preserves_the_strongest_blocker() {
        use Verification::*;
        for (values, expected) in [
            (vec![], Pending),
            (vec![Passed, Passed], Passed),
            (vec![Passed, Pending], Pending),
            (vec![Pending, Unknown], Unknown),
            (vec![Unknown, Pending], Unknown),
            (vec![Unknown, Failed], Failed),
            (vec![Failed, Unknown], Failed),
        ] {
            assert_eq!(coverage(values.into_iter()), expected);
        }
    }
}
