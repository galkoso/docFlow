use openidconnect::{
    AccessTokenHash, AuthorizationCode, ClientId, ClientSecret, CsrfToken, EndpointMaybeSet,
    EndpointNotSet, EndpointSet, IssuerUrl, Nonce, OAuth2TokenResponse, PkceCodeChallenge,
    PkceCodeVerifier, RedirectUrl, TokenResponse,
    core::{CoreAuthenticationFlow, CoreClient, CoreProviderMetadata},
};
use std::time::Duration;

type DiscoveredClient = CoreClient<
    EndpointSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointMaybeSet,
    EndpointMaybeSet,
>;

#[derive(Clone, Copy)]
pub enum Deployment {
    Production,
    LocalDevelopment,
}

#[derive(Debug, PartialEq, Eq)]
pub enum OidcError {
    InvalidConfiguration,
    ProviderUnavailable,
    AuthenticationFailed,
}

/// Contains secrets and therefore deliberately has no Debug/Serialize support.
pub struct Oidc {
    issuer: IssuerUrl,
    client_id: ClientId,
    client_secret: ClientSecret,
    redirect: RedirectUrl,
    deployment: Deployment,
    http: openidconnect::reqwest::Client,
}

/// Server-side login material; persist nonce/verifier with a browser-bound state.
pub struct AuthorizationStart {
    pub redirect_url: String,
    pub state: String,
    pub nonce: String,
    pub pkce_verifier: String,
}

pub struct VerifiedIdentity {
    pub issuer: String,
    pub subject: String,
}

fn permitted_url(url: &openidconnect::url::Url, deployment: Deployment) -> bool {
    url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
        && (url.scheme() == "https"
            || (matches!(deployment, Deployment::LocalDevelopment)
                && url.scheme() == "http"
                && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))))
}

impl Oidc {
    pub fn new(
        issuer: &str,
        client_id: &str,
        client_secret: &str,
        redirect: &str,
        deployment: Deployment,
    ) -> Result<Self, OidcError> {
        let issuer = IssuerUrl::new(issuer.into()).map_err(|_| OidcError::InvalidConfiguration)?;
        let redirect =
            RedirectUrl::new(redirect.into()).map_err(|_| OidcError::InvalidConfiguration)?;
        if !permitted_url(issuer.url(), deployment)
            || !permitted_url(redirect.url(), deployment)
            || client_id.is_empty()
            || client_secret.is_empty()
        {
            return Err(OidcError::InvalidConfiguration);
        }
        let http = openidconnect::reqwest::Client::builder()
            .retry(openidconnect::reqwest::retry::never())
            .redirect(openidconnect::reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(15))
            .connect_timeout(Duration::from_secs(5))
            .build()
            .map_err(|_| OidcError::InvalidConfiguration)?;
        Ok(Self {
            issuer,
            client_id: ClientId::new(client_id.into()),
            client_secret: ClientSecret::new(client_secret.into()),
            redirect,
            deployment,
            http,
        })
    }

    async fn client(&self) -> Result<DiscoveredClient, OidcError> {
        // Fresh discovery/JWKS per login operation supports provider key rotation.
        // Normal session-authenticated requests never contact the identity provider.
        let metadata = CoreProviderMetadata::discover_async(self.issuer.clone(), &self.http)
            .await
            .map_err(|_| OidcError::ProviderUnavailable)?;
        if !permitted_url(metadata.authorization_endpoint().url(), self.deployment)
            || !permitted_url(metadata.jwks_uri().url(), self.deployment)
            || !metadata
                .token_endpoint()
                .is_some_and(|endpoint| permitted_url(endpoint.url(), self.deployment))
        {
            return Err(OidcError::InvalidConfiguration);
        }
        Ok(CoreClient::from_provider_metadata(
            metadata,
            self.client_id.clone(),
            Some(self.client_secret.clone()),
        )
        .set_redirect_uri(self.redirect.clone()))
    }

    pub async fn begin(&self) -> Result<AuthorizationStart, OidcError> {
        let client = self.client().await?;
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let (url, state, nonce) = client
            .authorize_url(
                CoreAuthenticationFlow::AuthorizationCode,
                CsrfToken::new_random,
                Nonce::new_random,
            )
            .set_pkce_challenge(challenge)
            .url();
        Ok(AuthorizationStart {
            redirect_url: url.into(),
            state: state.secret().clone(),
            nonce: nonce.secret().clone(),
            pkce_verifier: verifier.secret().clone(),
        })
    }

    /// Nonce and verifier must come from an atomically consumed LoginFlow record.
    pub async fn verify_code(
        &self,
        code: &str,
        nonce: String,
        pkce_verifier: String,
    ) -> Result<VerifiedIdentity, OidcError> {
        if code.is_empty()
            || code.len() > 4096
            || nonce.is_empty()
            || nonce.len() > 512
            || !(43..=128).contains(&pkce_verifier.len())
        {
            return Err(OidcError::AuthenticationFailed);
        }
        let client = self.client().await?;
        let response = client
            .exchange_code(AuthorizationCode::new(code.into()))
            .map_err(|_| OidcError::InvalidConfiguration)?
            .set_pkce_verifier(PkceCodeVerifier::new(pkce_verifier))
            .request_async(&self.http)
            .await
            .map_err(|_| OidcError::AuthenticationFailed)?;
        let token = response.id_token().ok_or(OidcError::AuthenticationFailed)?;
        let verifier = client.id_token_verifier();
        // The library checks signature, issuer, audience, expiry and nonce.
        let claims = token
            .claims(&verifier, &Nonce::new(nonce))
            .map_err(|_| OidcError::AuthenticationFailed)?;
        if claims.subject().as_str().is_empty() || claims.subject().as_str().len() > 255 {
            return Err(OidcError::AuthenticationFailed);
        }
        if let Some(expected) = claims.access_token_hash() {
            let actual = AccessTokenHash::from_token(
                response.access_token(),
                token
                    .signing_alg()
                    .map_err(|_| OidcError::AuthenticationFailed)?,
                token
                    .signing_key(&verifier)
                    .map_err(|_| OidcError::AuthenticationFailed)?,
            )
            .map_err(|_| OidcError::AuthenticationFailed)?;
            if actual != *expected {
                return Err(OidcError::AuthenticationFailed);
            }
        }
        Ok(VerifiedIdentity {
            issuer: self.issuer.as_str().into(),
            subject: claims.subject().as_str().into(),
        })
        // OAuth access/refresh tokens are dropped here; they are never browser state.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Form, Json, Router,
        extract::State,
        routing::{get, post},
    };
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
    use openidconnect::{JsonWebKeyId, PrivateSigningKey, core::CoreRsaPrivateSigningKey};
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};
    use std::{
        collections::HashMap,
        process::Command,
        sync::{
            Arc, Mutex,
            atomic::{AtomicU8, Ordering},
        },
    };

    struct IdentityProvider {
        origin: String,
        key: EncodingKey,
        public: Value,
        nonce: Mutex<String>,
        challenge: Mutex<String>,
        mode: AtomicU8,
    }

    async fn discovery(State(state): State<Arc<IdentityProvider>>) -> Json<Value> {
        Json(json!({
            "issuer": state.origin, "authorization_endpoint": format!("{}/authorize", state.origin),
            "token_endpoint": format!("{}/token", state.origin), "jwks_uri": format!("{}/jwks", state.origin),
            "response_types_supported": ["code"], "subject_types_supported": ["public"],
            "id_token_signing_alg_values_supported": ["RS256"],
            "token_endpoint_auth_methods_supported": ["client_secret_basic"],
            "code_challenge_methods_supported": ["S256"]
        }))
    }

    async fn jwks(State(state): State<Arc<IdentityProvider>>) -> Json<Value> {
        Json(json!({"keys": [state.public]}))
    }

    async fn token(
        State(state): State<Arc<IdentityProvider>>,
        Form(form): Form<HashMap<String, String>>,
    ) -> Json<Value> {
        assert_eq!(form["grant_type"], "authorization_code");
        assert_eq!(form["code"], "synthetic-code");
        assert_eq!(
            URL_SAFE_NO_PAD.encode(Sha256::digest(form["code_verifier"].as_bytes())),
            *state.challenge.lock().unwrap()
        );
        let mode = state.mode.load(Ordering::SeqCst);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let access_hash = URL_SAFE_NO_PAD.encode(&Sha256::digest(b"synthetic-access-token")[..16]);
        let claims = json!({
            "iss": if mode == 4 { "https://wrong-issuer.example" } else { &state.origin },
            "sub": "synthetic-subject", "aud": if mode == 2 { "wrong-client" } else { "synthetic-client" },
            "iat": now, "exp": if mode == 3 { now - 3600 } else { now + 300 },
            "nonce": if mode == 1 { "wrong-nonce".into() } else { state.nonce.lock().unwrap().clone() },
            "at_hash": if mode == 6 { "wrong-access-hash".into() } else { access_hash }
        });
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some("synthetic-key".into());
        let mut jwt = encode(&header, &claims, &state.key).unwrap();
        if mode == 5 {
            let parts: Vec<_> = jwt.split('.').collect();
            let mut tampered = claims;
            tampered["sub"] = json!("tampered-subject");
            jwt = format!(
                "{}.{}.{}",
                parts[0],
                URL_SAFE_NO_PAD.encode(serde_json::to_vec(&tampered).unwrap()),
                parts[2]
            );
        }
        Json(
            json!({"access_token": "synthetic-access-token", "token_type": "Bearer", "expires_in": 3600, "id_token": jwt}),
        )
    }

    #[tokio::test]
    async fn authorization_code_pkce_verifies_identity_and_rejects_invalid_tokens() {
        // Synthetic runtime-only key, never persisted or printed.
        let generated = Command::new("openssl")
            .args(["genrsa", "-traditional", "2048"])
            .output()
            .unwrap();
        assert!(generated.status.success());
        let private = String::from_utf8(generated.stdout).unwrap();
        let signing = CoreRsaPrivateSigningKey::from_pem(
            &private,
            Some(JsonWebKeyId::new("synthetic-key".into())),
        )
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(IdentityProvider {
            origin: origin.clone(),
            key: EncodingKey::from_rsa_pem(private.as_bytes()).unwrap(),
            public: serde_json::to_value(signing.as_verification_key()).unwrap(),
            nonce: Mutex::new(String::new()),
            challenge: Mutex::new(String::new()),
            mode: AtomicU8::new(0),
        });
        let router = Router::new()
            .route("/.well-known/openid-configuration", get(discovery))
            .route("/jwks", get(jwks))
            .route("/token", post(token))
            .with_state(state.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let oidc = Oidc::new(
            &origin,
            "synthetic-client",
            "synthetic-secret",
            "http://127.0.0.1:3000/api/v1/auth/callback",
            Deployment::LocalDevelopment,
        )
        .unwrap();
        let start = oidc.begin().await.unwrap();
        let url = openidconnect::url::Url::parse(&start.redirect_url).unwrap();
        let query: HashMap<_, _> = url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        assert_eq!(query["response_type"], "code");
        assert_eq!(query["code_challenge_method"], "S256");
        assert_eq!(query["state"], start.state);
        *state.nonce.lock().unwrap() = query["nonce"].clone();
        *state.challenge.lock().unwrap() = query["code_challenge"].clone();
        let identity = oidc
            .verify_code(
                "synthetic-code",
                start.nonce.clone(),
                start.pkce_verifier.clone(),
            )
            .await
            .unwrap();
        assert_eq!(identity.issuer, origin);
        assert_eq!(identity.subject, "synthetic-subject");
        for mode in 1..=6 {
            state.mode.store(mode, Ordering::SeqCst);
            assert!(
                matches!(
                    oidc.verify_code(
                        "synthetic-code",
                        start.nonce.clone(),
                        start.pkce_verifier.clone()
                    )
                    .await,
                    Err(OidcError::AuthenticationFailed)
                ),
                "accepted invalid token mode {mode}"
            );
        }
        server.abort();
    }

    #[test]
    fn production_requires_https_and_local_mode_allows_only_loopback_http() {
        assert!(
            Oidc::new(
                "http://127.0.0.1:1234",
                "client",
                "secret",
                "https://app.example/callback",
                Deployment::Production
            )
            .is_err()
        );
        assert!(
            Oidc::new(
                "http://internal.example",
                "client",
                "secret",
                "http://127.0.0.1/callback",
                Deployment::LocalDevelopment
            )
            .is_err()
        );
        assert!(
            Oidc::new(
                "https://idp.example",
                "client",
                "secret",
                "https://app.example/callback",
                Deployment::Production
            )
            .is_ok()
        );
    }
}
