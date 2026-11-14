use sha2::{Digest, Sha256};
use sqlx::{FromRow, PgPool};
use subtle::ConstantTimeEq;
use uuid::Uuid;

#[derive(Debug, PartialEq, Eq)]
pub enum SessionError {
    InvalidInput,
    Unauthorized,
    DatabaseUnavailable,
}

/// Plaintext credentials exist only for delivery to the browser as cookies.
/// This type deliberately has no Debug or Serialize implementation.
pub struct SessionCredentials {
    token: String,
    csrf: String,
}

impl SessionCredentials {
    pub fn session_cookie_value(&self) -> &str {
        &self.token
    }
    pub fn csrf_cookie_value(&self) -> &str {
        &self.csrf
    }
}

#[derive(FromRow)]
pub struct SessionIdentity {
    pub session_id: Uuid,
    pub user_id: Uuid,
    csrf_hash: Vec<u8>,
}

impl SessionIdentity {
    pub fn verify_csrf(&self, supplied: &str) -> bool {
        valid_secret(supplied) && bool::from(self.csrf_hash.as_slice().ct_eq(&hash(supplied)))
    }
}

fn secret() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}
pub(crate) fn hash(value: &str) -> Vec<u8> {
    Sha256::digest(value.as_bytes()).to_vec()
}
fn valid_secret(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub struct SessionStore {
    pool: PgPool,
}

impl SessionStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn mint(
        &self,
        user_id: Uuid,
        ttl_seconds: u32,
    ) -> Result<SessionCredentials, SessionError> {
        if !(60..=86400).contains(&ttl_seconds) {
            return Err(SessionError::InvalidInput);
        }
        let credentials = SessionCredentials {
            token: secret(),
            csrf: secret(),
        };
        let result = sqlx::query(
            "INSERT INTO sessions (id, user_id, token_hash, csrf_hash, expires_at)
            SELECT $1, u.id, $3, $4, now() + make_interval(secs => $5)
            FROM users u WHERE u.id = $2 AND u.status = 'ACTIVE'
              AND EXISTS (SELECT 1 FROM memberships m WHERE m.user_id = u.id)",
        )
        .bind(Uuid::new_v4())
        .bind(user_id)
        .bind(hash(&credentials.token))
        .bind(hash(&credentials.csrf))
        .bind(f64::from(ttl_seconds))
        .execute(&self.pool)
        .await
        .map_err(|_| SessionError::DatabaseUnavailable)?;
        if result.rows_affected() != 1 {
            return Err(SessionError::Unauthorized);
        }
        Ok(credentials)
    }

    pub async fn authenticate(&self, token: &str) -> Result<Option<SessionIdentity>, SessionError> {
        if !valid_secret(token) {
            return Ok(None);
        }
        sqlx::query_as::<_, SessionIdentity>(
            "SELECT s.id AS session_id, s.user_id, s.csrf_hash FROM sessions s
            JOIN users u ON u.id = s.user_id
            WHERE s.token_hash = $1 AND s.expires_at > now() AND u.status = 'ACTIVE'
              AND EXISTS (SELECT 1 FROM memberships m WHERE m.user_id = u.id)",
        )
        .bind(hash(token))
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| SessionError::DatabaseUnavailable)
    }

    pub async fn revoke(&self, token: &str) -> Result<(), SessionError> {
        if !valid_secret(token) {
            return Ok(());
        }
        sqlx::query("DELETE FROM sessions WHERE token_hash = $1")
            .bind(hash(token))
            .execute(&self.pool)
            .await
            .map_err(|_| SessionError::DatabaseUnavailable)?;
        Ok(())
    }
}
