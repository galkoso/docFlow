//! Stored inputs for the required review-policy check. These reads do not prove
//! fresh Git refs, publish a check, change proposal state, or authorize merge.
use specspace_domain::review_check::Verification;
use sqlx::{FromRow, PgConnection};
use uuid::Uuid;
pub(crate) mod decisions;

#[derive(FromRow)]
pub(crate) struct DiscussionEvidence {
    thread_count: i64,
    state_count: i64,
    unresolved_blockers: i64,
}
impl DiscussionEvidence {
    pub fn verification(&self) -> Verification {
        if self.thread_count < 0
            || self.state_count != self.thread_count
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
                "incomplete discussion state evidence".into(),
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
) -> Result<DiscussionEvidence, sqlx::Error> {
    sqlx::query_as::<_, DiscussionEvidence>(
        "SELECT e.thread_count,e.state_count,e.unresolved_blockers
         FROM proposals p CROSS JOIN LATERAL (
           SELECT count(*) AS thread_count,count(r.version) AS state_count,
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
            let evidence = DiscussionEvidence {
                thread_count: threads,
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
            let evidence = DiscussionEvidence {
                thread_count: threads,
                state_count: states,
                unresolved_blockers: blockers,
            };
            assert_eq!(evidence.verification(), Verification::Unknown);
            assert!(evidence.verified_unresolved().is_err());
        }
    }
}
