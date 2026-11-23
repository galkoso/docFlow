//! Scope-bound counts for regular official approval and readiness assembly.
use crate::review_coverage::CoverageScope;
use sqlx::PgConnection;

pub(crate) struct ApprovalCoverage {
    pub available: bool,
    pub total: i32,
    pub approved: i32,
}
impl ApprovalCoverage {
    pub fn complete(&self) -> bool {
        self.available && self.total > 0 && self.approved == self.total
    }
}
pub(crate) async fn read(
    conn: &mut PgConnection,
    s: CoverageScope<'_>,
) -> Result<ApprovalCoverage, sqlx::Error> {
    let (available,total,approved): (bool,i32,i32) = sqlx::query_as("SELECT available,total_units,approved_units FROM review_unit_approval_coverage($1,$2,$3,$4,$5,$6)")
        .bind(s.organization).bind(s.proposal).bind(s.snapshot).bind(s.actor).bind(s.domain.as_str()).bind(s.fingerprint).fetch_one(conn).await?;
    if !(0..=40000).contains(&total) || !(0..=total).contains(&approved) {
        return Err(sqlx::Error::Protocol(
            "invalid individual approval counts".into(),
        ));
    }
    Ok(ApprovalCoverage {
        available,
        total,
        approved,
    })
}
