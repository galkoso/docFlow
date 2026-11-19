use super::*;
use specspace_git_adapter::GitError;

pub(super) fn provider_error(error: GitError) -> OfficialReviewError {
    match error {
        GitError::RateLimited {
            retry_after_seconds,
        } => OfficialReviewError::ProviderRateLimited(
            retry_after_seconds.unwrap_or(60).clamp(1, u32::MAX as u64) as u32,
        ),
        GitError::BranchMissing | GitError::HeadMismatch => OfficialReviewError::Stale,
        _ => OfficialReviewError::ProviderUnavailable,
    }
}
pub(super) async fn admit(
    conn: &mut PgConnection,
    s: &Scope,
    actor: Uuid,
    proposal: Uuid,
    snapshot: Uuid,
    request: &OfficialReviewRequest,
    hash: &str,
) -> Result<(), OfficialReviewError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('official-review-quota:' || $1::uuid::text,0))").bind(s.organization_id).execute(&mut *conn).await?;
    let mismatch:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM official_review_reads WHERE organization_id=$1 AND request_id=$2 AND (request_hash<>$3 OR reviewer_id<>$4 OR proposal_id<>$5 OR snapshot_id<>$6))")
        .bind(s.organization_id).bind(request.request_id).bind(hash).bind(actor).bind(proposal).bind(snapshot).fetch_one(&mut *conn).await?;
    if mismatch {
        return Err(OfficialReviewError::IdempotencyMismatch);
    }
    let delay:Option<f64>=sqlx::query_scalar("SELECT ceil(extract(epoch FROM until_at-clock_timestamp()))::float8 FROM official_review_backoff WHERE organization_id=$1 AND repository_id=$2 AND remote_id=$3 AND installation_id=$4 AND until_at>clock_timestamp()")
        .bind(s.organization_id).bind(s.repository_id).bind(&s.remote_id).bind(&s.installation_id).fetch_optional(&mut *conn).await?;
    if let Some(delay) = delay {
        return Err(OfficialReviewError::ProviderRateLimited(
            delay.clamp(1., u32::MAX as f64) as u32,
        ));
    }
    let allowed:bool=sqlx::query_scalar("SELECT count(*)<200 AND count(*) FILTER(WHERE reviewer_id=$2)<30 FROM official_review_reads WHERE organization_id=$1 AND created_at>clock_timestamp()-interval '10 minutes'")
        .bind(s.organization_id).bind(actor).fetch_one(&mut *conn).await?;
    if !allowed {
        return Err(OfficialReviewError::Quota);
    }
    sqlx::query("INSERT INTO official_review_reads(id,organization_id,proposal_id,snapshot_id,reviewer_id,request_id,request_hash) VALUES($1,$2,$3,$4,$5,$6,$7)")
        .bind(Uuid::new_v4()).bind(s.organization_id).bind(proposal).bind(snapshot).bind(actor).bind(request.request_id).bind(hash).execute(conn).await?;
    Ok(())
}
pub(super) async fn backoff(
    pool: &PgPool,
    s: &Scope,
    seconds: u32,
) -> Result<(), OfficialReviewError> {
    sqlx::query("INSERT INTO official_review_backoff(organization_id,repository_id,remote_id,installation_id,until_at) VALUES($1,$2,$3,$4,clock_timestamp()+make_interval(secs=>$5)) ON CONFLICT(organization_id,repository_id,remote_id,installation_id) DO UPDATE SET until_at=greatest(official_review_backoff.until_at,excluded.until_at)")
        .bind(s.organization_id).bind(s.repository_id).bind(&s.remote_id).bind(&s.installation_id).bind(f64::from(seconds)).execute(pool).await?;
    Ok(())
}

pub(super) async fn failure(
    pool: &PgPool,
    s: &Scope,
    actor: Uuid,
    proposal: Uuid,
    snapshot: Uuid,
    request: Uuid,
    error: &OfficialReviewError,
) -> Result<(), OfficialReviewError> {
    let code = match error {
        OfficialReviewError::ProviderRateLimited(_) => "PROVIDER_RATE_LIMIT",
        OfficialReviewError::Stale => "PROVIDER_REFS_CHANGED",
        _ => "PROVIDER_UNAVAILABLE",
    };
    sqlx::query("INSERT INTO audit_events(id,organization_id,actor_id,action,entity_id,before_ref,after_ref,result) VALUES($1,$2,$3,'OFFICIAL_REVIEW_SOURCE_FAILED',$4,$5,$6,$7)")
        .bind(Uuid::new_v4()).bind(s.organization_id).bind(actor).bind(proposal).bind(snapshot.to_string()).bind(request.to_string()).bind(code).execute(pool).await?;
    Ok(())
}
