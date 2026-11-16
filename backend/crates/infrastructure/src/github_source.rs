use async_trait::async_trait;
use specspace_application::{
    book::BookError,
    comparison_reader::{BoundProviderFactory, ComparisonReadError, RepositoryBinding},
    reader::BookProviderFactory,
};
use specspace_git_adapter::{GitProvider, app_auth::GitHubAppKey};
use sqlx::{FromRow, PgPool};
use std::{
    collections::VecDeque,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;
use uuid::Uuid;

#[derive(FromRow)]
struct RepositoryConnection {
    remote_id: String,
    installation_id: String,
    default_branch: String,
}

struct Connection {
    repository: Uuid,
    remote_id: u64,
    installation_id: u64,
    expires_at: u64,
    provider: Arc<dyn GitProvider>,
}

pub struct PgGitHubSource {
    pool: PgPool,
    key: Option<Arc<GitHubAppKey>>,
    connections: Mutex<VecDeque<Connection>>,
    connecting: Mutex<()>,
}

impl PgGitHubSource {
    pub fn new(pool: PgPool, key: Option<Arc<GitHubAppKey>>) -> Self {
        Self {
            pool,
            key,
            connections: Mutex::new(VecDeque::new()),
            connecting: Mutex::new(()),
        }
    }

    async fn cached(
        &self,
        repository: Uuid,
        remote_id: u64,
        installation_id: u64,
    ) -> Option<Arc<dyn GitProvider>> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
        let mut connections = self.connections.lock().await;
        connections.retain(|entry| entry.expires_at > now.saturating_add(120));
        let index = connections.iter().position(|entry| {
            entry.repository == repository
                && entry.remote_id == remote_id
                && entry.installation_id == installation_id
        })?;
        let entry = connections.remove(index)?;
        let provider = entry.provider.clone();
        connections.push_back(entry);
        Some(provider)
    }
}

impl PgGitHubSource {
    async fn settings(&self, repository: Uuid) -> Result<RepositoryConnection, BookError> {
        sqlx::query_as::<_,RepositoryConnection>("SELECT remote_id,installation_id,default_branch FROM repositories WHERE id=$1 AND provider='GITHUB'")
            .bind(repository).fetch_optional(&self.pool).await.map_err(|_|BookError::AuthorizationUnavailable)?.ok_or(BookError::NotFound)
    }
    async fn connect(
        &self,
        repository: Uuid,
        settings: RepositoryConnection,
    ) -> Result<Arc<dyn GitProvider>, BookError> {
        let remote_id = settings
            .remote_id
            .parse::<u64>()
            .ok()
            .filter(|id| *id > 0)
            .ok_or(BookError::InvalidRepositoryConfiguration)?;
        let installation_id = settings
            .installation_id
            .parse::<u64>()
            .ok()
            .filter(|id| *id > 0)
            .ok_or(BookError::InvalidRepositoryConfiguration)?;
        if settings.default_branch != "main" {
            return Err(BookError::InvalidRepositoryConfiguration);
        }
        let key = self
            .key
            .as_ref()
            .ok_or(BookError::RepositoryNotConfigured)?;
        if let Some(provider) = self.cached(repository, remote_id, installation_id).await {
            return Ok(provider);
        }
        // Token issuance is serialized, without holding any database transaction.
        let _connecting = self.connecting.lock().await;
        if let Some(provider) = self.cached(repository, remote_id, installation_id).await {
            return Ok(provider);
        }
        let (provider, expires_at) = key
            .installation(installation_id, remote_id)
            .map_err(BookError::Provider)?
            .connect_repository()
            .await
            .map_err(BookError::Provider)?;
        let provider: Arc<dyn GitProvider> = Arc::new(provider);
        let mut connections = self.connections.lock().await;
        connections.push_back(Connection {
            repository,
            remote_id,
            installation_id,
            expires_at,
            provider: provider.clone(),
        });
        while connections.len() > 16 {
            connections.pop_front();
        }
        Ok(provider)
    }
}
#[async_trait]
impl BookProviderFactory for PgGitHubSource {
    async fn provider(&self, repository: Uuid) -> Result<Arc<dyn GitProvider>, BookError> {
        self.connect(repository, self.settings(repository).await?)
            .await
    }
}
#[async_trait]
impl BoundProviderFactory for PgGitHubSource {
    async fn provider_bound(
        &self,
        binding: &RepositoryBinding,
    ) -> Result<Arc<dyn GitProvider>, ComparisonReadError> {
        let settings = self
            .settings(binding.repository)
            .await
            .map_err(ComparisonReadError::Source)?;
        if settings.remote_id.parse::<u64>().ok() != Some(binding.remote_id)
            || settings.installation_id.parse::<u64>().ok() != Some(binding.installation_id)
            || settings.default_branch != "main"
        {
            return Err(ComparisonReadError::ContextChanged);
        }
        // Connect with the exact settings just matched, never another mutable
        // lookup. The caller separately rechecks current proposal authorization.
        self.connect(binding.repository, settings)
            .await
            .map_err(ComparisonReadError::Source)
    }
}
