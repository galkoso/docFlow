use super::*;
use crate::review_coverage::{self, CoverageScope};

impl PgReviewVisits {
    pub async fn coverage(
        &self,
        actor: Uuid,
        proposal: Uuid,
        snapshot: Uuid,
        request: ReviewCoverageRequest,
    ) -> Result<ReviewCoverage, VisitError> {
        if [actor, proposal, snapshot].iter().any(Uuid::is_nil)
            || !digest(&request.expected_fingerprint)
            || !(0..=40000).contains(&request.offset)
            || !(1..=100).contains(&request.limit)
        {
            return Err(VisitError::Invalid);
        }
        let mut tx = self.transaction().await?;
        let scope = official_reviews::reviewer_read_scope(
            &mut tx,
            actor,
            proposal,
            snapshot,
            request.domain,
        )
        .await?;
        if scope.fingerprint != request.expected_fingerprint {
            return Err(OfficialReviewError::Stale.into());
        }
        let page = review_coverage::read(
            &mut tx,
            CoverageScope {
                organization: scope.organization_id,
                proposal,
                snapshot,
                actor,
                domain: request.domain,
                fingerprint: &scope.fingerprint,
            },
            request.offset,
            request.limit,
        )
        .await
        .map_err(|_| VisitError::Unavailable)?;
        let head_sha: String =
            sqlx::query_scalar("SELECT head_sha FROM review_snapshots WHERE id=$1")
                .bind(snapshot)
                .fetch_one(&mut *tx)
                .await?;
        let end = request.offset + page.units.len() as i32;
        tx.commit().await?;
        Ok(ReviewCoverage {
            actor_id: actor,
            proposal_id: proposal,
            snapshot_id: snapshot,
            fingerprint: scope.fingerprint,
            head_sha,
            domain: request.domain,
            current_for_metadata: scope.current,
            available: page.available,
            total_units: page.total,
            viewed_units: page.viewed,
            unviewed_units: page.total - page.viewed,
            unsupported_units: page.unsupported,
            offset: request.offset,
            next_offset: (end < page.total).then_some(end),
            units: page.units,
        })
    }
}
