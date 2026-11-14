use specspace_application::book::ReadAuthorization;
use specspace_application::{book::BookError, reader::BookReader};
use specspace_infrastructure::authorization::PgReadAuthorization;
use specspace_infrastructure::github_source::PgGitHubSource;
use specspace_infrastructure::login_flows::LoginFlowStore;
use specspace_infrastructure::sessions::SessionStore;
use specspace_infrastructure::{complete_job, lease_job};
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

async fn organization(pool: &PgPool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO organizations (id, name) VALUES ($1, 'Synthetic test')")
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    id
}

#[sqlx::test(migrations = "../../migrations")]
async fn concurrent_workers_cannot_claim_same_job(pool: PgPool) {
    let organization_id = organization(&pool).await;
    let job_id = Uuid::new_v4();
    sqlx::query("INSERT INTO jobs (id, organization_id, kind) VALUES ($1, $2, 'INDEX')")
        .bind(job_id)
        .bind(organization_id)
        .execute(&pool)
        .await
        .unwrap();
    let worker_a = Uuid::new_v4();
    let worker_b = Uuid::new_v4();
    let (a, b) = tokio::join!(
        lease_job(&pool, worker_a, 30),
        lease_job(&pool, worker_b, 30)
    );
    let a = a.unwrap();
    let b = b.unwrap();
    assert_ne!(a.is_some(), b.is_some());
    let owner = if a.is_some() { worker_a } else { worker_b };
    let other = if a.is_some() { worker_b } else { worker_a };
    assert!(
        !complete_job(&pool, job_id, other, serde_json::json!({}))
            .await
            .unwrap()
    );
    assert!(
        complete_job(&pool, job_id, owner, serde_json::json!({"indexed": true}))
            .await
            .unwrap()
    );
    assert!(
        !complete_job(&pool, job_id, owner, serde_json::json!({}))
            .await
            .unwrap()
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn expired_side_effect_requires_reconciliation(pool: PgPool) {
    let organization_id = organization(&pool).await;
    let job_id = Uuid::new_v4();
    let owner = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO jobs (id, organization_id, kind, state, lease_owner, leased_until)
      VALUES ($1, $2, 'MERGE', 'RUNNING', $3, now() - interval '1 second')",
    )
    .bind(job_id)
    .bind(organization_id)
    .bind(owner)
    .execute(&pool)
    .await
    .unwrap();
    assert!(
        lease_job(&pool, Uuid::new_v4(), 30)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        !complete_job(&pool, job_id, owner, serde_json::json!({}))
            .await
            .unwrap()
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn audit_is_append_only(pool: PgPool) {
    let organization_id = organization(&pool).await;
    sqlx::query(
        "INSERT INTO audit_events (id, organization_id, action, entity_id, result)
      VALUES ($1, $2, 'TEST', $1, 'SUCCEEDED')",
    )
    .bind(Uuid::new_v4())
    .bind(organization_id)
    .execute(&pool)
    .await
    .unwrap();
    for statement in [
        "UPDATE audit_events SET action = 'CHANGED'",
        "DELETE FROM audit_events",
        "TRUNCATE audit_events",
    ] {
        let error = sqlx::query(statement).execute(&pool).await.unwrap_err();
        assert!(error.to_string().contains("append-only"));
    }
}

async fn proposal_fixture(pool: &PgPool) -> (Uuid, Uuid, Uuid, Uuid) {
    let organization_id = organization(pool).await;
    let user_id = Uuid::new_v4();
    let team_id = Uuid::new_v4();
    let repo_id = Uuid::new_v4();
    sqlx::query("INSERT INTO users (id, issuer, external_subject, display_name) VALUES ($1, 'test', $1::text, 'Test')")
        .bind(user_id).execute(pool).await.unwrap();
    sqlx::query(
        "INSERT INTO memberships (organization_id, user_id, role) VALUES ($1, $2, 'CONTRIBUTOR')",
    )
    .bind(organization_id)
    .bind(user_id)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO teams (id, organization_id, name) VALUES ($1, $2, 'Test')")
        .bind(team_id)
        .bind(organization_id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO repositories (id, organization_id, provider, remote_id, installation_id)
      VALUES ($1, $2, 'GITHUB', 'test-repo', 'test-installation')",
    )
    .bind(repo_id)
    .bind(organization_id)
    .execute(pool)
    .await
    .unwrap();
    (organization_id, user_id, team_id, repo_id)
}

async fn insert_proposal(
    pool: &PgPool,
    ids: (Uuid, Uuid, Uuid, Uuid),
    branch: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO proposals (id, organization_id, repository_id, title, branch,
      owner_id, author_id, team_id, description, ui_impact)
      VALUES ($1, $2, $3, 'Test proposal', $4, $5, $5, $6, 'Synthetic test description', 'YES')",
    )
    .bind(Uuid::new_v4())
    .bind(ids.0)
    .bind(ids.3)
    .bind(branch)
    .bind(ids.1)
    .bind(ids.2)
    .execute(pool)
    .await?;
    Ok(())
}

#[sqlx::test(migrations = "../../migrations")]
async fn branch_reservation_is_case_insensitive_and_survives_close(pool: PgPool) {
    let ids = proposal_fixture(&pool).await;
    let (a, b) = tokio::join!(
        insert_proposal(&pool, ids, "change/1-Feature"),
        insert_proposal(&pool, ids, "change/1-feature"),
    );
    assert_ne!(a.is_ok(), b.is_ok());
    let error = a.err().or_else(|| b.err()).unwrap();
    assert_eq!(
        error.as_database_error().unwrap().code().as_deref(),
        Some("23505")
    );
    sqlx::query("UPDATE proposals SET state = 'CLOSED', head_sha = 'head', base_sha = 'base', target_sha = 'target'")
        .execute(&pool).await.unwrap();
    assert!(
        insert_proposal(&pool, ids, "change/1-FEATURE")
            .await
            .is_err()
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn proposal_cannot_reference_team_from_another_organization(pool: PgPool) {
    let mut a = proposal_fixture(&pool).await;
    let b = proposal_fixture(&pool).await;
    a.2 = b.2;
    let error = insert_proposal(&pool, a, "change/1-cross-org")
        .await
        .unwrap_err();
    assert_eq!(
        error.as_database_error().unwrap().code().as_deref(),
        Some("23503")
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn operation_idempotency_rejects_duplicate_actor_kind_and_key(pool: PgPool) {
    let ids = proposal_fixture(&pool).await;
    let insert = "INSERT INTO git_operations (id, organization_id, actor_id, kind, idempotency_key, request_hash)
      VALUES ($1, $2, $3, 'CREATE', 'same-request', 'content-hash')";
    sqlx::query(insert)
        .bind(Uuid::new_v4())
        .bind(ids.0)
        .bind(ids.1)
        .execute(&pool)
        .await
        .unwrap();
    let error = sqlx::query(insert)
        .bind(Uuid::new_v4())
        .bind(ids.0)
        .bind(ids.1)
        .execute(&pool)
        .await
        .unwrap_err();
    assert_eq!(
        error.as_database_error().unwrap().code().as_deref(),
        Some("23505")
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn repository_authorization_uses_current_membership_and_tenant_scope(pool: PgPool) {
    let a = proposal_fixture(&pool).await;
    let b = proposal_fixture(&pool).await;
    let authorization = PgReadAuthorization::new(pool.clone());
    assert!(authorization.can_read(a.1, a.3).await.unwrap());
    assert!(!authorization.can_read(a.1, b.3).await.unwrap());
    sqlx::query("UPDATE users SET status = 'DISABLED' WHERE id = $1")
        .bind(a.1)
        .execute(&pool)
        .await
        .unwrap();
    assert!(!authorization.can_read(a.1, a.3).await.unwrap());
    sqlx::query("UPDATE users SET status = 'ACTIVE' WHERE id = $1")
        .bind(a.1)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM memberships WHERE user_id = $1")
        .bind(a.1)
        .execute(&pool)
        .await
        .unwrap();
    assert!(!authorization.can_read(a.1, a.3).await.unwrap());
}

#[sqlx::test(migrations = "../../migrations")]
async fn session_tokens_are_hashed_and_csrf_is_bound_to_the_session(pool: PgPool) {
    let ids = proposal_fixture(&pool).await;
    let sessions = SessionStore::new(pool.clone());
    let credentials = sessions.mint(ids.1, 3600).await.unwrap();
    let identity = sessions
        .authenticate(credentials.session_cookie_value())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(identity.user_id, ids.1);
    assert!(identity.verify_csrf(credentials.csrf_cookie_value()));
    assert!(!identity.verify_csrf(credentials.session_cookie_value()));
    let other = sessions.mint(ids.1, 3600).await.unwrap();
    assert!(!identity.verify_csrf(other.csrf_cookie_value()));
    let stored: Vec<u8> = sqlx::query_scalar("SELECT token_hash FROM sessions WHERE id = $1")
        .bind(identity.session_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored.len(), 32);
    assert_ne!(stored, credentials.session_cookie_value().as_bytes());
    sessions
        .revoke(credentials.session_cookie_value())
        .await
        .unwrap();
    assert!(
        sessions
            .authenticate(credentials.session_cookie_value())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        sessions
            .authenticate(other.session_cookie_value())
            .await
            .unwrap()
            .is_some()
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn expiration_and_disabled_users_invalidate_existing_sessions(pool: PgPool) {
    let ids = proposal_fixture(&pool).await;
    let sessions = SessionStore::new(pool.clone());
    let expired = sessions.mint(ids.1, 3600).await.unwrap();
    sqlx::query("UPDATE sessions SET expires_at = now() - interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        sessions
            .authenticate(expired.session_cookie_value())
            .await
            .unwrap()
            .is_none()
    );
    let active = sessions.mint(ids.1, 3600).await.unwrap();
    sqlx::query("UPDATE users SET status = 'DISABLED' WHERE id = $1")
        .bind(ids.1)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        sessions
            .authenticate(active.session_cookie_value())
            .await
            .unwrap()
            .is_none()
    );
    assert!(sessions.mint(ids.1, 3600).await.is_err());
    assert!(
        sessions
            .authenticate("invalid-input")
            .await
            .unwrap()
            .is_none()
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn login_state_requires_its_browser_binding_and_can_be_used_only_once(pool: PgPool) {
    let flows = LoginFlowStore::new(pool.clone());
    let state = "synthetic-state-token-at-least-sixteen";
    let verifier = "v".repeat(43);
    let binding = flows
        .begin(state, "synthetic-nonce", &verifier)
        .await
        .unwrap();
    assert!(
        flows
            .consume(state, &"0".repeat(64))
            .await
            .unwrap()
            .is_none()
    );
    let (a, b) = tokio::join!(
        flows.consume(state, &binding),
        flows.consume(state, &binding)
    );
    let a = a.unwrap();
    let b = b.unwrap();
    assert_ne!(a.is_some(), b.is_some());
    let consumed = a.or(b).unwrap();
    assert_eq!(consumed.nonce, "synthetic-nonce");
    assert_eq!(consumed.pkce_verifier, verifier);
    assert!(flows.consume(state, &binding).await.unwrap().is_none());
    let expired_binding = flows
        .begin("another-synthetic-state-token", "nonce", &verifier)
        .await
        .unwrap();
    sqlx::query("UPDATE oidc_login_flows SET expires_at = now() - interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        flows
            .consume("another-synthetic-state-token", &expired_binding)
            .await
            .unwrap()
            .is_none()
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn repository_configuration_is_db_selected_and_foreign_ids_stay_hidden(pool: PgPool) {
    let a = proposal_fixture(&pool).await;
    let b = proposal_fixture(&pool).await;
    sqlx::query("UPDATE repositories SET remote_id = '42', installation_id = '7'")
        .execute(&pool)
        .await
        .unwrap();
    let reader = BookReader::new(
        Arc::new(PgReadAuthorization::new(pool.clone())),
        Arc::new(PgGitHubSource::new(pool.clone(), None)),
    );
    assert!(matches!(
        reader.main(a.1, b.3).await,
        Err(BookError::NotFound)
    ));
    assert!(matches!(
        reader.main(a.1, a.3).await,
        Err(BookError::RepositoryNotConfigured)
    ));
    sqlx::query("UPDATE repositories SET default_branch = 'other' WHERE id = $1")
        .bind(a.3)
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        reader.main(a.1, a.3).await,
        Err(BookError::InvalidRepositoryConfiguration)
    ));
}
