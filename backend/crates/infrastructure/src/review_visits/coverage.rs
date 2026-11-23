use super::*;
use serde_json::Value;

// Union acknowledged byte ranges, never delivery counts. Empty spans require an
// explicit acknowledgement; an empty range is not evidence of reading a nonempty
// span. Exact h/actor/domain/fingerprint and source JSON prevent cross-context reuse.
const COVERAGE: &str = r#"
WITH acknowledged AS (
 SELECT a.unit_id,a.side,range_agg(int8range(d.start_byte::bigint,d.end_byte::bigint,'[)')) AS covered,count(*) AS seen
 FROM review_source_attempts a JOIN review_source_deliveries d ON d.attempt_id=a.id
 JOIN review_source_visits v ON v.delivery_id=d.id AND v.organization_id=a.organization_id
 JOIN review_snapshot_units u ON u.snapshot_id=a.snapshot_id AND u.unit_id=a.unit_id AND u.organization_id=a.organization_id AND u.proposal_id=a.proposal_id
 WHERE a.organization_id=$1 AND a.proposal_id=$2 AND a.snapshot_id=$3 AND a.reviewer_id=$4 AND a.domain=$5 AND a.fingerprint=$6
   AND d.source=u.unit->a.side
 GROUP BY a.unit_id,a.side
), sides AS (
 SELECT u.ordinal,u.unit_id,s.side,
 CASE WHEN s.source IS NULL OR s.source='null'::jsonb THEN 'absent'
  WHEN s.source->'range'='null'::jsonb THEN 'unsupported'
  WHEN (s.source#>>'{range,start_byte}')::bigint=(s.source#>>'{range,end_byte}')::bigint AND coalesce(a.seen,0)>0 THEN 'viewed'
  WHEN (s.source#>>'{range,start_byte}')::bigint<(s.source#>>'{range,end_byte}')::bigint
    AND coalesce(a.covered @> int8range((s.source#>>'{range,start_byte}')::bigint,(s.source#>>'{range,end_byte}')::bigint,'[)'),false) THEN 'viewed'
  WHEN coalesce(a.covered && int8range((s.source#>>'{range,start_byte}')::bigint,(s.source#>>'{range,end_byte}')::bigint,'[)'),false) THEN 'partial'
  ELSE 'unviewed' END AS state
 FROM review_snapshot_units u CROSS JOIN LATERAL (VALUES ('before',u.unit->'before'),('after',u.unit->'after')) s(side,source)
 LEFT JOIN acknowledged a ON a.unit_id=u.unit_id AND a.side=s.side
 WHERE u.organization_id=$1 AND u.proposal_id=$2 AND u.snapshot_id=$3
), units AS (
 SELECT ordinal,unit_id,max(state) FILTER(WHERE side='before') AS before_state,max(state) FILTER(WHERE side='after') AS after_state,
   bool_and(state IN ('absent','viewed')) AND bool_or(state<>'absent') AS viewed,bool_or(state='unsupported') AS unsupported
 FROM sides GROUP BY ordinal,unit_id
)
SELECT count(*)::integer,count(*) FILTER(WHERE viewed)::integer,count(*) FILTER(WHERE unsupported)::integer,
 (SELECT coalesce(jsonb_agg(jsonb_build_object('unit_id',p.unit_id,'before',p.before_state,'after',p.after_state,'viewed',p.viewed) ORDER BY p.ordinal),'[]'::jsonb)
  FROM (SELECT * FROM units WHERE ordinal>=$7 ORDER BY ordinal LIMIT $8) p)
FROM units
"#;
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
        let (version,expected):(i32,i32)=sqlx::query_as("SELECT review_units_version,unit_count FROM review_snapshots WHERE organization_id=$1 AND proposal_id=$2 AND id=$3")
            .bind(scope.organization_id).bind(proposal).bind(snapshot).fetch_one(&mut *tx).await?;
        let available = match (version, expected) {
            (0, 0) => false,
            (1, 1..=40000) => true,
            _ => return Err(VisitError::Unavailable),
        };
        let (total, viewed, unsupported, values): (i32, i32, i32, Value) = sqlx::query_as(COVERAGE)
            .bind(scope.organization_id)
            .bind(proposal)
            .bind(snapshot)
            .bind(actor)
            .bind(request.domain.as_str())
            .bind(&scope.fingerprint)
            .bind(request.offset)
            .bind(request.limit)
            .fetch_one(&mut *tx)
            .await?;
        let units: Vec<ReviewUnitCoverage> =
            serde_json::from_value(values).map_err(|_| VisitError::Unavailable)?;
        if total != expected
            || units.len() != (total - request.offset).clamp(0, request.limit) as usize
        {
            return Err(VisitError::Unavailable);
        }
        let head_sha: String =
            sqlx::query_scalar("SELECT head_sha FROM review_snapshots WHERE id=$1")
                .bind(snapshot)
                .fetch_one(&mut *tx)
                .await?;
        let end = request.offset + units.len() as i32;
        tx.commit().await?;
        Ok(ReviewCoverage {
            actor_id: actor,
            proposal_id: proposal,
            snapshot_id: snapshot,
            fingerprint: scope.fingerprint,
            head_sha,
            domain: request.domain,
            current_for_metadata: scope.current,
            available,
            total_units: total,
            viewed_units: viewed,
            unviewed_units: total - viewed,
            unsupported_units: unsupported,
            offset: request.offset,
            next_offset: (end < total).then_some(end),
            units,
        })
    }
}
