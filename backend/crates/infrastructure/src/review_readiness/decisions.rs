//! Unpaged effective decisions for the exact sealed snapshot. A display page of
//! comments must never be used as the approval input for policy evaluation.
use specspace_application::{official_review::ReviewDecision, review_configuration::ReviewArea};
use specspace_domain::review_check::Verification;
use sqlx::{FromRow, PgConnection};
use uuid::Uuid;

pub(crate) struct EffectiveDecision {
    id: Uuid,
    reviewer: Uuid,
    domain: ReviewArea,
    decision: ReviewDecision,
    eligible: bool,
    coverage: Verification,
}
impl EffectiveDecision {
    pub fn policy_review(&self, snapshot: Uuid) -> Option<specspace_domain::policy::Review> {
        use specspace_domain::policy::{Decision, Review};
        if !self.validates_entry(self.id, self.reviewer, self.domain, self.decision) {
            return None;
        }
        Some(Review {
            snapshot_id: snapshot,
            reviewer_id: self.reviewer,
            domain: super::report::domain(self.domain),
            decision: if self.decision == ReviewDecision::RequestChanges {
                Decision::RequestChanges
            } else {
                Decision::Approve
            },
        })
    }
    pub fn positive_coverage(&self) -> Option<Verification> {
        (self.eligible
            && matches!(
                self.decision,
                ReviewDecision::Approve | ReviewDecision::ApproveAll
            ))
        .then_some(self.coverage)
    }
    /// Stored eligibility/coverage only. The caller separately checks current
    /// proposal/snapshot context; this never proves provider freshness.
    pub fn validates_entry(
        &self,
        id: Uuid,
        reviewer: Uuid,
        domain: ReviewArea,
        decision: ReviewDecision,
    ) -> bool {
        self.id == id
            && self.reviewer == reviewer
            && self.domain == domain
            && self.decision == decision
            && self.eligible
            && (self.decision == ReviewDecision::RequestChanges
                || self.coverage == Verification::Passed)
    }
}

#[derive(FromRow)]
struct Row {
    id: Uuid,
    reviewer: Uuid,
    domain: String,
    decision: Option<String>,
    context_matches: bool,
    eligible: bool,
    individual_coverage: Option<bool>,
}
impl Row {
    fn decode(self) -> Result<EffectiveDecision, sqlx::Error> {
        let invalid = || sqlx::Error::Protocol("invalid effective review evidence".into());
        if !self.context_matches || self.id.is_nil() || self.reviewer.is_nil() {
            return Err(invalid());
        }
        let domain = match self.domain.as_str() {
            "PRODUCT" => ReviewArea::Product,
            "DEVELOPMENT" => ReviewArea::Development,
            "DESIGN" => ReviewArea::Design,
            _ => return Err(invalid()),
        };
        let (decision, coverage) = match self.decision.as_deref() {
            Some("APPROVE") => (
                ReviewDecision::Approve,
                match self.individual_coverage {
                    Some(true) => Verification::Passed,
                    Some(false) => Verification::Pending,
                    None => Verification::Unknown,
                },
            ),
            // This is an explicit snapshot override, never a synthetic set of
            // individual approvals. No unit rows are inserted or inferred.
            Some("APPROVE_ALL") => (ReviewDecision::ApproveAll, Verification::Passed),
            Some("REQUEST_CHANGES") => (ReviewDecision::RequestChanges, Verification::Unknown),
            _ => return Err(invalid()),
        };
        Ok(EffectiveDecision {
            id: self.id,
            reviewer: self.reviewer,
            domain,
            decision,
            eligible: self.eligible,
            coverage,
        })
    }
}

/// The caller must authorize and load the READY snapshot header in the same
/// consistent transaction first. At most 200 frozen assignments are supported;
/// excess pointers fail rather than silently truncating policy inputs.
pub(crate) async fn read(
    conn: &mut PgConnection,
    organization: Uuid,
    proposal: Uuid,
    snapshot: Uuid,
) -> Result<Vec<EffectiveDecision>, sqlx::Error> {
    let rows = sqlx::query_as::<_, Row>(
        "SELECT x.review_id AS id,x.reviewer_id AS reviewer,x.domain,r.decision,
           coalesce(ROW(r.fingerprint,r.head_sha,r.target_sha,r.policy_version,r.configuration_version)
             = ROW(h.fingerprint,h.head_sha,h.target_sha,h.policy_version,h.configuration_version),false) AS context_matches,
           coalesce(u.status='ACTIVE' AND m.role IN ('REVIEWER','MAINTAINER','ADMIN')
             AND x.domain=ANY(m.review_domains) AND p.author_id<>x.reviewer_id
             AND h.metadata#>>'{context,author_id}'<>x.reviewer_id::text
             AND EXISTS(SELECT 1 FROM jsonb_array_elements(h.metadata->'assignments') a
               WHERE a->>'reviewer_id'=x.reviewer_id::text AND a->>'domain'=x.domain),false) AS eligible,
           CASE WHEN r.decision='APPROVE' THEN (
             SELECT c.available AND c.total_units>0 AND c.approved_units=c.total_units
             FROM review_unit_approval_coverage(h.organization_id,h.proposal_id,h.id,x.reviewer_id,x.domain,h.fingerprint) c
           ) ELSE NULL END AS individual_coverage
         FROM review_snapshots h JOIN proposals p ON p.organization_id=h.organization_id AND p.id=h.proposal_id
         JOIN official_review_effective x ON x.organization_id=h.organization_id AND x.proposal_id=h.proposal_id AND x.snapshot_id=h.id
         LEFT JOIN official_reviews r ON r.organization_id=x.organization_id AND r.proposal_id=x.proposal_id
           AND r.snapshot_id=x.snapshot_id AND r.reviewer_id=x.reviewer_id AND r.domain=x.domain AND r.id=x.review_id
         LEFT JOIN users u ON u.id=x.reviewer_id
         LEFT JOIN memberships m ON m.organization_id=x.organization_id AND m.user_id=x.reviewer_id
         WHERE h.organization_id=$1 AND h.proposal_id=$2 AND h.id=$3 AND h.state='READY'
         ORDER BY x.reviewer_id,x.domain LIMIT 201",
    )
    .bind(organization)
    .bind(proposal)
    .bind(snapshot)
    .fetch_all(conn)
    .await?;
    if rows.len() > 200 {
        return Err(sqlx::Error::Protocol("too many effective reviews".into()));
    }
    rows.into_iter().map(Row::decode).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(decision: &str, individual_coverage: Option<bool>) -> Row {
        Row {
            id: Uuid::new_v4(),
            reviewer: Uuid::new_v4(),
            domain: "PRODUCT".into(),
            decision: Some(decision.into()),
            context_matches: true,
            eligible: true,
            individual_coverage,
        }
    }
    fn valid(e: &EffectiveDecision) -> bool {
        e.validates_entry(e.id, e.reviewer, e.domain, e.decision)
    }

    #[test]
    fn regular_decisions_need_complete_evidence_and_explicit_all_remains_separate() {
        for state in [None, Some(false), Some(true)] {
            let regular = row("APPROVE", state).decode().unwrap();
            assert_eq!(valid(&regular), state == Some(true));
            let all = row("APPROVE_ALL", state).decode().unwrap();
            assert!(valid(&all));
            assert_eq!(all.decision, ReviewDecision::ApproveAll);
            let changes = row("REQUEST_CHANGES", state).decode().unwrap();
            assert!(valid(&changes));
        }
    }

    #[test]
    fn lost_eligibility_and_different_entry_identity_cannot_reuse_a_decision() {
        let mut r = row("APPROVE_ALL", None);
        r.eligible = false;
        assert!(!valid(&r.decode().unwrap()));
        let e = row("APPROVE", Some(true)).decode().unwrap();
        assert!(!e.validates_entry(Uuid::new_v4(), e.reviewer, e.domain, e.decision));
        assert!(!e.validates_entry(e.id, Uuid::new_v4(), e.domain, e.decision));
        assert!(!e.validates_entry(e.id, e.reviewer, ReviewArea::Design, e.decision));
        assert!(!e.validates_entry(e.id, e.reviewer, e.domain, ReviewDecision::ApproveAll));
    }

    #[test]
    fn missing_mismatched_or_comment_pointers_are_unavailable() {
        for variant in 0..7 {
            let mut r = row("APPROVE_ALL", None);
            match variant {
                0 => r.context_matches = false,
                1 => r.id = Uuid::nil(),
                2 => r.reviewer = Uuid::nil(),
                3 => r.domain = "UNKNOWN".into(),
                4 => r.decision = None,
                5 => r.decision = Some("COMMENT".into()),
                _ => r.decision = Some("UNKNOWN".into()),
            }
            assert!(r.decode().is_err(), "variant {variant}");
        }
    }
}
