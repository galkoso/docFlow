//! Database contract tests; orchestration/provider evidence is tested separately.
use super::*;
use serde_json::json;
use specspace_infrastructure::{complete_job, lease_job};
use sqlx::PgConnection;

#[derive(Clone)]
struct Work {
    op: Uuid,
    job: Uuid,
    org: Uuid,
    proposal: Uuid,
    actor: Uuid,
    snapshot: Uuid,
    audit: Uuid,
    event: Uuid,
    result_audit: Uuid,
    result_event: Uuid,
    worker: Uuid,
    attempt: i32,
    target: Option<Uuid>,
}
impl Work {
    fn new(f: &Fixture, snapshot: Uuid) -> Self {
        Self {
            op: Uuid::new_v4(),
            job: Uuid::new_v4(),
            org: f.org,
            proposal: f.proposal,
            actor: f.owner,
            snapshot,
            audit: Uuid::new_v4(),
            event: Uuid::new_v4(),
            result_audit: Uuid::new_v4(),
            result_event: Uuid::new_v4(),
            worker: Uuid::new_v4(),
            attempt: 1,
            target: None,
        }
    }
    fn purpose(&self) -> &str {
        if self.target.is_some() {
            "INVALIDATION"
        } else {
            "EVALUATION"
        }
    }
    fn result(&self) -> &str {
        if self.target.is_some() {
            "FAILURE"
        } else {
            "PENDING"
        }
    }
}
async fn fixture(pool: &PgPool) -> (Fixture, Uuid) {
    let (f, _, h, _, _) = ready(pool, true).await;
    sqlx::query("UPDATE repositories SET check_app_id=7 WHERE id=$1")
        .bind(f.repository)
        .execute(pool)
        .await
        .unwrap();
    (f, h)
}
async fn reserve_parts(conn: &mut PgConnection, w: &Work) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO git_operations(id,organization_id,proposal_id,actor_id,kind,idempotency_key,request_hash,expected_sha)
        SELECT $1,$2,$3,$4,'REVIEW_CHECK',$1::text,$5,head_sha FROM review_snapshots WHERE id=$6")
        .bind(w.op).bind(w.org).bind(w.proposal).bind(w.actor).bind("a".repeat(64)).bind(w.snapshot).execute(&mut *conn).await?;
    sqlx::query("INSERT INTO jobs(id,organization_id,operation_id,kind,payload,max_attempts) VALUES($1,$2,$3,'PUBLISH_REVIEW_CHECK',jsonb_build_object('operation_id',$3::uuid),8)")
        .bind(w.job).bind(w.org).bind(w.op).execute(&mut *conn).await?;
    sqlx::query("INSERT INTO review_check_intents(operation_id,job_id,organization_id,proposal_id,repository_id,actor_id,snapshot_id,
        remote_id,installation_id,app_id,branch,head_sha,target_sha,merge_base_sha,proposal_version,configuration_version,policy_version,
        fingerprint,evidence_digest,authority_digest,request_hash,purpose,result,report_digest,invalidation_reason,target_run_id,target_operation,audit_id,event_id)
        SELECT $1,$2,p.organization_id,p.id,p.repository_id,$3,h.id,42,7,7,p.branch,h.head_sha,h.target_sha,h.merge_base_sha,
        p.version,h.configuration_version,h.policy_version,h.fingerprint,h.evidence_digest,$4,$4,$5,$6,
        CASE WHEN $5='EVALUATION' THEN $4 END,CASE WHEN $5='INVALIDATION' THEN 'TARGET_CHANGED' END,
        CASE WHEN $8::uuid IS NOT NULL THEN 81 END,$8,$9,$10
        FROM review_snapshots h JOIN proposals p ON p.id=h.proposal_id AND p.organization_id=h.organization_id WHERE h.id=$7 AND p.id=$11")
        .bind(w.op).bind(w.job).bind(w.actor).bind("a".repeat(64)).bind(w.purpose()).bind(w.result()).bind(w.snapshot).bind(w.target).bind(w.audit).bind(w.event).bind(w.proposal)
        .execute(&mut *conn).await?;
    Ok(())
}
async fn evidence(conn: &mut PgConnection, w: &Work) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO audit_events(id,organization_id,actor_id,action,entity_id,after_ref,operation_id,result) VALUES($1,$2,$3,'REVIEW_CHECK_RESERVED',$4,$5,$6,'QUEUED')")
        .bind(w.audit).bind(w.org).bind(w.actor).bind(w.proposal).bind(w.snapshot.to_string()).bind(w.op).execute(&mut *conn).await?;
    sqlx::query("INSERT INTO outbox(id,organization_id,event_type,entity_id,payload) VALUES($1,$2,'REVIEW_CHECK_QUEUED',$3,jsonb_build_object('operation_id',$4::uuid,'snapshot_id',$5::uuid,'purpose',$6::text,'result',$7::text))")
        .bind(w.event).bind(w.org).bind(w.proposal).bind(w.op).bind(w.snapshot).bind(w.purpose()).bind(w.result()).execute(conn).await?;
    Ok(())
}
async fn reserve(pool: &PgPool, w: &Work) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    reserve_parts(&mut tx, w).await?;
    evidence(&mut tx, w).await?;
    tx.commit().await
}
async fn claim(pool: &PgPool, w: &Work) {
    sqlx::query("UPDATE jobs SET state='RUNNING',attempts=$2,lease_owner=$3,leased_until=clock_timestamp()+interval '60 seconds' WHERE id=$1")
        .bind(w.job).bind(w.attempt).bind(w.worker).execute(pool).await.unwrap();
}
async fn begin(pool: &PgPool, w: &Work, phase: &str) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("INSERT INTO review_check_writes(operation_id,phase,run_id,check_operation,result,fence_worker,fence_attempt)
        VALUES($1,$2,CASE WHEN $2='UPDATE' THEN 81 END,$3,$4,$5,$6)")
        .bind(w.op).bind(phase).bind(w.target.unwrap_or(w.op)).bind(if phase=="CREATE" {"PENDING"} else {w.result()}).bind(w.worker).bind(w.attempt).execute(&mut *tx).await?;
    sqlx::query("UPDATE git_operations SET state='RUNNING' WHERE id=$1")
        .bind(w.op)
        .execute(&mut *tx)
        .await?;
    tx.commit().await
}
async fn adopt(pool: &PgPool, w: &Work) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO review_check_runs(operation_id,organization_id,proposal_id,repository_id,remote_id,app_id,head_sha,run_id,fence_worker,fence_attempt)
        SELECT operation_id,organization_id,proposal_id,repository_id,remote_id,app_id,head_sha,81,$2,$3 FROM review_check_intents WHERE operation_id=$1")
        .bind(w.op).bind(w.worker).bind(w.attempt).execute(pool).await.map(|_| ())
}
async fn finish_parts(conn: &mut PgConnection, w: &Work) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO review_check_results(operation_id,run_id,check_operation,result,request_hash,fence_worker,fence_attempt,audit_id,event_id) VALUES($1,81,$2,$3,$4,$5,$6,$7,$8)")
        .bind(w.op).bind(w.target.unwrap_or(w.op)).bind(w.result()).bind("a".repeat(64)).bind(w.worker).bind(w.attempt).bind(w.result_audit).bind(w.result_event).execute(&mut *conn).await?;
    sqlx::query("UPDATE jobs SET state='SUCCEEDED',lease_owner=NULL,leased_until=NULL,result=jsonb_build_object('phase','REVIEW_CHECK_PUBLISHED','run_id',81,'result',$2::text) WHERE id=$1")
        .bind(w.job).bind(w.result()).execute(&mut *conn).await?;
    sqlx::query("UPDATE git_operations SET state='SUCCEEDED',result_sha=expected_sha WHERE id=$1")
        .bind(w.op)
        .execute(conn)
        .await?;
    Ok(())
}
async fn finish(pool: &PgPool, w: &Work) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    finish_parts(&mut tx, w).await?;
    sqlx::query("INSERT INTO audit_events(id,organization_id,actor_id,action,entity_id,after_ref,operation_id,result) VALUES($1,$2,$3,'REVIEW_CHECK_PUBLISHED',$4,'81',$5,'PUBLISHED')")
        .bind(w.result_audit).bind(w.org).bind(w.actor).bind(w.proposal).bind(w.op).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO outbox(id,organization_id,event_type,entity_id,payload) VALUES($1,$2,'REVIEW_CHECK_PUBLISHED',$3,jsonb_build_object('operation_id',$4::uuid,'snapshot_id',$5::uuid,'run_id',81,'result',$6::text))")
        .bind(w.result_event).bind(w.org).bind(w.proposal).bind(w.op).bind(w.snapshot).bind(w.result()).execute(&mut *tx).await?;
    tx.commit().await
}

#[sqlx::test(migrations = "../../migrations")]
async fn check_admission_requires_exact_atomic_scope_app_audit_and_event(pool: PgPool) {
    let (f, h) = fixture(&pool).await;
    let w = Work::new(&f, h);
    let mut tx = pool.begin().await.unwrap();
    reserve_parts(&mut tx, &w).await.unwrap();
    assert!(tx.commit().await.is_err());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM git_operations WHERE id=$1")
            .bind(w.op)
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    for app in [None, Some(8)] {
        sqlx::query("UPDATE repositories SET check_app_id=$2 WHERE id=$1")
            .bind(f.repository)
            .bind(app)
            .execute(&pool)
            .await
            .unwrap();
        assert!(reserve(&pool, &w).await.is_err());
    }
    sqlx::query("UPDATE repositories SET check_app_id=7 WHERE id=$1")
        .bind(f.repository)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE memberships SET role='READER' WHERE organization_id=$1 AND user_id=$2")
        .bind(f.org)
        .bind(f.owner)
        .execute(&pool)
        .await
        .unwrap();
    assert!(reserve(&pool, &w).await.is_err());
    sqlx::query(
        "UPDATE memberships SET role='CONTRIBUTOR' WHERE organization_id=$1 AND user_id=$2",
    )
    .bind(f.org)
    .bind(f.owner)
    .execute(&pool)
    .await
    .unwrap();
    reserve(&pool, &w).await.unwrap();
    assert!(reserve(&pool, &Work::new(&f, h)).await.is_err());
    assert!(
        lease_job(&pool, Uuid::new_v4(), 30)
            .await
            .unwrap()
            .is_none()
    );
    for query in [
        "UPDATE review_check_intents SET request_hash=repeat('b',64)",
        "DELETE FROM review_check_intents",
        "TRUNCATE review_check_intents CASCADE",
        "UPDATE outbox SET payload='{}' WHERE event_type='REVIEW_CHECK_QUEUED'",
    ] {
        assert!(sqlx::query(query).execute(&pool).await.is_err(), "{query}");
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn check_write_run_and_completion_are_phase_bound_and_immutable(pool: PgPool) {
    let (f, h) = fixture(&pool).await;
    let w = Work::new(&f, h);
    reserve(&pool, &w).await.unwrap();
    claim(&pool, &w).await;
    assert!(
        !complete_job(&pool, w.job, w.worker, json!({}))
            .await
            .unwrap()
    );
    assert!(adopt(&pool, &w).await.is_err());
    assert!(begin(&pool, &w, "UPDATE").await.is_err());
    assert!(finish(&pool, &w).await.is_err());
    begin(&pool, &w, "CREATE").await.unwrap();
    assert!(begin(&pool, &w, "CREATE").await.is_err());
    adopt(&pool, &w).await.unwrap();
    assert!(finish(&pool, &w).await.is_err());
    begin(&pool, &w, "UPDATE").await.unwrap();
    assert!(
        sqlx::query("UPDATE jobs SET state='SUCCEEDED' WHERE id=$1")
            .bind(w.job)
            .execute(&pool)
            .await
            .is_err()
    );
    finish(&pool, &w).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM review_check_results WHERE operation_id=$1"
        )
        .bind(w.op)
        .fetch_one(&pool)
        .await
        .unwrap(),
        1
    );
    for table in [
        "review_check_writes",
        "review_check_runs",
        "review_check_results",
    ] {
        assert!(
            sqlx::query(&format!("DELETE FROM {table}"))
                .execute(&pool)
                .await
                .is_err()
        );
        assert!(
            sqlx::query(&format!("TRUNCATE {table} CASCADE"))
                .execute(&pool)
                .await
                .is_err()
        );
    }
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM proposals WHERE id=$1")
            .bind(f.proposal)
            .fetch_one(&pool)
            .await
            .unwrap(),
        "IN_REVIEW"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn check_same_owner_reclaim_changes_attempt_and_never_rearms_started_writes(pool: PgPool) {
    let (f, h) = fixture(&pool).await;
    let w = Work::new(&f, h);
    reserve(&pool, &w).await.unwrap();
    claim(&pool, &w).await;
    begin(&pool, &w, "CREATE").await.unwrap();
    sqlx::query("UPDATE git_operations SET state='UNKNOWN' WHERE id=$1")
        .bind(w.op)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE jobs SET leased_until=clock_timestamp()-interval '1 second' WHERE id=$1")
        .bind(w.job)
        .execute(&pool)
        .await
        .unwrap();
    assert!(adopt(&pool, &w).await.is_err());
    let mut next = w.clone();
    next.attempt = 2;
    claim(&pool, &next).await;
    assert!(adopt(&pool, &w).await.is_err());
    assert!(begin(&pool, &next, "CREATE").await.is_err());
    assert!(
        sqlx::query("UPDATE git_operations SET state='FAILED' WHERE id=$1")
            .bind(w.op)
            .execute(&pool)
            .await
            .is_err()
    );
    adopt(&pool, &next).await.unwrap();
    begin(&pool, &next, "UPDATE").await.unwrap();
    assert!(finish(&pool, &w).await.is_err());
    finish(&pool, &next).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i32>(
            "SELECT fence_attempt FROM review_check_writes WHERE operation_id=$1 AND phase='CREATE'"
        )
        .bind(w.op)
        .fetch_one(&pool)
        .await
        .unwrap(),
        1
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn changed_authority_and_metadata_block_writes_but_preserve_known_run_identity(pool: PgPool) {
    let (f, h) = fixture(&pool).await;
    let w = Work::new(&f, h);
    reserve(&pool, &w).await.unwrap();
    claim(&pool, &w).await;
    begin(&pool, &w, "CREATE").await.unwrap();
    sqlx::query("UPDATE repositories SET check_app_id=8 WHERE id=$1")
        .bind(f.repository)
        .execute(&pool)
        .await
        .unwrap();
    assert!(adopt(&pool, &w).await.is_err());
    sqlx::query("UPDATE repositories SET check_app_id=7 WHERE id=$1")
        .bind(f.repository)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE users SET status='DISABLED' WHERE id=$1")
        .bind(f.owner)
        .execute(&pool)
        .await
        .unwrap();
    adopt(&pool, &w).await.unwrap();
    assert!(begin(&pool, &w, "UPDATE").await.is_err());
    sqlx::query("UPDATE users SET status='ACTIVE' WHERE id=$1")
        .bind(f.owner)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE proposals SET head_sha=$2,version=version+1 WHERE id=$1")
        .bind(f.proposal)
        .bind(sha(3).as_str())
        .execute(&pool)
        .await
        .unwrap();
    assert!(begin(&pool, &w, "UPDATE").await.is_err());
    assert!(finish(&pool, &w).await.is_err());
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM review_check_writes WHERE operation_id=$1"
        )
        .bind(w.op)
        .fetch_one(&pool)
        .await
        .unwrap(),
        1
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn explicit_invalidation_can_fail_only_its_original_run_after_context_changes(pool: PgPool) {
    let (f, h) = fixture(&pool).await;
    let w = Work::new(&f, h);
    reserve(&pool, &w).await.unwrap();
    claim(&pool, &w).await;
    begin(&pool, &w, "CREATE").await.unwrap();
    adopt(&pool, &w).await.unwrap();
    begin(&pool, &w, "UPDATE").await.unwrap();
    finish(&pool, &w).await.unwrap();
    let mut invalidation = Work::new(&f, h);
    invalidation.target = Some(w.op);
    assert!(reserve(&pool, &invalidation).await.is_err());
    sqlx::query("UPDATE memberships SET role='MAINTAINER' WHERE organization_id=$1 AND user_id=$2")
        .bind(f.org)
        .bind(f.owner)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE proposals SET head_sha=$2,version=version+1,state='CLOSED' WHERE id=$1")
        .bind(f.proposal)
        .bind(sha(3).as_str())
        .execute(&pool)
        .await
        .unwrap();
    reserve(&pool, &invalidation).await.unwrap();
    claim(&pool, &invalidation).await;
    assert!(begin(&pool, &invalidation, "CREATE").await.is_err());
    begin(&pool, &invalidation, "UPDATE").await.unwrap();
    finish(&pool, &invalidation).await.unwrap();
    let (operation, result): (Uuid, String) = sqlx::query_as(
        "SELECT check_operation,result FROM review_check_results WHERE operation_id=$1",
    )
    .bind(invalidation.op)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(operation, w.op);
    assert_eq!(result, "FAILURE");
}

#[sqlx::test(migrations = "../../migrations")]
async fn publication_without_its_audit_and_event_rolls_back_the_receipt_and_outcome(pool: PgPool) {
    let (f, h) = fixture(&pool).await;
    let w = Work::new(&f, h);
    reserve(&pool, &w).await.unwrap();
    claim(&pool, &w).await;
    begin(&pool, &w, "CREATE").await.unwrap();
    adopt(&pool, &w).await.unwrap();
    begin(&pool, &w, "UPDATE").await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    finish_parts(&mut tx, &w).await.unwrap();
    assert!(tx.commit().await.is_err());
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM review_check_results WHERE operation_id=$1"
        )
        .bind(w.op)
        .fetch_one(&pool)
        .await
        .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM jobs WHERE id=$1")
            .bind(w.job)
            .fetch_one(&pool)
            .await
            .unwrap(),
        "RUNNING"
    );
    finish(&pool, &w).await.unwrap();
    assert!(
        sqlx::query("UPDATE outbox SET payload='{}' WHERE id=$1")
            .bind(w.result_event)
            .execute(&pool)
            .await
            .is_err()
    );
}
