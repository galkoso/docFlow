use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::Response,
    routing::get,
};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Clone)]
pub struct AppState {
    pub database: PgPool,
}

#[derive(Serialize)]
struct Health {
    status: &'static str,
}

#[derive(Serialize)]
pub struct ErrorBody {
    pub error: ApiError,
}

#[derive(Serialize)]
pub struct ApiError {
    pub code: &'static str,
    pub message_he: &'static str,
    pub trace_id: Uuid,
    pub retryable: bool,
    pub field_errors: serde_json::Value,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/v1/health/live", get(live))
        .route("/api/v1/health/ready", get(ready))
        .fallback(not_found)
        .layer(DefaultBodyLimit::max(10 * 1024 * 1024))
        .layer(middleware::from_fn(security_headers))
        .with_state(state)
}

async fn live() -> Json<Health> {
    Json(Health { status: "live" })
}

/// Readiness currently covers the API's database dependency only.
/// Provider and storage readiness must be included when those adapters are wired.
async fn ready(State(state): State<AppState>) -> (StatusCode, Json<Health>) {
    let available = sqlx::query("SELECT 1")
        .execute(&state.database)
        .await
        .is_ok();
    if available {
        (StatusCode::OK, Json(Health { status: "ready" }))
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(Health {
                status: "not_ready",
            }),
        )
    }
}

async fn not_found() -> (StatusCode, Json<ErrorBody>) {
    (
        StatusCode::NOT_FOUND,
        Json(ErrorBody {
            error: ApiError {
                code: "NOT_FOUND",
                message_he: "המשאב המבוקש לא נמצא.",
                trace_id: Uuid::new_v4(),
                retryable: false,
                field_errors: serde_json::json!({}),
            },
        }),
    )
}

async fn security_headers(request: axum::extract::Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; frame-ancestors 'none'"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use sqlx::postgres::PgPoolOptions;
    use std::time::Duration;
    use tower::ServiceExt;

    fn app() -> Router {
        let database = PgPoolOptions::new()
            .acquire_timeout(Duration::from_millis(50))
            .connect_lazy("postgres://invalid:invalid@127.0.0.1:1/specspace")
            .unwrap();
        router(AppState { database })
    }

    #[tokio::test]
    async fn liveness_does_not_require_database() {
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/health/live")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(
            response.headers()[header::X_CONTENT_TYPE_OPTIONS],
            "nosniff"
        );
    }

    #[tokio::test]
    async fn database_failure_prevents_readiness() {
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/health/ready")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn unknown_resources_return_structured_errors() {
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/proposals/unknown")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["error"]["code"], "NOT_FOUND");
        assert!(Uuid::parse_str(value["error"]["trace_id"].as_str().unwrap()).is_ok());
    }
}
