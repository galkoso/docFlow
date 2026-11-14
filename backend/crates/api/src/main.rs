use specspace_api::auth::CookiePolicy;
use specspace_api::{AppState, router};
use specspace_infrastructure::oidc::{Deployment, Oidc};
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;
use std::{net::SocketAddr, time::Duration};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let address: SocketAddr = std::env::var("APP_LISTEN_ADDRESS")
        .unwrap_or_else(|_| "127.0.0.1:3000".into())
        .parse()?;
    let database_url = std::env::var("DATABASE_URL")?;
    let development = match std::env::var("APP_ENV")
        .unwrap_or_else(|_| "production".into())
        .as_str()
    {
        "production" => false,
        "development" => true,
        _ => return Err("APP_ENV must be production or development".into()),
    };
    if development && !address.ip().is_loopback() {
        return Err("Development mode must bind to loopback".into());
    }
    let database = PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(Duration::from_secs(3))
        .connect_lazy(&database_url)?;
    let listener = tokio::net::TcpListener::bind(address).await?;
    let mut state = AppState::new(database);
    let public_url = std::env::var("APP_PUBLIC_URL").ok();
    if let Some(public_url) = &public_url {
        state.cookies = CookiePolicy::new(public_url, development)?;
    }
    if let Ok(issuer) = std::env::var("OIDC_ISSUER") {
        let public_url = public_url.ok_or("APP_PUBLIC_URL is required for OIDC")?;
        let client_id = std::env::var("OIDC_CLIENT_ID")?;
        let client_secret = std::env::var("OIDC_CLIENT_SECRET")?;
        let redirect = format!("{}/api/v1/auth/callback", public_url.trim_end_matches('/'));
        let deployment = if development {
            Deployment::LocalDevelopment
        } else {
            Deployment::Production
        };
        state.oidc = Some(Arc::new(
            Oidc::new(&issuer, &client_id, &client_secret, &redirect, deployment)
                .map_err(|_| "Invalid OIDC configuration")?,
        ));
    }
    tracing::info!(%address, "API listening");
    axum::serve(listener, router(state))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
