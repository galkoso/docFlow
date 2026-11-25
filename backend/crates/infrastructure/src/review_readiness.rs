//! Stored review-policy inputs and an explicit fresh provider observation.
//! Neither report publishes a check, changes proposal state, or authorizes merge.
use specspace_domain::review_check::Verification;
use sqlx::{FromRow, PgConnection};
use uuid::Uuid;
pub(crate) mod decisions;
mod fresh;
mod report;
pub use fresh::FreshReadinessError;
pub use report::{PgReviewReadiness, ReadinessError};

#[derive(FromRow)]
pub(crate) struct BlockerEvidence {
    total_count: i64,
    state_count: i64,
    unresolved_blockers: i64,
}
impl BlockerEvidence {
    pub fn verification(&self) -> Verification {
        if self.total_count < 0
            || self.state_count != self.total_count
            || !(0..=self.state_count).contains(&self.unresolved_blockers)
        {
            Verification::Unknown
        } else if self.unresolved_blockers > 0 {
            Verification::Pending
        } else {
            Verification::Passed
        }
    }

    pub fn verified_unresolved(&self) -> Result<i64, sqlx::Error> {
        if self.verification() == Verification::Unknown {
            return Err(sqlx::Error::Protocol(
                "incomplete blocker state evidence".into(),
            ));
        }
        Ok(self.unresolved_blockers)
    }
}

/// Call within the caller's authorized, consistent transaction. Include every
/// thread on the proposal, even when its original snapshot or creator is no
/// longer current. Only the latest revision determines blocking/resolution;
/// comments, tombstones and loss of membership do not resolve a blocker.
/// A missing proposal is not evidence for an empty set of threads.
pub(crate) async fn discussions(
    conn: &mut PgConnection,
    organization: Uuid,
    proposal: Uuid,
) -> Result<BlockerEvidence, sqlx::Error> {
    sqlx::query_as::<_, BlockerEvidence>(
        "SELECT e.total_count,e.state_count,e.unresolved_blockers
         FROM proposals p CROSS JOIN LATERAL (
           SELECT count(*) AS total_count,count(r.version) AS state_count,
             count(*) FILTER (WHERE r.blocking AND NOT r.resolved) AS unresolved_blockers
           FROM discussion_threads t LEFT JOIN LATERAL (
             SELECT version,blocking,resolved FROM discussion_revisions
             WHERE organization_id=t.organization_id AND proposal_id=t.proposal_id
               AND thread_id=t.id ORDER BY version DESC LIMIT 1
           ) r ON true
           WHERE t.organization_id=p.organization_id AND t.proposal_id=p.id
         ) e WHERE p.organization_id=$1 AND p.id=$2",
    )
    .bind(organization)
    .bind(proposal)
    .fetch_one(conn)
    .await
}

/// Product questions are proposal-level blockers; old origin SHAs and departed
/// creators do not resolve them. Missing state remains unknown.
pub(crate) async fn questions(
    conn: &mut PgConnection,
    organization: Uuid,
    proposal: Uuid,
) -> Result<BlockerEvidence, sqlx::Error> {
    sqlx::query_as::<_, BlockerEvidence>(
        "SELECT e.total_count,e.state_count,e.unresolved_blockers FROM proposals p CROSS JOIN LATERAL (
           SELECT count(*) AS total_count,count(r.version) AS state_count,
             count(*) FILTER (WHERE r.blocking AND r.state<>'DECIDED') AS unresolved_blockers
           FROM product_questions q LEFT JOIN LATERAL (
             SELECT version,blocking,state FROM product_question_revisions
             WHERE organization_id=q.organization_id AND proposal_id=q.proposal_id AND question_id=q.id ORDER BY version DESC LIMIT 1
           ) r ON true WHERE q.organization_id=p.organization_id AND q.proposal_id=p.id
         ) e WHERE p.organization_id=$1 AND p.id=$2"
    ).bind(organization).bind(proposal).fetch_one(conn).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_empty_or_resolved_discussions_pass_and_open_blockers_remain_pending() {
        for (threads, blockers, expected) in [
            (0, 0, Verification::Passed),
            (30, 0, Verification::Passed),
            (30, 1, Verification::Pending),
            (30, 30, Verification::Pending),
        ] {
            let evidence = BlockerEvidence {
                total_count: threads,
                state_count: threads,
                unresolved_blockers: blockers,
            };
            assert_eq!(evidence.verification(), expected);
            assert_eq!(evidence.verified_unresolved().unwrap(), blockers);
        }
    }

    #[test]
    fn incomplete_or_impossible_counts_never_prove_no_blockers() {
        for (threads, states, blockers) in [
            (1, 0, 0),
            (30, 29, 0),
            (1, 2, 0),
            (-1, -1, 0),
            (1, 1, -1),
            (1, 1, 2),
        ] {
            let evidence = BlockerEvidence {
                total_count: threads,
                state_count: states,
                unresolved_blockers: blockers,
            };
            assert_eq!(evidence.verification(), Verification::Unknown);
            assert!(evidence.verified_unresolved().is_err());
        }
    }
}
