use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use specspace_api::{AppState, auth::CookiePolicy, router};
use specspace_infrastructure::sessions::SessionStore;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

async fn user(pool: &PgPool) -> Uuid {
    let user = Uuid::new_v4();
    let org = Uuid::new_v4();
    sqlx::query("INSERT INTO organizations (id, name) VALUES ($1, 'HTTP fixture')")
        .bind(org)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO users (id, issuer, external_subject, display_name) VALUES ($1, 'test', $1::text, 'Synthetic Reader')").bind(user).execute(pool).await.unwrap();
    sqlx::query(
        "INSERT INTO memberships (organization_id, user_id, role) VALUES ($1, $2, 'READER')",
    )
    .bind(org)
    .bind(user)
    .execute(pool)
    .await
    .unwrap();
    user
}

#[sqlx::test(migrations = "../../migrations")]
async fn me_uses_server_session_and_ignores_client_identity_headers(pool: PgPool) {
    let user = user(&pool).await;
    let credentials = SessionStore::new(pool.clone())
        .mint(user, 3600)
        .await
        .unwrap();
    let app = router(AppState::new(pool));
    let unauthenticated = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/me")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/me")
                .header(
                    "Cookie",
                    format!(
                        "__Host-specspace-session={}",
                        credentials.session_cookie_value()
                    ),
                )
                .header("X-User-Id", Uuid::new_v4().to_string())
                .header("X-Role", "ADMIN")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["user_id"], user.to_string());
    assert_eq!(body["memberships"][0]["role"], "READER");
}

#[sqlx::test(migrations = "../../migrations")]
async fn logout_requires_origin_and_csrf_then_revokes_only_the_current_session(pool: PgPool) {
    let user = user(&pool).await;
    let sessions = SessionStore::new(pool.clone());
    let credentials = sessions.mint(user, 3600).await.unwrap();
    let other = sessions.mint(user, 3600).await.unwrap();
    let mut state = AppState::new(pool);
    state.cookies = CookiePolicy::new("http://localhost:4200", true).unwrap();
    let app = router(state);
    for (origin, csrf, status) in [
        ("http://localhost:4200", "", StatusCode::FORBIDDEN),
        (
            "https://attacker.example",
            credentials.csrf_cookie_value(),
            StatusCode::FORBIDDEN,
        ),
        (
            "http://localhost:4200",
            credentials.csrf_cookie_value(),
            StatusCode::NO_CONTENT,
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/logout")
                    .header(
                        "Cookie",
                        format!("specspace-session={}", credentials.session_cookie_value()),
                    )
                    .header("Origin", origin)
                    .header("X-XSRF-TOKEN", csrf)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), status);
    }
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
