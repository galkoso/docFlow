use async_trait::async_trait;
use specspace_application::book::{BookError, ReadAuthorization};
use sqlx::PgPool;
use uuid::Uuid;

pub struct PgReadAuthorization {
    pool: PgPool,
}

impl PgReadAuthorization {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl ReadAuthorization for PgReadAuthorization {
    async fn can_read(&self, actor: Uuid, repository: Uuid) -> Result<bool, BookError> {
        // v1 has one organization repository. All its active members may read it.
        // No role, organization or repository scope is accepted from the browser.
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (
              SELECT 1 FROM users u
              JOIN memberships m ON m.user_id = u.id
              JOIN repositories r ON r.organization_id = m.organization_id
              WHERE u.id = $1 AND r.id = $2 AND u.status = 'ACTIVE'
            )",
        )
        .bind(actor)
        .bind(repository)
        .fetch_one(&self.pool)
        .await
        .map_err(|_| BookError::AuthorizationUnavailable)
    }
}
