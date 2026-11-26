use super::*;
use sqlx::postgres::PgPoolOptions;

fn lazy_source() -> PgGitHubSource {
    PgGitHubSource::new(
        PgPoolOptions::new()
            .connect_lazy("postgres://unused:unused@127.0.0.1:1/unused")
            .unwrap(),
        None,
    )
}

#[tokio::test]
async fn cache_never_crosses_repository_or_installation_and_evicts_near_expiry() {
    let source = lazy_source();
    let repository = Uuid::new_v4();
    // No provider IO occurs in this test; production entries are inserted only
    // after verified GitHub App connection, never from name-only constructors.
    let provider = Arc::new(GitHub::new("synthetic/specs", "synthetic-token").unwrap());
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    source.connections.lock().await.push_back(Connection {
        repository,
        remote_id: 42,
        installation_id: 7,
        expires_at: now + 3600,
        provider: provider.clone(),
    });
    assert!(source.cached(Uuid::new_v4(), 42, 7).await.is_none());
    assert!(source.cached(repository, 43, 7).await.is_none());
    assert!(source.cached(repository, 42, 8).await.is_none());
    assert!(Arc::ptr_eq(
        &source.cached(repository, 42, 7).await.unwrap(),
        &provider
    ));
    source.connections.lock().await[0].expires_at = now + 30;
    assert!(source.cached(repository, 42, 7).await.is_none());
    assert!(source.connections.lock().await.is_empty());
}

#[tokio::test]
async fn malformed_frozen_binding_fails_without_database_or_provider_access() {
    let source = lazy_source();
    for binding in [
        RepositoryBinding {
            repository: Uuid::nil(),
            remote_id: 42,
            installation_id: 7,
        },
        RepositoryBinding {
            repository: Uuid::new_v4(),
            remote_id: 0,
            installation_id: 7,
        },
        RepositoryBinding {
            repository: Uuid::new_v4(),
            remote_id: 42,
            installation_id: 0,
        },
    ] {
        assert!(matches!(
            source.pr_provider_bound(&binding).await,
            Err(PrProviderError::BindingChanged)
        ));
        assert!(matches!(
            source.provider_bound(&binding).await,
            Err(ComparisonReadError::ContextChanged)
        ));
        assert!(matches!(
            source.check_provider_bound(&binding).await,
            Err(CheckProviderError::BindingChanged)
        ));
    }
}

#[cfg(feature = "postgres-tests")]
#[sqlx::test(migrations = "../../migrations")]
async fn frozen_binding_is_rechecked_before_credential_selection(pool: PgPool) {
    let org = Uuid::new_v4();
    let repository = Uuid::new_v4();
    sqlx::query("INSERT INTO organizations(id,name) VALUES($1,'Synthetic PR factory')")
        .bind(org)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO repositories(id,organization_id,provider,remote_id,installation_id) VALUES($1,$2,'GITHUB','42','7')")
        .bind(repository).bind(org).execute(&pool).await.unwrap();
    let binding = RepositoryBinding {
        repository,
        remote_id: 42,
        installation_id: 7,
    };
    let source = PgGitHubSource::new(pool.clone(), None);
    // Exact configuration reaches credential selection; no key means no IO.
    assert!(matches!(
        source.pr_provider_bound(&binding).await,
        Err(PrProviderError::Source(BookError::RepositoryNotConfigured))
    ));
    assert!(matches!(
        source.check_provider_bound(&binding).await,
        Err(CheckProviderError::Source(
            BookError::RepositoryNotConfigured
        ))
    ));
    for (remote, installation, branch) in [
        ("43", "7", "main"),
        ("42", "8", "main"),
        ("42", "7", "other"),
    ] {
        sqlx::query(
            "UPDATE repositories SET remote_id=$2,installation_id=$3,default_branch=$4 WHERE id=$1",
        )
        .bind(repository)
        .bind(remote)
        .bind(installation)
        .bind(branch)
        .execute(&pool)
        .await
        .unwrap();
        assert!(matches!(
            source.pr_provider_bound(&binding).await,
            Err(PrProviderError::BindingChanged)
        ));
        assert!(matches!(
            source.provider_bound(&binding).await,
            Err(ComparisonReadError::ContextChanged)
        ));
        assert!(matches!(
            source.check_provider_bound(&binding).await,
            Err(CheckProviderError::BindingChanged)
        ));
    }
    sqlx::query("DELETE FROM repositories WHERE id=$1")
        .bind(repository)
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        source.pr_provider_bound(&binding).await,
        Err(PrProviderError::Source(BookError::NotFound))
    ));
    assert!(matches!(
        source.check_provider_bound(&binding).await,
        Err(CheckProviderError::Source(BookError::NotFound))
    ));
}
