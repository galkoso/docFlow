use crate::sessions::{SessionError, hash};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

/// Single-use OIDC secrets; never serialized or included in logs.
#[derive(FromRow)]
pub struct LoginFlow {
    pub nonce: String,
    pub pkce_verifier: String,
}

pub struct LoginFlowStore {
    pool: PgPool,
}

impl LoginFlowStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Returns the independent value for the HttpOnly browser-binding cookie.
    pub async fn begin(
        &self,
        state: &str,
        nonce: &str,
        pkce_verifier: &str,
    ) -> Result<String, SessionError> {
        if !(16..=200).contains(&state.len())
            || nonce.is_empty()
            || nonce.len() > 512
            || !(43..=128).contains(&pkce_verifier.len())
        {
            return Err(SessionError::InvalidInput);
        }
        let binding = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        sqlx::query(
            "INSERT INTO oidc_login_flows (state_hash, browser_binding_hash, nonce, pkce_verifier, expires_at)
            VALUES ($1, $2, $3, $4, now() + interval '5 minutes')",
        ).bind(hash(state)).bind(hash(&binding)).bind(nonce).bind(pkce_verifier)
            .execute(&self.pool).await.map_err(|_| SessionError::DatabaseUnavailable)?;
        Ok(binding)
    }

    /// Atomic consume: an incorrect browser binding cannot burn another login.
    pub async fn consume(
        &self,
        state: &str,
        browser_binding: &str,
    ) -> Result<Option<LoginFlow>, SessionError> {
        if !(16..=200).contains(&state.len()) || browser_binding.len() != 64 {
            return Ok(None);
        }
        sqlx::query_as::<_, LoginFlow>(
            "DELETE FROM oidc_login_flows
            WHERE state_hash = $1 AND browser_binding_hash = $2 AND expires_at > now()
            RETURNING nonce, pkce_verifier",
        )
        .bind(hash(state))
        .bind(hash(browser_binding))
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| SessionError::DatabaseUnavailable)
    }
}
