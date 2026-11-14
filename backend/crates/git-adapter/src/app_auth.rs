//! GitHub App credentials stay server-side; tokens are restricted to one repo.
use crate::{GitError, github::GitHub};
use chrono::DateTime;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use reqwest::{Client, header::HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Serialize, Deserialize)]
struct Claims {
    iat: u64,
    exp: u64,
    iss: String,
}

/// Deliberately does not implement Debug or Serialize.
pub struct GitHubApp {
    issuer: String,
    installation_id: u64,
    repository_id: u64,
    signing_key: EncodingKey,
    client: Client,
    api_base: String,
}

/// Reusable server credential. Installation/repository scope is chosen from DB.
pub struct GitHubAppKey {
    issuer: String,
    signing_key: EncodingKey,
    client: Client,
}

/// Deliberately does not expose the raw token through public accessors.
pub struct InstallationToken {
    token: String,
    expires_at: u64,
}

fn now() -> Result<u64, GitError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|time| time.as_secs())
        .map_err(|_| GitError::InvalidInput)
}

impl InstallationToken {
    pub fn github(&self, repository: &str) -> Result<GitHub, GitError> {
        if self.expires_at <= now()?.saturating_add(60) {
            return Err(GitError::Unauthorized);
        }
        GitHub::new(repository, &self.token)
    }

    pub fn expires_at(&self) -> u64 {
        self.expires_at
    }
}

impl GitHubAppKey {
    /// `private_pem` is resolved by the server's secret provider, never a client.
    pub fn from_pem(issuer: &str, private_pem: &[u8]) -> Result<Self, GitError> {
        if issuer.is_empty()
            || issuer.len() > 100
            || !issuer
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return Err(GitError::InvalidInput);
        }
        let signing_key =
            EncodingKey::from_rsa_pem(private_pem).map_err(|_| GitError::InvalidInput)?;
        let client = Client::builder()
            .timeout(Duration::from_secs(15))
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .user_agent("SpecSpace/0.1")
            .build()
            .map_err(|_| GitError::ProviderUnavailable)?;
        Ok(Self {
            issuer: issuer.into(),
            signing_key,
            client,
        })
    }

    pub fn installation(
        &self,
        installation_id: u64,
        repository_id: u64,
    ) -> Result<GitHubApp, GitError> {
        if installation_id == 0 || repository_id == 0 {
            return Err(GitError::InvalidInput);
        }
        Ok(GitHubApp {
            issuer: self.issuer.clone(),
            signing_key: self.signing_key.clone(),
            client: self.client.clone(),
            installation_id,
            repository_id,
            api_base: "https://api.github.com".into(),
        })
    }
}

impl GitHubApp {
    pub fn from_pem(
        issuer: &str,
        installation_id: u64,
        repository_id: u64,
        private_pem: &[u8],
    ) -> Result<Self, GitError> {
        GitHubAppKey::from_pem(issuer, private_pem)?.installation(installation_id, repository_id)
    }

    pub async fn connect_repository(&self) -> Result<(GitHub, u64), GitError> {
        let token = self.installation_token().await?;
        let mut authorization = HeaderValue::from_str(&format!("Bearer {}", token.token))
            .map_err(|_| GitError::InvalidInput)?;
        authorization.set_sensitive(true);
        let response = self
            .client
            .get(format!(
                "{}/repositories/{}",
                self.api_base, self.repository_id
            ))
            .header("authorization", authorization)
            .header("accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2026-03-10")
            .send()
            .await
            .map_err(|_| GitError::ProviderUnavailable)?;
        let metadata = GitHub::response(response, false).await?;
        if metadata["id"].as_u64() != Some(self.repository_id) {
            return Err(GitError::InvalidResponse);
        }
        let name = metadata["full_name"]
            .as_str()
            .ok_or(GitError::InvalidResponse)?;
        Ok((token.github(name)?, token.expires_at()))
    }

    pub async fn installation_token(&self) -> Result<InstallationToken, GitError> {
        let issued = now()?;
        let claims = Claims {
            iat: issued.saturating_sub(60),
            exp: issued + 540,
            iss: self.issuer.clone(),
        };
        let jwt = encode(&Header::new(Algorithm::RS256), &claims, &self.signing_key)
            .map_err(|_| GitError::InvalidInput)?;
        let mut authorization =
            HeaderValue::from_str(&format!("Bearer {jwt}")).map_err(|_| GitError::InvalidInput)?;
        authorization.set_sensitive(true);
        let response = self.client.post(format!("{}/app/installations/{}/access_tokens", self.api_base, self.installation_id))
            .header("authorization", authorization).header("accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2026-03-10")
            .json(&json!({
                "repository_ids": [self.repository_id],
                "permissions": {"contents": "write", "pull_requests": "write", "checks": "write", "metadata": "read"}
            })).send().await.map_err(|_| GitError::ProviderUnavailable)?;
        let value = GitHub::response(response, false).await?;
        let token = value["token"]
            .as_str()
            .filter(|token| !token.is_empty())
            .ok_or(GitError::InvalidResponse)?;
        let expiry = DateTime::parse_from_rfc3339(
            value["expires_at"]
                .as_str()
                .ok_or(GitError::InvalidResponse)?,
        )
        .map_err(|_| GitError::InvalidResponse)?
        .timestamp();
        let expires_at = u64::try_from(expiry).map_err(|_| GitError::InvalidResponse)?;
        if expires_at <= issued + 60 {
            return Err(GitError::Unauthorized);
        }
        Ok(InstallationToken {
            token: token.into(),
            expires_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        extract::State,
        http::HeaderMap,
        routing::{get, post},
    };
    use jsonwebtoken::{DecodingKey, Validation, decode};
    use std::{
        io::Write,
        process::{Command, Stdio},
        sync::Arc,
    };

    fn keys() -> (Vec<u8>, Vec<u8>) {
        // Ephemeral synthetic key: never stored in the repository or printed.
        let generated = Command::new("openssl")
            .args([
                "genpkey",
                "-algorithm",
                "RSA",
                "-pkeyopt",
                "rsa_keygen_bits:2048",
            ])
            .output()
            .unwrap();
        assert!(generated.status.success());
        let private = generated.stdout;
        let mut process = Command::new("openssl")
            .args(["pkey", "-pubout"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        process.stdin.take().unwrap().write_all(&private).unwrap();
        let public = process.wait_with_output().unwrap();
        assert!(public.status.success());
        (private, public.stdout)
    }

    async fn issue(
        State(public): State<Arc<Vec<u8>>>,
        headers: HeaderMap,
        Json(body): Json<serde_json::Value>,
    ) -> Json<serde_json::Value> {
        let jwt = headers["authorization"]
            .to_str()
            .unwrap()
            .strip_prefix("Bearer ")
            .unwrap();
        let token = decode::<Claims>(
            jwt,
            &DecodingKey::from_rsa_pem(&public).unwrap(),
            &Validation::new(Algorithm::RS256),
        )
        .unwrap();
        assert_eq!(token.claims.iss, "Iv1SyntheticApp");
        assert!(token.claims.iat <= now().unwrap());
        assert!(token.claims.exp <= now().unwrap() + 600);
        assert_eq!(body["repository_ids"], json!([42]));
        assert_eq!(body["permissions"]["contents"], "write");
        assert!(body["permissions"].get("administration").is_none());
        Json(
            json!({"token": "synthetic-installation-token", "expires_at": chrono::Utc::now().checked_add_signed(chrono::Duration::hours(1)).unwrap().to_rfc3339()}),
        )
    }

    async fn repository(headers: HeaderMap) -> Json<serde_json::Value> {
        assert_eq!(
            headers["authorization"],
            "Bearer synthetic-installation-token"
        );
        Json(json!({"id": 42, "full_name": "synthetic/book"}))
    }

    #[tokio::test]
    async fn signed_app_jwt_requests_one_repository_and_bounded_permissions() {
        let (private, public) = keys();
        let mut app = GitHubApp::from_pem("Iv1SyntheticApp", 7, 42, &private).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        app.api_base = format!("http://{}", listener.local_addr().unwrap());
        let service = Router::new()
            .route("/app/installations/7/access_tokens", post(issue))
            .route("/repositories/42", get(repository))
            .with_state(Arc::new(public));
        let server = tokio::spawn(async move {
            axum::serve(listener, service).await.unwrap();
        });
        let token = app.installation_token().await.unwrap();
        assert!(token.github("synthetic/book").is_ok());
        assert!(token.expires_at() > now().unwrap());
        assert!(app.connect_repository().await.is_ok());
        server.abort();
    }

    #[tokio::test]
    async fn mismatched_repository_metadata_cannot_redirect_an_installation_to_another_repo() {
        let (private, public) = keys();
        let key = GitHubAppKey::from_pem("Iv1SyntheticApp", &private).unwrap();
        let mut app = key.installation(7, 42).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        app.api_base = format!("http://{}", listener.local_addr().unwrap());
        let service = Router::new()
            .route("/app/installations/7/access_tokens", post(issue))
            .route(
                "/repositories/42",
                get(|| async { Json(json!({"id": 99, "full_name": "other/repo"})) }),
            )
            .with_state(Arc::new(public));
        let server = tokio::spawn(async move {
            axum::serve(listener, service).await.unwrap();
        });
        assert!(matches!(
            app.connect_repository().await,
            Err(GitError::InvalidResponse)
        ));
        server.abort();
    }

    #[test]
    fn expired_tokens_cannot_construct_a_provider_client() {
        let expired = InstallationToken {
            token: "synthetic-expired-token".into(),
            expires_at: 1,
        };
        assert!(matches!(
            expired.github("synthetic/book"),
            Err(GitError::Unauthorized)
        ));
        assert!(GitHubApp::from_pem("app", 1, 42, b"invalid-key").is_err());
    }
}
