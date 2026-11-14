//! PostgreSQL persistence primitives; provider calls never hold a DB transaction.
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");

#[derive(Debug, FromRow)]
pub struct LeasedJob {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub operation_id: Option<Uuid>,
    pub kind: String,
    pub payload: serde_json::Value,
    pub attempts: i32,
}

/// Claims only queued work. An expired RUNNING operation must be reconciled with
/// its provider before a separate recovery action can safely requeue it.
pub async fn lease_job(
    pool: &PgPool,
    worker_id: Uuid,
    lease_seconds: i32,
) -> Result<Option<LeasedJob>, sqlx::Error> {
    if !(1..=3600).contains(&lease_seconds) {
        return Err(sqlx::Error::Protocol("invalid lease duration".into()));
    }
    sqlx::query_as::<_, LeasedJob>(
        "WITH candidate AS (
          SELECT id FROM jobs
          WHERE state = 'QUEUED' AND available_at <= now() AND attempts < max_attempts
          ORDER BY created_at, id FOR UPDATE SKIP LOCKED LIMIT 1
        )
        UPDATE jobs SET state = 'RUNNING', lease_owner = $1,
          leased_until = now() + make_interval(secs => $2), heartbeat_at = now(),
          attempts = attempts + 1
        FROM candidate WHERE jobs.id = candidate.id
        RETURNING jobs.id, organization_id, operation_id, kind, payload, attempts",
    )
    .bind(worker_id)
    .bind(f64::from(lease_seconds))
    .fetch_optional(pool)
    .await
}

/// Fencing by owner and unexpired lease prevents a late worker completing work
/// that recovery has already reassigned. Returns false when the lease was lost.
pub async fn complete_job(
    pool: &PgPool,
    job_id: Uuid,
    worker_id: Uuid,
    result: serde_json::Value,
) -> Result<bool, sqlx::Error> {
    let updated = sqlx::query(
        "UPDATE jobs SET state = 'SUCCEEDED', result = $3, lease_owner = NULL,
          leased_until = NULL
        WHERE id = $1 AND lease_owner = $2 AND state = 'RUNNING' AND leased_until > now()",
    )
    .bind(job_id)
    .bind(worker_id)
    .bind(result)
    .execute(pool)
    .await?;
    Ok(updated.rows_affected() == 1)
}
