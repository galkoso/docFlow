//! Read acknowledged mandatory ranges within the caller's existing transaction.
//! Callers establish eligibility and capture the immutable scope before reading.
use serde_json::Value;
use specspace_application::{review_configuration::ReviewArea, review_visits::ReviewUnitCoverage};
use sqlx::PgConnection;
use uuid::Uuid;

// Union acknowledged byte ranges, never delivery counts. Empty spans require an
// explicit acknowledgement; an empty range is not evidence of reading a nonempty
// span. Exact h/actor/domain/fingerprint and source JSON prevent cross-context reuse.
const COVERAGE: &str = r#"
WITH units AS (SELECT * FROM review_unit_coverage($1,$2,$3,$4,$5,$6))
SELECT count(*)::integer,count(*) FILTER(WHERE viewed)::integer,count(*) FILTER(WHERE unsupported)::integer,
 (SELECT coalesce(jsonb_agg(jsonb_build_object('unit_id',p.unit_id,'before',p.before_state,'after',p.after_state,'viewed',p.viewed) ORDER BY p.ordinal),'[]'::jsonb)
  FROM (SELECT * FROM units WHERE ordinal>=$7 ORDER BY ordinal LIMIT $8) p)
FROM units
"#;

pub(crate) struct CoverageScope<'a> {
    pub organization: Uuid,
    pub proposal: Uuid,
    pub snapshot: Uuid,
    pub actor: Uuid,
    pub domain: ReviewArea,
    pub fingerprint: &'a str,
}
pub(crate) struct CoveragePage {
    pub available: bool,
    pub total: i32,
    pub viewed: i32,
    pub unsupported: i32,
    pub units: Vec<ReviewUnitCoverage>,
}
impl CoveragePage {
    pub fn has_unviewed(&self) -> bool {
        !self.available || self.total < 1 || self.viewed < self.total
    }
}
#[derive(Debug)]
pub(crate) struct CoverageUnavailable;
impl From<sqlx::Error> for CoverageUnavailable {
    fn from(_: sqlx::Error) -> Self {
        Self
    }
}
pub(crate) async fn read(
    conn: &mut PgConnection,
    scope: CoverageScope<'_>,
    offset: i32,
    limit: i32,
) -> Result<CoveragePage, CoverageUnavailable> {
    let (version, expected): (i32, i32) = sqlx::query_as("SELECT review_units_version,unit_count FROM review_snapshots WHERE organization_id=$1 AND proposal_id=$2 AND id=$3")
        .bind(scope.organization).bind(scope.proposal).bind(scope.snapshot).fetch_one(&mut *conn).await?;
    let available = match (version, expected) {
        (0, 0) => false,
        (1, 1..=40000) => true,
        _ => return Err(CoverageUnavailable),
    };
    let (total, viewed, unsupported, values): (i32, i32, i32, Value) = sqlx::query_as(COVERAGE)
        .bind(scope.organization)
        .bind(scope.proposal)
        .bind(scope.snapshot)
        .bind(scope.actor)
        .bind(scope.domain.as_str())
        .bind(scope.fingerprint)
        .bind(offset)
        .bind(limit)
        .fetch_one(&mut *conn)
        .await?;
    let units: Vec<ReviewUnitCoverage> =
        serde_json::from_value(values).map_err(|_| CoverageUnavailable)?;
    if total != expected || units.len() != (total - offset).clamp(0, limit) as usize {
        return Err(CoverageUnavailable);
    }
    Ok(CoveragePage {
        available,
        total,
        viewed,
        unsupported,
        units,
    })
}
