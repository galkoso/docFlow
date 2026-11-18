use specspace_api::auth::CookiePolicy;
use specspace_api::{AppState, router};
use specspace_git_adapter::app_auth::GitHubAppKey;
use specspace_infrastructure::oidc::{Deployment, Oidc};
use specspace_infrastructure::secrets;
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
    if let Ok(days) = std::env::var("DRAFT_RETENTION_DAYS") {
        let retention = specspace_application::draft::DraftRetention::new(days.parse()?)
            .map_err(|_| "DRAFT_RETENTION_DAYS must be between 1 and 365")?;
        state.configure_draft_retention(retention);
        state
            .drafts
            .enforce_retention()
            .await
            .map_err(|_| "Unable to apply the configured draft retention policy")?;
    }
    if let Ok(provider) = std::env::var("GIT_PROVIDER")
        && !provider.eq_ignore_ascii_case("github")
    {
        return Err("Only GitHub is supported in v1".into());
    }
    match (
        std::env::var("GITHUB_APP_ID").ok(),
        std::env::var("GITHUB_PRIVATE_KEY_SECRET_REF").ok(),
    ) {
        (None, None) => {}
        (Some(issuer), Some(reference)) => {
            let secret = secrets::resolve(&reference)
                .map_err(|_| "Unable to resolve the configured GitHub App key")?;
            let key = GitHubAppKey::from_pem(&issuer, secret.as_bytes())
                .map_err(|_| "Invalid GitHub App key configuration")?;
            state.configure_github(key);
        }
        _ => {
            return Err(
                "GITHUB_APP_ID and GITHUB_PRIVATE_KEY_SECRET_REF must be configured together"
                    .into(),
            );
        }
    }
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
    let index_timeout = std::env::var("INDEX_WORKER_TIMEOUT_SECONDS")
        .unwrap_or_else(|_| "900".into())
        .parse::<u64>()?;
    let workers = (0..2)
        .map(|_| state.index_worker(Duration::from_secs(index_timeout)))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "INDEX_WORKER_TIMEOUT_SECONDS must be between 30 and 3600")?;
    let proposal_timeout = std::env::var("PROPOSAL_WORKER_TIMEOUT_SECONDS")
        .unwrap_or_else(|_| "120".into())
        .parse::<u64>()?;
    let proposal_worker = state
        .proposal_worker(Duration::from_secs(proposal_timeout))
        .map_err(|_| "PROPOSAL_WORKER_TIMEOUT_SECONDS must be between 30 and 3600")?;
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let save_timeout = std::env::var("SAVE_WORKER_TIMEOUT_SECONDS")
        .unwrap_or_else(|_| "900".into())
        .parse::<u64>()?;
    let save_worker = state
        .save_worker(Duration::from_secs(save_timeout))
        .map_err(|_| "SAVE_WORKER_TIMEOUT_SECONDS must be between 30 and 3600")?;
    let comparison_timeout = std::env::var("COMPARISON_WORKER_TIMEOUT_SECONDS")
        .unwrap_or_else(|_| "900".into())
        .parse::<u64>()?;
    let comparison_worker = state
        .comparison_worker(Duration::from_secs(comparison_timeout))
        .map_err(|_| "COMPARISON_WORKER_TIMEOUT_SECONDS must be between 30 and 900")?;
    let review_timeout = std::env::var("REVIEW_WORKER_TIMEOUT_SECONDS")
        .unwrap_or_else(|_| "900".into())
        .parse::<u64>()?;
    let review_worker = state
        .review_worker(Duration::from_secs(review_timeout))
        .map_err(|_| "REVIEW_WORKER_TIMEOUT_SECONDS must be between 30 and 900")?;
    let pr_timeout = std::env::var("PR_SYNC_WORKER_TIMEOUT_SECONDS")
        .unwrap_or_else(|_| "120".into())
        .parse::<u64>()?;
    let pr_worker = state
        .pr_sync_worker(Duration::from_secs(pr_timeout))
        .map_err(|_| "PR_SYNC_WORKER_TIMEOUT_SECONDS must be between 30 and 900")?;
    let mut handles: Vec<_> = workers
        .into_iter()
        .map(|worker| {
            let stopped = stopped.clone();
            tokio::spawn(async move { worker.run(stopped).await })
        })
        .collect();
    let comparison_stopped = stopped.clone();
    handles.push(tokio::spawn(async move {
        comparison_worker.run(comparison_stopped).await
    }));
    let review_stopped = stopped.clone();
    handles.push(tokio::spawn(async move {
        review_worker.run(review_stopped).await
    }));
    let pr_stopped = stopped.clone();
    handles.push(tokio::spawn(async move { pr_worker.run(pr_stopped).await }));
    let draft_stopped = stopped.clone();
    let save_stopped = stopped.clone();
    handles.push(tokio::spawn(
        async move { save_worker.run(save_stopped).await },
    ));
    let drafts = state.drafts.clone();
    handles.push(tokio::spawn(async move {
        drafts.run_cleanup(draft_stopped).await
    }));
    handles.push(tokio::spawn(
        async move { proposal_worker.run(stopped).await },
    ));
    let shutdown_stop = stop.clone();
    tracing::info!(%address, index_workers = 2, proposal_workers = 1, save_workers=1, comparison_workers=1, review_workers=1, pr_sync_workers=1, "API listening");
    let result = axum::serve(listener, router(state))
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            let _ = shutdown_stop.send(true);
        })
        .await;
    let _ = stop.send(true);
    for handle in handles {
        handle.await?;
    }
    result?;
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut terminate) => tokio::select! {
                _ = tokio::signal::ctrl_c() => {},
                _ = terminate.recv() => {},
            },
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
