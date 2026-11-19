//! Qualified, quota-charged source reads and immutable explicit acknowledgements.
//! No database transaction spans source IO; final writes recheck locked authority.
use crate::{
    comparison_authorization,
    official_reviews::{self, OfficialReviewError, Scope},
    provider_backoff,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use specspace_application::{
    comparison_file::{
        FileReadError,
        discussion::AnchorSide,
        review_unit::{ReviewUnitReader, UnitReadError, UnitWindowRequest},
    },
    review_visits::*,
};
use sqlx::{PgPool, Postgres, Transaction};
use std::sync::Arc;
use uuid::Uuid;
type Timestamp = sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>;
#[derive(Debug, PartialEq, Eq)]
pub enum VisitError {
    Authority(OfficialReviewError),
    Source(UnitReadError),
    Invalid,
    Expired,
    Conflict,
    Quota,
    Backoff(u32),
    Unavailable,
}
impl From<sqlx::Error> for VisitError {
    fn from(_: sqlx::Error) -> Self {
        Self::Unavailable
    }
}
impl From<OfficialReviewError> for VisitError {
    fn from(e: OfficialReviewError) -> Self {
        Self::Authority(e)
    }
}
fn digest(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn side(s: AnchorSide) -> &'static str {
    match s {
        AnchorSide::Before => "before",
        AnchorSide::After => "after",
    }
}
fn current(s: &Scope, fp: &str) -> Result<(), VisitError> {
    if !s.current || s.fingerprint != fp {
        Err(OfficialReviewError::Stale.into())
    } else {
        Ok(())
    }
}
fn key(s: &Scope) -> provider_backoff::ConnectionKey<'_> {
    provider_backoff::ConnectionKey {
        organization: s.organization_id,
        repository: s.repository_id,
        remote_id: &s.remote_id,
        installation_id: &s.installation_id,
    }
}
pub struct PgReviewVisits {
    pool: PgPool,
    reader: Arc<ReviewUnitReader>,
}
impl PgReviewVisits {
    pub fn new(pool: PgPool, reader: Arc<ReviewUnitReader>) -> Self {
        Self { pool, reader }
    }
    async fn transaction(&self) -> Result<Transaction<'_, Postgres>, VisitError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SET LOCAL lock_timeout='5s'")
            .execute(&mut *tx)
            .await?;
        sqlx::query("SET LOCAL statement_timeout='10s'")
            .execute(&mut *tx)
            .await?;
        Ok(tx)
    }
    pub async fn deliver(
        &self,
        actor: Uuid,
        proposal: Uuid,
        snapshot: Uuid,
        request: ReviewSourceRequest,
    ) -> Result<ReviewSourceWindow, VisitError> {
        if [actor, proposal, snapshot].iter().any(Uuid::is_nil)
            || !digest(&request.expected_fingerprint)
            || !digest(&request.unit_id)
            || request.offset > 2097152
        {
            return Err(VisitError::Invalid);
        }
        let mut tx = self.transaction().await?;
        let before =
            official_reviews::reviewer_scope(&mut tx, actor, proposal, snapshot, request.domain)
                .await?;
        current(&before, &request.expected_fingerprint)?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('review-source-quota:'||$1::uuid::text,0))").bind(before.organization_id).execute(&mut *tx).await?;
        if let Some(seconds) = provider_backoff::remaining(&mut tx, &key(&before)).await? {
            return Err(VisitError::Backoff(seconds));
        }
        let (actor_count,org_count):(i64,i64)=sqlx::query_as("SELECT count(*) FILTER(WHERE reviewer_id=$2),count(*) FROM review_source_attempts WHERE organization_id=$1 AND created_at>clock_timestamp()-interval '10 minutes'").bind(before.organization_id).bind(actor).fetch_one(&mut *tx).await?;
        if actor_count >= 600 || org_count >= 6000 {
            return Err(VisitError::Quota);
        }
        let valid:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM review_snapshot_units WHERE organization_id=$1 AND proposal_id=$2 AND snapshot_id=$3 AND unit_id=$4 AND unit->$5<>'null'::jsonb AND unit->$5->'range'<>'null'::jsonb AND $6<=(unit#>>ARRAY[$5,'range','end_byte'])::integer-(unit#>>ARRAY[$5,'range','start_byte'])::integer)")
   .bind(before.organization_id).bind(proposal).bind(snapshot).bind(&request.unit_id).bind(side(request.side)).bind(request.offset as i32).fetch_one(&mut *tx).await?;
        if !valid {
            return Err(VisitError::Invalid);
        }
        let attempt = Uuid::new_v4();
        sqlx::query("INSERT INTO review_source_attempts(id,organization_id,proposal_id,snapshot_id,reviewer_id,domain,fingerprint,unit_id,side,start_offset) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)")
   .bind(attempt).bind(before.organization_id).bind(proposal).bind(snapshot).bind(actor).bind(request.domain.as_str()).bind(&request.expected_fingerprint).bind(&request.unit_id).bind(side(request.side)).bind(request.offset as i32).execute(&mut *tx).await?;
        tx.commit().await?;
        let proof = match self
            .reader
            .load(
                actor,
                proposal,
                snapshot,
                UnitWindowRequest {
                    unit: &request.unit_id,
                    side: request.side,
                    offset: request.offset,
                },
            )
            .await
        {
            Ok(proof) => proof,
            Err(error) => {
                use specspace_application::{
                    book::BookError, comparison_reader::ComparisonReadError,
                };
                use specspace_git_adapter::GitError;
                let delay = match &error {
                    UnitReadError::Source(
                        FileReadError::Provider(GitError::RateLimited {
                            retry_after_seconds,
                        })
                        | FileReadError::Access(ComparisonReadError::Provider(
                            GitError::RateLimited {
                                retry_after_seconds,
                            },
                        ))
                        | FileReadError::Access(ComparisonReadError::Source(BookError::Provider(
                            GitError::RateLimited {
                                retry_after_seconds,
                            },
                        ))),
                    ) => Some(retry_after_seconds.unwrap_or(60).clamp(1, u32::MAX as u64) as u32),
                    _ => None,
                };
                if let Some(delay) = delay {
                    let mut tx = self.transaction().await?;
                    provider_backoff::defer(&mut tx, &key(&before), delay).await?;
                    tx.commit().await?;
                    return Err(VisitError::Backoff(delay));
                }
                return Err(VisitError::Source(error));
            }
        };
        self.reader
            .recheck(actor, &proof)
            .await
            .map_err(|e| VisitError::Source(e.into()))?;
        let mut tx = self.transaction().await?;
        let after =
            official_reviews::reviewer_scope(&mut tx, actor, proposal, snapshot, request.domain)
                .await?;
        current(&after, &request.expected_fingerprint)?;
        let scope = comparison_authorization::context_in(&mut tx, actor, proposal, true)
            .await
            .map_err(|e| VisitError::Source(FileReadError::Access(e).into()))?;
        if proof.context().scope != scope
            || proof.context().fingerprint != after.fingerprint
            || before.organization_id != after.organization_id
            || before.repository_id != after.repository_id
            || before.remote_id != after.remote_id
            || before.installation_id != after.installation_id
        {
            return Err(OfficialReviewError::Stale.into());
        }
        let id = Uuid::new_v4();
        let source = serde_json::to_value(proof.source()).map_err(|_| VisitError::Unavailable)?;
        let window_sha256 = format!("{:x}", Sha256::digest(proof.text().as_bytes()));
        let expires:Timestamp=sqlx::query_scalar("INSERT INTO review_source_deliveries(id,attempt_id,source,start_byte,end_byte,window_sha256) VALUES($1,$2,$3,$4,$5,$6) RETURNING expires_at")
   .bind(id).bind(attempt).bind(source).bind(proof.start_byte() as i32).bind(proof.end_byte() as i32).bind(&window_sha256).fetch_one(&mut *tx).await?;
        tx.commit().await?;
        Ok(ReviewSourceWindow {
            actor_id: actor,
            proposal_id: proposal,
            snapshot_id: snapshot,
            delivery_id: id,
            fingerprint: after.fingerprint,
            domain: request.domain,
            unit_id: request.unit_id,
            side: request.side,
            source: proof.source().clone(),
            head_sha: proof.context().head.as_str().into(),
            merge_base_sha: proof.context().merge_base.as_str().into(),
            start_byte: proof.start_byte(),
            end_byte: proof.end_byte(),
            offset: request.offset,
            next_offset: proof.next_offset(),
            text: proof.text().into(),
            window_sha256,
            expires_at: expires.to_rfc3339(),
        })
    }
    pub async fn acknowledge(
        &self,
        actor: Uuid,
        proposal: Uuid,
        snapshot: Uuid,
        request: ReviewVisitRequest,
    ) -> Result<ReviewVisitRecorded, VisitError> {
        if [
            actor,
            proposal,
            snapshot,
            request.request_id,
            request.delivery_id,
        ]
        .iter()
        .any(Uuid::is_nil)
            || !digest(&request.expected_fingerprint)
        {
            return Err(VisitError::Invalid);
        }
        let hash = format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&json!([
                    "review-source-visit-v1",
                    actor,
                    proposal,
                    snapshot,
                    request.delivery_id,
                    request.domain,
                    request.expected_fingerprint
                ]))
                .map_err(|_| VisitError::Unavailable)?
            )
        );
        let mut tx = self.transaction().await?;
        let s =
            official_reviews::reviewer_scope(&mut tx, actor, proposal, snapshot, request.domain)
                .await?;
        current(&s, &request.expected_fingerprint)?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('review-visit-request:'||$1::uuid::text||':'||$2::uuid::text,0))")
            .bind(s.organization_id).bind(request.request_id).execute(&mut *tx).await?;
        let old:Option<(Uuid,Uuid,String,Timestamp)>=sqlx::query_as("SELECT v.id,v.delivery_id,v.request_hash,v.created_at FROM review_source_visits v WHERE organization_id=$1 AND request_id=$2")
   .bind(s.organization_id).bind(request.request_id).fetch_optional(&mut *tx).await?;
        if old
            .as_ref()
            .is_some_and(|(_, delivery, h, _)| *delivery != request.delivery_id || h != &hash)
        {
            return Err(VisitError::Conflict);
        }
        let row:Option<Delivery>=sqlx::query_as("SELECT a.unit_id,a.side,d.start_byte,d.end_byte,d.expires_at>clock_timestamp() AS unexpired FROM review_source_deliveries d JOIN review_source_attempts a ON a.id=d.attempt_id WHERE d.id=$1 AND a.organization_id=$2 AND a.proposal_id=$3 AND a.snapshot_id=$4 AND a.reviewer_id=$5 AND a.domain=$6 AND a.fingerprint=$7 FOR SHARE OF a,d")
   .bind(request.delivery_id).bind(s.organization_id).bind(proposal).bind(snapshot).bind(actor).bind(request.domain.as_str()).bind(&request.expected_fingerprint).fetch_optional(&mut *tx).await?;
        let delivery = row.ok_or(VisitError::Conflict)?;
        if let Some((id, _, _, created)) = old {
            tx.commit().await?;
            return Ok(receipt(
                (actor, proposal, snapshot),
                &request,
                delivery,
                id,
                created,
                true,
            ));
        }
        if !delivery.unexpired {
            return Err(VisitError::Expired);
        }
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM review_source_visits WHERE delivery_id=$1)",
        )
        .bind(request.delivery_id)
        .fetch_one(&mut *tx)
        .await?;
        if exists {
            return Err(VisitError::Conflict);
        }
        let id = Uuid::new_v4();
        let audit = Uuid::new_v4();
        let event = Uuid::new_v4();
        sqlx::query("INSERT INTO audit_events(id,organization_id,actor_id,entity_id,action,after_ref,result) VALUES($1,$2,$3,$4,'REVIEW_SOURCE_ACKNOWLEDGED',$5,'RECORDED')").bind(audit).bind(s.organization_id).bind(actor).bind(proposal).bind(id.to_string()).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO outbox(id,organization_id,entity_id,event_type,payload) VALUES($1,$2,$3,'REVIEW_SOURCE_ACKNOWLEDGED',$4)").bind(event).bind(s.organization_id).bind(proposal).bind(json!({"visit_id":id,"delivery_id":request.delivery_id,"snapshot_id":snapshot,"unit_id":delivery.unit_id,"domain":request.domain,"side":delivery.side})).execute(&mut *tx).await?;
        let created:Timestamp=sqlx::query_scalar("INSERT INTO review_source_visits(id,organization_id,delivery_id,request_id,request_hash,audit_id,event_id) VALUES($1,$2,$3,$4,$5,$6,$7) RETURNING created_at")
   .bind(id).bind(s.organization_id).bind(request.delivery_id).bind(request.request_id).bind(hash).bind(audit).bind(event).fetch_one(&mut *tx).await?;
        tx.commit().await?;
        Ok(receipt(
            (actor, proposal, snapshot),
            &request,
            delivery,
            id,
            created,
            false,
        ))
    }
}
#[derive(sqlx::FromRow)]
struct Delivery {
    unit_id: String,
    side: String,
    start_byte: i32,
    end_byte: i32,
    unexpired: bool,
}
fn receipt(
    scope: (Uuid, Uuid, Uuid),
    r: &ReviewVisitRequest,
    d: Delivery,
    id: Uuid,
    created: Timestamp,
    replayed: bool,
) -> ReviewVisitRecorded {
    let (actor, proposal, snapshot) = scope;
    ReviewVisitRecorded {
        id,
        request_id: r.request_id,
        delivery_id: r.delivery_id,
        actor_id: actor,
        proposal_id: proposal,
        snapshot_id: snapshot,
        fingerprint: r.expected_fingerprint.clone(),
        domain: r.domain,
        unit_id: d.unit_id,
        side: if d.side == "before" {
            AnchorSide::Before
        } else {
            AnchorSide::After
        },
        start_byte: d.start_byte as usize,
        end_byte: d.end_byte as usize,
        recorded_at: created.to_rfc3339(),
        replayed,
    }
}
