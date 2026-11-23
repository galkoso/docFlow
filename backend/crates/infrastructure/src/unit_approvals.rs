//! Individual approvals require locked current eligibility and acknowledged
//! source coverage. No provider IO, official decision or proposal-state change.
use crate::official_reviews::{self, OfficialReviewError};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use specspace_application::{review_configuration::ReviewArea, unit_approval::*};
use sqlx::{FromRow, PgConnection, PgPool, Postgres, Transaction};
use uuid::Uuid;
type Timestamp = sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>;

#[derive(Debug, PartialEq, Eq)]
pub enum UnitApprovalError {
    Authority(OfficialReviewError),
    Invalid,
    NotFound,
    IncompleteCoverage,
    EvidenceUnavailable,
    Conflict,
    Quota,
    Unavailable,
}
impl From<sqlx::Error> for UnitApprovalError {
    fn from(_: sqlx::Error) -> Self {
        Self::Unavailable
    }
}
impl From<OfficialReviewError> for UnitApprovalError {
    fn from(e: OfficialReviewError) -> Self {
        Self::Authority(e)
    }
}
#[derive(FromRow)]
struct Header {
    head_sha: String,
    target_sha: String,
    policy_version: i64,
    configuration_version: i64,
    review_units_version: i32,
    unit_count: i32,
}
impl Header {
    fn available(&self) -> Result<bool, UnitApprovalError> {
        match (self.review_units_version, self.unit_count) {
            (0, 0) => Ok(false),
            (1, 1..=40000) => Ok(true),
            _ => Err(UnitApprovalError::EvidenceUnavailable),
        }
    }
}
async fn header(
    conn: &mut PgConnection,
    organization: Uuid,
    proposal: Uuid,
    snapshot: Uuid,
) -> Result<Header, UnitApprovalError> {
    Ok(sqlx::query_as("SELECT head_sha,target_sha,policy_version,configuration_version,review_units_version,unit_count FROM review_snapshots WHERE organization_id=$1 AND proposal_id=$2 AND id=$3")
        .bind(organization).bind(proposal).bind(snapshot).fetch_one(conn).await?)
}
#[derive(FromRow)]
struct Recorded {
    id: Uuid,
    request_id: Uuid,
    actor_id: Uuid,
    proposal_id: Uuid,
    snapshot_id: Uuid,
    fingerprint: String,
    domain: String,
    unit_id: String,
    request_hash: String,
    head_sha: String,
    target_sha: String,
    policy_version: i64,
    configuration_version: i64,
    created_at: Timestamp,
}
const RECORDED: &str = "SELECT a.id,a.request_id,a.reviewer_id AS actor_id,a.proposal_id,a.snapshot_id,a.fingerprint,a.domain,a.unit_id,a.request_hash,h.head_sha,h.target_sha,h.policy_version,h.configuration_version,a.created_at FROM review_unit_approvals a JOIN review_snapshots h ON h.id=a.snapshot_id WHERE a.organization_id=$1 AND a.request_id=$2";
impl Recorded {
    fn dto(self, replayed: bool) -> Result<UnitApprovalRecorded, UnitApprovalError> {
        let domain = match self.domain.as_str() {
            "PRODUCT" => ReviewArea::Product,
            "DEVELOPMENT" => ReviewArea::Development,
            "DESIGN" => ReviewArea::Design,
            _ => return Err(UnitApprovalError::Unavailable),
        };
        Ok(UnitApprovalRecorded {
            id: self.id,
            request_id: self.request_id,
            actor_id: self.actor_id,
            proposal_id: self.proposal_id,
            snapshot_id: self.snapshot_id,
            fingerprint: self.fingerprint,
            domain,
            unit_id: self.unit_id,
            head_sha: self.head_sha,
            target_sha: self.target_sha,
            policy_version: self.policy_version,
            configuration_version: self.configuration_version,
            recorded_at: self.created_at.to_rfc3339(),
            replayed,
        })
    }
}
pub struct PgUnitApprovals {
    pool: PgPool,
}
impl PgUnitApprovals {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
    async fn transaction(&self) -> Result<Transaction<'_, Postgres>, UnitApprovalError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SET LOCAL lock_timeout='5s'")
            .execute(&mut *tx)
            .await?;
        sqlx::query("SET LOCAL statement_timeout='10s'")
            .execute(&mut *tx)
            .await?;
        Ok(tx)
    }
    pub async fn approve(
        &self,
        actor: Uuid,
        proposal: Uuid,
        snapshot: Uuid,
        request: UnitApprovalRequest,
    ) -> Result<UnitApprovalRecorded, UnitApprovalError> {
        request.validate().map_err(|_| UnitApprovalError::Invalid)?;
        if [actor, proposal, snapshot].iter().any(Uuid::is_nil) {
            return Err(UnitApprovalError::Invalid);
        }
        let hash = format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&json!([
                    "review-unit-approval-v1",
                    actor,
                    proposal,
                    snapshot,
                    request.expected_fingerprint,
                    request.domain,
                    request.unit_id
                ]))
                .map_err(|_| UnitApprovalError::Unavailable)?
            )
        );
        let mut tx = self.transaction().await?;
        let scope =
            official_reviews::reviewer_scope(&mut tx, actor, proposal, snapshot, request.domain)
                .await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('review-unit-approval-org:'||$1::uuid::text,0))")
            .bind(scope.organization_id).execute(&mut *tx).await?;
        if let Some(old) = sqlx::query_as::<_, Recorded>(RECORDED)
            .bind(scope.organization_id)
            .bind(request.request_id)
            .fetch_optional(&mut *tx)
            .await?
        {
            if old.request_hash != hash
                || old.actor_id != actor
                || old.proposal_id != proposal
                || old.snapshot_id != snapshot
                || old.unit_id != request.unit_id
                || old.domain != request.domain.as_str()
                || old.fingerprint != request.expected_fingerprint
            {
                return Err(UnitApprovalError::Conflict);
            }
            let receipt = old.dto(true)?;
            tx.commit().await?;
            return Ok(receipt);
        }
        if !scope.current || scope.fingerprint != request.expected_fingerprint {
            return Err(OfficialReviewError::Stale.into());
        }
        if !header(&mut tx, scope.organization_id, proposal, snapshot)
            .await?
            .available()?
        {
            return Err(UnitApprovalError::EvidenceUnavailable);
        }
        let viewed: Option<bool> =
            sqlx::query_scalar("SELECT viewed FROM review_unit_coverage($1,$2,$3,$4,$5,$6,$7)")
                .bind(scope.organization_id)
                .bind(proposal)
                .bind(snapshot)
                .bind(actor)
                .bind(request.domain.as_str())
                .bind(&scope.fingerprint)
                .bind(&request.unit_id)
                .fetch_optional(&mut *tx)
                .await?;
        if !viewed.ok_or(UnitApprovalError::NotFound)? {
            return Err(UnitApprovalError::IncompleteCoverage);
        }
        let allowed: bool = sqlx::query_scalar("SELECT count(*)<6000 AND count(*) FILTER(WHERE reviewer_id=$2)<600 FROM review_unit_approvals WHERE organization_id=$1 AND created_at>clock_timestamp()-interval '10 minutes'")
            .bind(scope.organization_id).bind(actor).fetch_one(&mut *tx).await?;
        if !allowed {
            return Err(UnitApprovalError::Quota);
        }
        let id = Uuid::new_v4();
        let audit = Uuid::new_v4();
        let event = Uuid::new_v4();
        sqlx::query("INSERT INTO audit_events(id,organization_id,actor_id,entity_id,action,after_ref,result) VALUES($1,$2,$3,$4,'REVIEW_UNIT_APPROVED',$5,'RECORDED')")
            .bind(audit).bind(scope.organization_id).bind(actor).bind(proposal).bind(id.to_string()).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO outbox(id,organization_id,entity_id,event_type,payload) VALUES($1,$2,$3,'REVIEW_UNIT_APPROVED',$4)")
            .bind(event).bind(scope.organization_id).bind(proposal)
            .bind(json!({"approval_id":id,"snapshot_id":snapshot,"unit_id":request.unit_id,"domain":request.domain})).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO review_unit_approvals(id,organization_id,proposal_id,snapshot_id,reviewer_id,domain,fingerprint,unit_id,request_id,request_hash,audit_id,event_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)")
            .bind(id).bind(scope.organization_id).bind(proposal).bind(snapshot).bind(actor).bind(request.domain.as_str())
            .bind(&scope.fingerprint).bind(&request.unit_id).bind(request.request_id).bind(hash).bind(audit).bind(event).execute(&mut *tx).await?;
        let receipt = sqlx::query_as::<_, Recorded>(RECORDED)
            .bind(scope.organization_id)
            .bind(request.request_id)
            .fetch_one(&mut *tx)
            .await?
            .dto(false)?;
        tx.commit().await?;
        Ok(receipt)
    }
    pub async fn page(
        &self,
        actor: Uuid,
        proposal: Uuid,
        snapshot: Uuid,
        request: UnitApprovalPageRequest,
    ) -> Result<UnitApprovalPage, UnitApprovalError> {
        if request.validate().is_err() || [actor, proposal, snapshot].iter().any(Uuid::is_nil) {
            return Err(UnitApprovalError::Invalid);
        }
        let mut tx = self.transaction().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
            .execute(&mut *tx)
            .await?;
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
        let h = header(&mut tx, scope.organization_id, proposal, snapshot).await?;
        let available = h.available()?;
        let (total, approved, units): (i32, i32, Value) = sqlx::query_as(PAGE)
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
        let units: Vec<UnitApprovalStatus> =
            serde_json::from_value(units).map_err(|_| UnitApprovalError::Unavailable)?;
        if total != h.unit_count
            || units.len() != (total - request.offset).clamp(0, request.limit) as usize
        {
            return Err(UnitApprovalError::EvidenceUnavailable);
        }
        let end = request.offset + units.len() as i32;
        tx.commit().await?;
        Ok(UnitApprovalPage {
            actor_id: actor,
            proposal_id: proposal,
            snapshot_id: snapshot,
            fingerprint: scope.fingerprint,
            head_sha: h.head_sha,
            target_sha: h.target_sha,
            policy_version: h.policy_version,
            configuration_version: h.configuration_version,
            domain: request.domain,
            current_for_metadata: scope.current,
            available,
            total_units: total,
            approved_units: approved,
            unapproved_units: total - approved,
            offset: request.offset,
            next_offset: (end < total).then_some(end),
            units,
        })
    }
}
const PAGE: &str = r#"
WITH latest AS (
 SELECT DISTINCT ON (unit_id) unit_id,id,created_at FROM review_unit_approvals
 WHERE organization_id=$1 AND proposal_id=$2 AND snapshot_id=$3 AND reviewer_id=$4 AND domain=$5 AND fingerprint=$6
 ORDER BY unit_id,created_at DESC,id DESC
), items AS (
 SELECT u.ordinal,u.unit_id,a.id,a.created_at FROM review_snapshot_units u LEFT JOIN latest a USING(unit_id)
 WHERE u.organization_id=$1 AND u.proposal_id=$2 AND u.snapshot_id=$3
)
SELECT count(*)::integer,count(*) FILTER(WHERE id IS NOT NULL)::integer,
 (SELECT coalesce(jsonb_agg(jsonb_build_object('unit_id',p.unit_id,'approval_id',p.id,'approved_at',p.created_at) ORDER BY p.ordinal),'[]'::jsonb)
 FROM (SELECT * FROM items WHERE ordinal>=$7 ORDER BY ordinal LIMIT $8) p) FROM items
"#;
