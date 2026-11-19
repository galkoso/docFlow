//! Immutable shared evidence, published together with the lease outcome. Git IO
//! finishes before the transaction; no source bodies or image bytes are stored.
use crate::{
    review_authorization,
    review_submissions::{self, PgReviewSubmissions, SubmissionError, SubmissionLease},
};
use serde::Serialize;
use serde_json::{Value, json};
use specspace_application::{
    comparison::Change,
    review_preparation::{ReviewPreparationError, ReviewPreparer, VerifiedReview},
    review_snapshot::{
        ReviewBookDocument, ReviewMediaReceipt, ReviewSnapshot, ReviewSnapshotDocuments,
        ReviewSnapshotHistory, ReviewSnapshotHistoryEntry, ReviewSnapshotMedia,
        ReviewSnapshotMetadata,
    },
    review_submission::ReviewPublication,
};
use sqlx::{FromRow, PgConnection, PgPool, Postgres, QueryBuilder, Transaction};
use std::{sync::Arc, time::Duration};
use uuid::Uuid;
type Timestamp = sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>;

#[derive(Debug)]
pub enum SnapshotError {
    Submission(SubmissionError),
    Preparation(ReviewPreparationError),
    InvalidEvidence,
    TimedOut,
}
impl From<SubmissionError> for SnapshotError {
    fn from(error: SubmissionError) -> Self {
        Self::Submission(error)
    }
}
impl From<sqlx::Error> for SnapshotError {
    fn from(_: sqlx::Error) -> Self {
        Self::Submission(SubmissionError::Unavailable)
    }
}
#[derive(Serialize, FromRow)]
pub struct SharedReviewSnapshot {
    pub id: Uuid,
    pub proposal_id: Uuid,
    pub fingerprint: String,
    pub evidence_digest: String,
    pub media_validation_version: i32,
    pub created_at: Timestamp,
    pub created_by: Uuid,
    pub title: String,
    pub proposal_number: i64,
    pub delivery_status: String,
    pub metadata: Value,
    pub file_count: i32,
    pub media_count: i32,
    /// Compares current database metadata only; this is not a fresh Git check.
    pub current_metadata: bool,
}
impl SharedReviewSnapshot {
    pub fn into_read_model(self) -> Result<ReviewSnapshot, SnapshotError> {
        let metadata: ReviewSnapshotMetadata =
            serde_json::from_value(self.metadata).map_err(|_| SnapshotError::InvalidEvidence)?;
        if metadata.schema_version != 1
            || metadata.summary.proposal_id != self.proposal_id
            || metadata.summary.changed_files != self.file_count as usize
            || metadata.summary.media_checks_required != self.media_count as usize
            || metadata.assignments.len() > 200
            || !metadata.validation.complete
            || metadata.validation.blockers_total != 0
            || metadata.validation.findings.len() > 1000
            || metadata.validation.findings_total < metadata.validation.findings.len()
            || metadata.summary.content_findings_total != metadata.validation.findings_total
        {
            return Err(SnapshotError::InvalidEvidence);
        }
        Ok(ReviewSnapshot {
            snapshot_id: self.id,
            proposal_id: self.proposal_id,
            fingerprint: self.fingerprint,
            evidence_digest: self.evidence_digest,
            media_validation_version: self.media_validation_version as u32,
            title: self.title,
            proposal_number: self.proposal_number,
            delivery_status: self.delivery_status,
            created_at: self.created_at.to_rfc3339(),
            created_by: self.created_by,
            metadata,
            file_count: self.file_count,
            media_count: self.media_count,
            current_metadata: self.current_metadata,
        })
    }
}
#[derive(Serialize)]
pub struct SharedReviewFiles {
    pub snapshot_id: Uuid,
    pub offset: i32,
    pub total: i32,
    pub files: Vec<Change>,
}
pub struct PgReviewSnapshots {
    pool: PgPool,
}
impl PgReviewSnapshots {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
    async fn transaction(&self) -> Result<Transaction<'_, Postgres>, SnapshotError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SET LOCAL lock_timeout='5s'")
            .execute(&mut *tx)
            .await?;
        sqlx::query("SET LOCAL statement_timeout='10s'")
            .execute(&mut *tx)
            .await?;
        Ok(tx)
    }
    pub async fn publish(
        &self,
        lease: &SubmissionLease,
        verified: &VerifiedReview,
        preparer: &ReviewPreparer,
    ) -> Result<ReviewPublication, SnapshotError> {
        if !lease.matches_scope(verified.candidate().scope()) {
            return Err(SnapshotError::InvalidEvidence);
        }
        if let Some(outcome) = self.replay(lease, verified).await? {
            return Ok(outcome);
        }
        let submissions = PgReviewSubmissions::new(self.pool.clone());
        let bound = preparer.with_authorization(Arc::new(submissions.authorization(lease)));
        let attempt = tokio::time::timeout(Duration::from_secs(60), async {
            submissions.heartbeat(lease, 300).await?;
            bound
                .recheck(verified.candidate())
                .await
                .map_err(SnapshotError::Preparation)?;
            self.publish_in(lease, verified).await
        })
        .await
        .unwrap_or(Err(SnapshotError::TimedOut));
        match attempt {
            Ok(outcome) => Ok(outcome),
            Err(error) => {
                // A concurrent invocation may commit between our initial read
                // and heartbeat/recheck. Recover its receipt with fresh access.
                match self.replay(lease, verified).await? {
                    Some(outcome) => Ok(outcome),
                    None => Err(error),
                }
            }
        }
    }
    async fn replay(
        &self,
        lease: &SubmissionLease,
        verified: &VerifiedReview,
    ) -> Result<Option<ReviewPublication>, SnapshotError> {
        // Read-only outcome replay still requires current original-actor access,
        // but does not require a live lease or repeat any provider work.
        let mut tx = self.transaction().await?;
        let org =
            review_submissions::access(&mut tx, lease.actor(), lease.proposal(), false).await?;
        if org != lease.organization() {
            return Err(SnapshotError::InvalidEvidence);
        }
        if let Some(mut outcome) = publication_in(&mut tx, lease.operation_id()).await? {
            if outcome.fingerprint != verified.fingerprint() {
                return Err(SnapshotError::InvalidEvidence);
            }
            outcome.replayed = true;
            tx.commit().await?;
            return Ok(Some(outcome));
        }
        tx.commit().await?;
        Ok(None)
    }
    async fn publish_in(
        &self,
        lease: &SubmissionLease,
        verified: &VerifiedReview,
    ) -> Result<ReviewPublication, SnapshotError> {
        let plan = verified.candidate().plan();
        let summary = plan.summary();
        let context = plan.context();
        let read = &verified.candidate().scope().comparison;
        let metadata = json!({"schema_version":1,"summary":summary,"context":{
            "author_id":context.author,"owner_id":context.owner,"team_id":context.team,
            "description":context.description,"ui_impact":summary.ui_impact,"ui_impact_reason":context.ui_impact_reason
        },"assignments":plan.assignments(),"validation":plan.validation()});
        let files: Vec<_> = plan
            .files()
            .iter()
            .map(serde_json::to_value)
            .collect::<Result<_, _>>()
            .map_err(|_| SnapshotError::InvalidEvidence)?;
        let book_documents = serde_json::to_value(plan.book_documents())
            .map_err(|_| SnapshotError::InvalidEvidence)?;
        let media: Vec<_> = verified
            .media()
            .iter()
            .map(serde_json::to_value)
            .collect::<Result<_, _>>()
            .map_err(|_| SnapshotError::InvalidEvidence)?;
        let mut tx = self.transaction().await?;
        // Use one lock order: proposal, then job/operation, then authority rows.
        let (title,number,delivery,active,state):(String,i64,String,Option<Uuid>,String) = sqlx::query_as("SELECT title,number,delivery_status,active_review_snapshot_id,state FROM proposals WHERE id=$1 AND organization_id=$2 FOR UPDATE")
            .bind(lease.proposal()).bind(lease.organization()).fetch_one(&mut *tx).await?;
        // Another publisher may have committed while our provider recheck ran.
        if let Some(mut outcome) = publication_in(&mut tx, lease.operation_id()).await? {
            review_submissions::access(&mut tx, lease.actor(), lease.proposal(), false).await?;
            if outcome.fingerprint != verified.fingerprint() {
                return Err(SnapshotError::InvalidEvidence);
            }
            outcome.replayed = true;
            tx.commit().await?;
            return Ok(outcome);
        }
        let current = review_authorization::lease_context_in(&mut tx, lease, true)
            .await
            .map_err(SubmissionError::from)?;
        if current != *verified.candidate().scope() {
            return Err(SnapshotError::InvalidEvidence);
        }
        let reused: Option<Uuid> = if matches!(
            state.as_str(),
            "IN_REVIEW" | "APPROVED" | "CHANGES_REQUESTED"
        ) {
            sqlx::query_scalar("SELECT id FROM review_snapshots WHERE id=$1 AND organization_id=$2 AND proposal_id=$3 AND state='READY' AND fingerprint=$4 AND book_manifest_version=1")
                .bind(active).bind(lease.organization()).bind(lease.proposal()).bind(verified.fingerprint()).fetch_optional(&mut *tx).await?
        } else {
            None
        };
        let activated = reused.is_none();
        let snapshot = reused.unwrap_or_else(Uuid::new_v4);
        if activated {
            let ids: Vec<_> = plan.files().iter().map(|file| &file.id).collect();
            sqlx::query("INSERT INTO review_snapshots(id,organization_id,proposal_id,repository_id,created_operation,created_by,fingerprint,plan_fingerprint,evidence_digest,media_validation_version,head_sha,target_sha,merge_base_sha,recorded_base_sha,prepared_version,configuration_version,policy_version,branch,remote_id,installation_id,proposal_number,title,delivery_status,metadata,file_count,file_ids,media_count,media_required) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23,$24,$25,$26,$27,$28)")
                .bind(snapshot).bind(lease.organization()).bind(lease.proposal()).bind(read.connection.repository).bind(lease.operation_id()).bind(lease.actor())
                .bind(verified.fingerprint()).bind(&summary.fingerprint).bind(verified.evidence_digest()).bind(verified.media_validation_version() as i32)
                .bind(&summary.head_sha).bind(&summary.target_sha).bind(&summary.merge_base_sha).bind(&summary.recorded_base_sha)
                .bind(summary.expected_proposal_version).bind(summary.configuration_version as i64).bind(summary.policy_version as i64)
                .bind(&summary.branch).bind(read.connection.remote_id.to_string()).bind(read.connection.installation_id.to_string())
                .bind(number).bind(title).bind(delivery).bind(metadata).bind(files.len() as i32).bind(json!(ids))
                .bind(media.len() as i32).bind(json!(plan.required_media())).execute(&mut *tx).await?;
            sqlx::query("INSERT INTO review_snapshot_books(organization_id,proposal_id,snapshot_id,documents) VALUES($1,$2,$3,$4)")
                .bind(lease.organization()).bind(lease.proposal()).bind(snapshot).bind(book_documents)
                .execute(&mut *tx).await?;
            for (chunk, values) in files.chunks(500).enumerate() {
                let mut query = QueryBuilder::<Postgres>::new(
                    "INSERT INTO review_snapshot_files(organization_id,proposal_id,snapshot_id,ordinal,file_id,change) ",
                );
                query.push_values(values.iter().enumerate(), |mut row, (index, value)| {
                    row.push_bind(lease.organization())
                        .push_bind(lease.proposal())
                        .push_bind(snapshot)
                        .push_bind((chunk * 500 + index) as i32)
                        .push_bind(&plan.files()[chunk * 500 + index].id)
                        .push_bind(value);
                });
                query.build().execute(&mut *tx).await?;
            }
            for (chunk, values) in media.chunks(500).enumerate() {
                let mut query = QueryBuilder::<Postgres>::new(
                    "INSERT INTO review_snapshot_media(organization_id,proposal_id,snapshot_id,ordinal,path,receipt) ",
                );
                query.push_values(values.iter().enumerate(), |mut row, (index, value)| {
                    row.push_bind(lease.organization())
                        .push_bind(lease.proposal())
                        .push_bind(snapshot)
                        .push_bind((chunk * 500 + index) as i32)
                        .push_bind(verified.media()[chunk * 500 + index].path())
                        .push_bind(value);
                });
                query.build().execute(&mut *tx).await?;
            }
            sqlx::query("UPDATE review_snapshots SET state='READY' WHERE id=$1")
                .bind(snapshot)
                .execute(&mut *tx)
                .await?;
        }
        let version = summary
            .expected_proposal_version
            .checked_add(i64::from(activated))
            .ok_or(SnapshotError::InvalidEvidence)?;
        sqlx::query("INSERT INTO review_submission_results(operation_id,organization_id,proposal_id,snapshot_id,applied_version,activated,fence_worker,fence_attempt) VALUES($1,$2,$3,$4,$5,$6,$7,$8)")
            .bind(lease.operation_id()).bind(lease.organization()).bind(lease.proposal()).bind(snapshot).bind(version).bind(activated).bind(lease.worker()).bind(lease.attempts()).execute(&mut *tx).await?;
        if activated {
            sqlx::query("UPDATE proposals SET active_review_snapshot_id=$2,target_sha=$3,state='IN_REVIEW',version=$4,updated_at=clock_timestamp() WHERE id=$1")
                .bind(lease.proposal()).bind(snapshot).bind(&summary.target_sha).bind(version).execute(&mut *tx).await?;
        }
        sqlx::query("INSERT INTO outbox(id,organization_id,event_type,entity_id,payload) VALUES($1,$2,'REVIEW_SNAPSHOT_PUBLISHED',$3,$4)")
            .bind(Uuid::new_v4()).bind(lease.organization()).bind(lease.proposal()).bind(json!({"snapshot_id":snapshot,"operation_id":lease.operation_id(),"pr_sync_required":true,"approvals_granted":false,"activated":activated})).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO audit_events(id,organization_id,actor_id,action,entity_id,after_ref,operation_id,result) VALUES($1,$2,$3,'REVIEW_SNAPSHOT_PUBLISHED',$4,$5,$6,$7)")
            .bind(Uuid::new_v4()).bind(lease.organization()).bind(lease.actor()).bind(lease.proposal()).bind(snapshot.to_string()).bind(lease.operation_id()).bind(if activated {"ACTIVATED"} else {"REUSED"}).execute(&mut *tx).await?;
        review_submissions::lock_lease(&mut tx, lease, true).await?;
        let changed = sqlx::query("UPDATE jobs SET state='SUCCEEDED',lease_owner=NULL,leased_until=NULL,result=jsonb_build_object('phase','SNAPSHOT_READY','snapshot_id',$2::uuid) WHERE id=$1 AND lease_owner=$3 AND attempts=$4 AND state='RUNNING' AND leased_until>clock_timestamp()")
            .bind(lease.job_id()).bind(snapshot).bind(lease.worker()).bind(lease.attempts()).execute(&mut *tx).await?.rows_affected();
        if changed != 1 {
            return Err(SubmissionError::LeaseLost.into());
        }
        sqlx::query("UPDATE git_operations SET state='SUCCEEDED',result_sha=$2,updated_at=clock_timestamp() WHERE id=$1 AND state='PENDING'")
            .bind(lease.operation_id()).bind(&summary.head_sha).execute(&mut *tx).await?;
        let result = publication_in(&mut tx, lease.operation_id())
            .await?
            .ok_or(SnapshotError::InvalidEvidence)?;
        tx.commit().await?;
        Ok(result)
    }
    pub async fn get(
        &self,
        actor: Uuid,
        proposal: Uuid,
        snapshot: Uuid,
    ) -> Result<SharedReviewSnapshot, SnapshotError> {
        let mut tx = self.transaction().await?;
        let org = read_access(&mut tx, actor, proposal).await?;
        let result = sqlx::query_as::<_, SharedReviewSnapshot>("SELECT h.id,h.proposal_id,h.fingerprint,h.evidence_digest,h.media_validation_version,h.created_at,h.created_by,h.title,h.proposal_number,h.delivery_status,h.metadata,h.file_count,h.media_count,coalesce(p.active_review_snapshot_id=h.id AND p.head_sha=h.head_sha AND p.target_sha=h.target_sha AND p.base_sha=h.recorded_base_sha AND p.review_configuration_version=h.configuration_version AND o.policy_version=h.policy_version AND p.repository_id=h.repository_id AND r.remote_id=h.remote_id AND r.installation_id=h.installation_id AND r.default_branch='main',false) AS current_metadata FROM review_snapshots h JOIN proposals p ON p.id=h.proposal_id AND p.organization_id=h.organization_id JOIN repositories r ON r.id=p.repository_id AND r.organization_id=p.organization_id JOIN organizations o ON o.id=p.organization_id WHERE h.id=$1 AND h.organization_id=$2 AND h.proposal_id=$3 AND h.state='READY'")
            .bind(snapshot).bind(org).bind(proposal).fetch_optional(&mut *tx).await?.ok_or(SubmissionError::NotFound)?;
        tx.commit().await?;
        Ok(result)
    }
    pub async fn history(
        &self,
        actor: Uuid,
        proposal: Uuid,
        after: Option<Uuid>,
        limit: i32,
    ) -> Result<ReviewSnapshotHistory, SnapshotError> {
        if !(1..=50).contains(&limit) || after.is_some_and(|id| id.is_nil()) {
            return Err(SubmissionError::InvalidQuery.into());
        }
        let mut tx = self.transaction().await?;
        let org = read_access(&mut tx, actor, proposal).await?;
        let (active,version,state,number,title,branch,target_branch):(Option<Uuid>,i64,String,i64,String,String,String)=sqlx::query_as("SELECT p.active_review_snapshot_id,p.version,p.state,p.number,p.title,p.branch,r.default_branch FROM proposals p JOIN repositories r ON r.id=p.repository_id AND r.organization_id=p.organization_id WHERE p.id=$1 AND p.organization_id=$2")
            .bind(proposal).bind(org).fetch_one(&mut *tx).await?;
        let cursor = if let Some(after) = after {
            Some(sqlx::query_scalar::<_,Timestamp>("SELECT created_at FROM review_snapshots WHERE id=$1 AND proposal_id=$2 AND organization_id=$3 AND state='READY'")
                .bind(after).bind(proposal).bind(org).fetch_optional(&mut *tx).await?.ok_or(SubmissionError::NotFound)?)
        } else {
            None
        };
        let mut rows=sqlx::query_as::<_,HistoryRow>("SELECT id,fingerprint,title,branch,head_sha,target_sha,prepared_version,configuration_version,policy_version,file_count,media_count,created_at,created_by FROM review_snapshots WHERE organization_id=$1 AND proposal_id=$2 AND state='READY' AND ($3::uuid IS NULL OR (created_at,id)<($4::timestamptz,$3::uuid)) ORDER BY created_at DESC,id DESC LIMIT $5")
            .bind(org).bind(proposal).bind(after).bind(cursor).bind(limit+1).fetch_all(&mut *tx).await?;
        let more = rows.len() > limit as usize;
        rows.truncate(limit as usize);
        let next_after = if more {
            rows.last().map(|row| row.id)
        } else {
            None
        };
        let snapshots = rows
            .into_iter()
            .map(|row| ReviewSnapshotHistoryEntry {
                snapshot_id: row.id,
                fingerprint: row.fingerprint,
                title: row.title,
                branch: row.branch,
                head_sha: row.head_sha,
                target_sha: row.target_sha,
                prepared_version: row.prepared_version,
                configuration_version: row.configuration_version,
                policy_version: row.policy_version,
                file_count: row.file_count,
                media_count: row.media_count,
                created_at: row.created_at.to_rfc3339(),
                created_by: row.created_by,
            })
            .collect();
        tx.commit().await?;
        Ok(ReviewSnapshotHistory {
            proposal_id: proposal,
            number,
            title,
            branch,
            target_branch,
            active_snapshot_id: active,
            current_proposal_version: version,
            current_proposal_state: state,
            snapshots,
            next_after,
        })
    }
    pub async fn media(
        &self,
        actor: Uuid,
        proposal: Uuid,
        snapshot: Uuid,
        offset: i32,
        limit: i32,
    ) -> Result<ReviewSnapshotMedia, SnapshotError> {
        if !(0..=20000).contains(&offset) || !(1..=100).contains(&limit) {
            return Err(SubmissionError::InvalidQuery.into());
        }
        let mut tx = self.transaction().await?;
        let org = read_access(&mut tx, actor, proposal).await?;
        let total:i32=sqlx::query_scalar("SELECT media_count FROM review_snapshots WHERE id=$1 AND organization_id=$2 AND proposal_id=$3 AND state='READY'")
            .bind(snapshot).bind(org).bind(proposal).fetch_optional(&mut *tx).await?.ok_or(SubmissionError::NotFound)?;
        let values:Vec<Value>=sqlx::query_scalar("SELECT receipt FROM review_snapshot_media WHERE snapshot_id=$1 AND organization_id=$2 AND proposal_id=$3 AND ordinal>=$4 ORDER BY ordinal LIMIT $5")
            .bind(snapshot).bind(org).bind(proposal).bind(offset).bind(limit).fetch_all(&mut *tx).await?;
        let receipts: Vec<ReviewMediaReceipt> = values
            .into_iter()
            .map(serde_json::from_value)
            .collect::<Result<_, _>>()
            .map_err(|_| SnapshotError::InvalidEvidence)?;
        let end = offset + receipts.len() as i32;
        tx.commit().await?;
        Ok(ReviewSnapshotMedia {
            snapshot_id: snapshot,
            offset,
            total,
            next_offset: (end < total).then_some(end),
            receipts,
        })
    }
    /// Legacy snapshots have no complete-book proof. Never fabricate it from
    /// their changed-file list or from a fresh repository read.
    pub async fn book_documents(
        &self,
        actor: Uuid,
        proposal: Uuid,
        snapshot: Uuid,
    ) -> Result<Option<Vec<ReviewBookDocument>>, SnapshotError> {
        let mut tx = self.transaction().await?;
        let org = read_access(&mut tx, actor, proposal).await?;
        let (version, documents): (i32, Option<Value>) = sqlx::query_as("SELECT h.book_manifest_version,b.documents FROM review_snapshots h LEFT JOIN review_snapshot_books b ON b.snapshot_id=h.id AND b.organization_id=h.organization_id AND b.proposal_id=h.proposal_id WHERE h.id=$1 AND h.proposal_id=$2 AND h.organization_id=$3 AND h.state='READY'")
            .bind(snapshot).bind(proposal).bind(org).fetch_optional(&mut *tx).await?.ok_or(SubmissionError::NotFound)?;
        let documents = match (version, documents) {
            (0, None) => None,
            (1, Some(value)) => {
                Some(serde_json::from_value(value).map_err(|_| SnapshotError::InvalidEvidence)?)
            }
            _ => return Err(SnapshotError::InvalidEvidence),
        };
        tx.commit().await?;
        Ok(documents)
    }
    pub async fn documents(
        &self,
        actor: Uuid,
        proposal: Uuid,
        snapshot: Uuid,
        offset: i32,
        limit: i32,
    ) -> Result<ReviewSnapshotDocuments, SnapshotError> {
        if !(0..=1000).contains(&offset) || !(1..=100).contains(&limit) {
            return Err(SubmissionError::InvalidQuery.into());
        }
        let mut tx = self.transaction().await?;
        let org = read_access(&mut tx, actor, proposal).await?;
        let (fingerprint, head_sha, version, exists, total, values): (String, String, i32, bool, i32, Value) = sqlx::query_as("SELECT h.fingerprint,h.head_sha,h.book_manifest_version,b.snapshot_id IS NOT NULL,coalesce(jsonb_array_length(b.documents),0),(SELECT coalesce(jsonb_agg(d.value ORDER BY d.ordinality),'[]'::jsonb) FROM (SELECT value,ordinality FROM jsonb_array_elements(b.documents) WITH ORDINALITY WHERE ordinality>$4 AND ordinality<=$4+$5) d) FROM review_snapshots h LEFT JOIN review_snapshot_books b ON b.snapshot_id=h.id AND b.organization_id=h.organization_id AND b.proposal_id=h.proposal_id WHERE h.id=$1 AND h.organization_id=$2 AND h.proposal_id=$3 AND h.state='READY'")
            .bind(snapshot).bind(org).bind(proposal).bind(offset).bind(limit).fetch_optional(&mut *tx).await?.ok_or(SubmissionError::NotFound)?;
        let available = match (version, exists) {
            (0, false) => false,
            (1, true) => true,
            _ => return Err(SnapshotError::InvalidEvidence),
        };
        let documents: Vec<ReviewBookDocument> =
            serde_json::from_value(values).map_err(|_| SnapshotError::InvalidEvidence)?;
        let end = offset + documents.len() as i32;
        tx.commit().await?;
        Ok(ReviewSnapshotDocuments {
            actor_id: actor,
            proposal_id: proposal,
            snapshot_id: snapshot,
            fingerprint,
            head_sha,
            available,
            offset,
            total,
            next_offset: (end < total).then_some(end),
            documents,
        })
    }
    pub async fn files(
        &self,
        actor: Uuid,
        proposal: Uuid,
        snapshot: Uuid,
        offset: i32,
        limit: i32,
    ) -> Result<SharedReviewFiles, SnapshotError> {
        if !(0..=40000).contains(&offset) || !(1..=100).contains(&limit) {
            return Err(SubmissionError::InvalidQuery.into());
        }
        let mut tx = self.transaction().await?;
        let org = read_access(&mut tx, actor, proposal).await?;
        let total: i32 = sqlx::query_scalar("SELECT file_count FROM review_snapshots WHERE id=$1 AND proposal_id=$2 AND organization_id=$3 AND state='READY'")
            .bind(snapshot).bind(proposal).bind(org).fetch_optional(&mut *tx).await?.ok_or(SubmissionError::NotFound)?;
        let values: Vec<Value> = sqlx::query_scalar("SELECT change FROM review_snapshot_files WHERE snapshot_id=$1 AND organization_id=$2 AND proposal_id=$3 AND ordinal>=$4 ORDER BY ordinal LIMIT $5")
            .bind(snapshot).bind(org).bind(proposal).bind(offset).bind(limit).fetch_all(&mut *tx).await?;
        let files = values
            .into_iter()
            .map(serde_json::from_value)
            .collect::<Result<_, _>>()
            .map_err(|_| SnapshotError::InvalidEvidence)?;
        tx.commit().await?;
        Ok(SharedReviewFiles {
            snapshot_id: snapshot,
            offset,
            total,
            files,
        })
    }
}
#[derive(FromRow)]
struct HistoryRow {
    id: Uuid,
    fingerprint: String,
    title: String,
    branch: String,
    head_sha: String,
    target_sha: String,
    prepared_version: i64,
    configuration_version: i64,
    policy_version: i64,
    file_count: i32,
    media_count: i32,
    created_at: Timestamp,
    created_by: Uuid,
}
pub(crate) async fn read_access(
    connection: &mut PgConnection,
    actor: Uuid,
    proposal: Uuid,
) -> Result<Uuid, SubmissionError> {
    sqlx::query_scalar("SELECT p.organization_id FROM proposals p JOIN repositories r ON r.id=p.repository_id AND r.organization_id=p.organization_id JOIN organizations o ON o.id=p.organization_id JOIN memberships m ON m.organization_id=p.organization_id JOIN users u ON u.id=m.user_id WHERE p.id=$1 AND u.id=$2 AND u.status='ACTIVE' FOR SHARE OF p,r,o,m,u")
        .bind(proposal).bind(actor).fetch_optional(connection).await?.ok_or(SubmissionError::NotFound)
}
#[derive(FromRow)]
struct PublicationRow {
    proposal_id: Uuid,
    operation_id: Uuid,
    job_id: Uuid,
    snapshot_id: Uuid,
    fingerprint: String,
    evidence_digest: String,
    proposal_version: i64,
    activated: bool,
}
pub(crate) async fn publication_in(
    connection: &mut PgConnection,
    operation: Uuid,
) -> Result<Option<ReviewPublication>, SubmissionError> {
    let row = sqlx::query_as::<_,PublicationRow>("SELECT x.proposal_id,x.operation_id,s.job_id,x.snapshot_id,h.fingerprint,h.evidence_digest,x.applied_version AS proposal_version,x.activated FROM review_submission_results x JOIN review_submissions s ON s.operation_id=x.operation_id AND s.organization_id=x.organization_id AND s.proposal_id=x.proposal_id JOIN review_snapshots h ON h.id=x.snapshot_id AND h.organization_id=x.organization_id AND h.proposal_id=x.proposal_id JOIN jobs j ON j.id=s.job_id AND j.operation_id=s.operation_id AND j.organization_id=s.organization_id JOIN git_operations o ON o.id=s.operation_id AND o.organization_id=s.organization_id WHERE x.operation_id=$1 AND h.state='READY' AND j.state='SUCCEEDED' AND o.state='SUCCEEDED' AND j.result->>'phase'='SNAPSHOT_READY' AND j.result->>'snapshot_id'=x.snapshot_id::text")
        .bind(operation).fetch_optional(connection).await?;
    Ok(row.map(|row| ReviewPublication {
        proposal_id: row.proposal_id,
        operation_id: row.operation_id,
        job_id: row.job_id,
        snapshot_id: row.snapshot_id,
        fingerprint: row.fingerprint,
        evidence_digest: row.evidence_digest,
        proposal_version: row.proposal_version,
        activated: row.activated,
        replayed: false,
    }))
}
