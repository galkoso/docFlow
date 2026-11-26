//! Shared atomic reservation shape for evaluation and failure-only invalidation.
use super::*;
use serde_json::json;
use specspace_application::review_check_publication::CheckPurpose;

pub(super) async fn reserve(
    conn: &mut PgConnection,
    intent: &CheckPublicationIntent,
) -> Result<Uuid, CheckStoreError> {
    let job = Uuid::new_v4();
    let audit = Uuid::new_v4();
    let event = Uuid::new_v4();
    let s = intent.source();
    let (purpose, report, reason, run, target) = match intent.purpose() {
        CheckPurpose::Evaluation { report_digest } => {
            ("EVALUATION", Some(report_digest.as_str()), None, None, None)
        }
        CheckPurpose::Invalidation {
            reason,
            run,
            check_operation,
        } => (
            "INVALIDATION",
            None,
            Some(reason.code()),
            Some(run.get() as i64),
            Some(*check_operation),
        ),
    };
    sqlx::query("INSERT INTO git_operations(id,organization_id,proposal_id,actor_id,kind,idempotency_key,request_hash,expected_sha) VALUES($1,$2,$3,$4,'REVIEW_CHECK',$1::text,$5,$6)")
        .bind(s.operation).bind(s.organization).bind(s.proposal).bind(s.actor).bind(intent.request_hash()).bind(s.head.as_str()).execute(&mut *conn).await?;
    sqlx::query("INSERT INTO jobs(id,organization_id,operation_id,kind,payload,max_attempts) VALUES($1,$2,$3,'PUBLISH_REVIEW_CHECK',jsonb_build_object('operation_id',$3::uuid),8)")
        .bind(job).bind(s.organization).bind(s.operation).execute(&mut *conn).await?;
    sqlx::query("INSERT INTO review_check_intents(operation_id,job_id,organization_id,proposal_id,repository_id,actor_id,snapshot_id,
        remote_id,installation_id,app_id,branch,head_sha,target_sha,merge_base_sha,proposal_version,configuration_version,policy_version,
        fingerprint,evidence_digest,authority_digest,request_hash,purpose,result,report_digest,invalidation_reason,target_run_id,target_operation,audit_id,event_id)
        VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23,$24,$25,$26,$27,$28,$29)")
        .bind(s.operation).bind(job).bind(s.organization).bind(s.proposal).bind(s.repository.repository).bind(s.actor).bind(s.snapshot)
        .bind(s.repository.remote_id as i64).bind(s.repository.installation_id as i64).bind(s.app_id as i64).bind(s.branch.as_str()).bind(s.head.as_str()).bind(s.target.as_str()).bind(s.merge_base.as_str())
        .bind(s.proposal_version).bind(s.configuration_version).bind(s.policy_version).bind(&s.fingerprint).bind(&s.evidence_digest).bind(&s.authority_digest).bind(intent.request_hash())
        .bind(purpose).bind(read::result(intent.result())).bind(report).bind(reason).bind(run).bind(target).bind(audit).bind(event).execute(&mut *conn).await?;
    sqlx::query("INSERT INTO audit_events(id,organization_id,actor_id,action,entity_id,after_ref,operation_id,result) VALUES($1,$2,$3,'REVIEW_CHECK_RESERVED',$4,$5,$6,'QUEUED')")
        .bind(audit).bind(s.organization).bind(s.actor).bind(s.proposal).bind(s.snapshot.to_string()).bind(s.operation).execute(&mut *conn).await?;
    sqlx::query("INSERT INTO outbox(id,organization_id,event_type,entity_id,payload) VALUES($1,$2,'REVIEW_CHECK_QUEUED',$3,$4)")
        .bind(event).bind(s.organization).bind(s.proposal).bind(json!({"operation_id":s.operation,"snapshot_id":s.snapshot,"purpose":purpose,"result":read::result(intent.result())})).execute(conn).await?;
    Ok(job)
}
