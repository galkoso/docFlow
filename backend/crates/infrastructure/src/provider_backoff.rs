//! Persistent cooldown for the exact server-resolved repository connection.
//! The existing table name reflects the first caller; discussions share it.
use sqlx::PgConnection;
use uuid::Uuid;

pub(crate) struct ConnectionKey<'a> {
    pub organization: Uuid,
    pub repository: Uuid,
    pub remote_id: &'a str,
    pub installation_id: &'a str,
}

pub(crate) async fn remaining(
    conn: &mut PgConnection,
    key: &ConnectionKey<'_>,
) -> Result<Option<u32>, sqlx::Error> {
    let seconds:Option<f64>=sqlx::query_scalar("SELECT ceil(extract(epoch FROM until_at-clock_timestamp()))::float8 FROM official_review_backoff WHERE organization_id=$1 AND repository_id=$2 AND remote_id=$3 AND installation_id=$4 AND until_at>clock_timestamp()")
        .bind(key.organization).bind(key.repository).bind(key.remote_id).bind(key.installation_id).fetch_optional(conn).await?;
    Ok(seconds.map(|seconds| seconds.clamp(1., u32::MAX as f64) as u32))
}

pub(crate) async fn defer(
    conn: &mut PgConnection,
    key: &ConnectionKey<'_>,
    seconds: u32,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO official_review_backoff(organization_id,repository_id,remote_id,installation_id,until_at) VALUES($1,$2,$3,$4,clock_timestamp()+make_interval(secs=>$5)) ON CONFLICT(organization_id,repository_id,remote_id,installation_id) DO UPDATE SET until_at=greatest(official_review_backoff.until_at,excluded.until_at)")
        .bind(key.organization).bind(key.repository).bind(key.remote_id).bind(key.installation_id).bind(f64::from(seconds.max(1))).execute(conn).await?;
    Ok(())
}
