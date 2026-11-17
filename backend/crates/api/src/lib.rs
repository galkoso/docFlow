pub mod auth;
pub mod comparisons;
pub mod contract;
pub mod creation_context;
pub mod drafts;
pub mod editor;
pub mod proposal_content;
pub mod proposal_reads;
pub mod proposals;
pub mod saves;
pub mod specification;

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::Response,
    routing::get,
};
use serde::Serialize;
use specspace_application::comparison_reader::{BoundProviderFactory, ComparisonReader};
use specspace_application::draft::DraftRetention;
use specspace_application::proposal_reader::ProposalReader;
use specspace_application::reader::{BookProviderFactory, BookReader};
use specspace_git_adapter::app_auth::GitHubAppKey;
use specspace_infrastructure::authorization::PgProposalReadAuthorization;
use specspace_infrastructure::comparison_authorization::PgComparisonAuthorization;
use specspace_infrastructure::drafts::PgDrafts;
use specspace_infrastructure::oidc::Oidc;
use specspace_infrastructure::{authorization::PgReadAuthorization, github_source::PgGitHubSource};
use specspace_infrastructure::{
    comparison_worker::ComparisonWorker,
    comparisons::{ComparisonStoreError, PgComparisons},
};
use specspace_infrastructure::{
    index_worker::{GitIndexSource, IndexSource, IndexWorker},
    proposal_worker::ProposalWorker,
    provisioning::{PgProvisioning, ProvisionError},
    search_index::{IndexError, PgSearchIndex},
};
use specspace_infrastructure::{
    save_worker::SaveWorker,
    saves::{PgSaves, SaveJournalError},
};
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone)]
pub struct AppState {
    pub database: PgPool,
    pub oidc: Option<Arc<Oidc>>,
    pub cookies: auth::CookiePolicy,
    pub books: Arc<BookReader>,
    pub proposal_books: Arc<ProposalReader>,
    pub comparisons: Arc<ComparisonReader>,
    pub comparison_jobs: Arc<PgComparisons>,
    pub drafts: Arc<PgDrafts>,
    pub search_index: Arc<PgSearchIndex>,
    pub saves: Arc<PgSaves>,
    index_source: Arc<dyn IndexSource>,
    book_source: Arc<dyn BookProviderFactory>,
    provisioning: Arc<PgProvisioning>,
    pub render_slots: Arc<tokio::sync::Semaphore>,
}

impl AppState {
    pub fn new(database: PgPool) -> Self {
        let source = Arc::new(PgGitHubSource::new(database.clone(), None));
        let search_index = Arc::new(PgSearchIndex::new(database.clone()));
        let books = Arc::new(
            BookReader::new(
                Arc::new(PgReadAuthorization::new(database.clone())),
                source.clone(),
            )
            .with_history(search_index.clone()),
        );
        let index_source = Arc::new(GitIndexSource::new(
            database.clone(),
            source.clone(),
            Some(books.clone()),
        ));
        let provisioning = Arc::new(PgProvisioning::new(database.clone()));
        let proposal_books = Arc::new(ProposalReader::new(
            Arc::new(PgProposalReadAuthorization::new(database.clone())),
            Arc::new(PgReadAuthorization::new(database.clone())),
            source.clone(),
        ));
        let comparisons = Arc::new(
            ComparisonReader::new(
                Arc::new(PgComparisonAuthorization::new(database.clone())),
                source.clone(),
                std::time::Duration::from_secs(900),
            )
            .expect("fixed comparison timeout is valid"),
        );
        Self {
            comparison_jobs: Arc::new(PgComparisons::new(database.clone())),
            comparisons,
            saves: Arc::new(PgSaves::new(database.clone())),
            drafts: Arc::new(PgDrafts::new(database.clone(), None)),
            database,
            oidc: None,
            cookies: auth::CookiePolicy::unconfigured(),
            books,
            proposal_books,
            search_index,
            index_source,
            book_source: source,
            provisioning,
            render_slots: Arc::new(tokio::sync::Semaphore::new(2)),
        }
    }

    pub fn configure_github(&mut self, key: GitHubAppKey) {
        let source = Arc::new(PgGitHubSource::new(
            self.database.clone(),
            Some(Arc::new(key)),
        ));
        self.configure_comparison_source(source.clone());
        self.configure_book_source(source);
    }

    /// Comparison test/production sources must support frozen connection settings;
    /// an unbound book source is never silently reused for this capability.
    pub fn configure_comparison_source(&mut self, source: Arc<dyn BoundProviderFactory>) {
        self.comparisons = Arc::new(
            ComparisonReader::new(
                Arc::new(PgComparisonAuthorization::new(self.database.clone())),
                source,
                std::time::Duration::from_secs(900),
            )
            .expect("fixed comparison timeout is valid"),
        );
    }

    pub fn configure_draft_retention(&mut self, retention: DraftRetention) {
        self.drafts = Arc::new(PgDrafts::new(self.database.clone(), Some(retention)));
    }

    /// Reader and background indexer must use the same configured source/cache.
    pub fn configure_book_source(&mut self, source: Arc<dyn BookProviderFactory>) {
        self.book_source = source.clone();
        self.proposal_books = Arc::new(ProposalReader::new(
            Arc::new(PgProposalReadAuthorization::new(self.database.clone())),
            Arc::new(PgReadAuthorization::new(self.database.clone())),
            source.clone(),
        ));
        self.books = Arc::new(
            BookReader::new(
                Arc::new(PgReadAuthorization::new(self.database.clone())),
                source.clone(),
            )
            .with_history(self.search_index.clone()),
        );
        self.index_source = Arc::new(GitIndexSource::new(
            self.database.clone(),
            source,
            Some(self.books.clone()),
        ));
    }

    pub fn comparison_worker(
        &self,
        timeout: std::time::Duration,
    ) -> Result<ComparisonWorker, ComparisonStoreError> {
        ComparisonWorker::new(
            self.comparison_jobs.clone(),
            self.comparisons.clone(),
            timeout,
        )
    }

    pub fn index_worker(&self, timeout: std::time::Duration) -> Result<IndexWorker, IndexError> {
        IndexWorker::new(
            self.search_index.clone(),
            self.index_source.clone(),
            timeout,
        )
    }

    pub fn proposal_worker(
        &self,
        timeout: std::time::Duration,
    ) -> Result<ProposalWorker, ProvisionError> {
        ProposalWorker::new(self.provisioning.clone(), self.book_source.clone(), timeout)
    }
    pub fn save_worker(
        &self,
        timeout: std::time::Duration,
    ) -> Result<SaveWorker, SaveJournalError> {
        SaveWorker::new(self.saves.clone(), self.book_source.clone(), timeout)
    }
}

#[derive(Serialize, utoipa::ToSchema)]
struct Health {
    status: &'static str,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct ErrorBody {
    pub error: ApiError,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct ApiError {
    pub code: &'static str,
    pub message_he: &'static str,
    pub trace_id: Uuid,
    pub retryable: bool,
    pub field_errors: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .merge(auth::routes())
        .merge(specification::routes())
        .merge(proposals::routes())
        .merge(proposal_content::routes())
        .merge(drafts::routes())
        .merge(editor::routes())
        .merge(saves::routes())
        .merge(comparisons::routes())
        .route("/api/v1/health/live", get(live))
        .route("/api/v1/health/ready", get(ready))
        .fallback(not_found)
        .layer(DefaultBodyLimit::max(10 * 1024 * 1024))
        .layer(middleware::from_fn(security_headers))
        .with_state(state)
}

#[utoipa::path(get, path = "/api/v1/health/live", responses((status = 200, body = Health)))]
async fn live() -> Json<Health> {
    Json(Health { status: "live" })
}

/// Readiness currently covers the API's database dependency only.
/// Provider and storage readiness must be included when those adapters are wired.
#[utoipa::path(get, path = "/api/v1/health/ready", responses((status = 200, body = Health), (status = 503, body = Health)))]
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
                details: None,
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
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
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
        router(AppState::new(database))
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
                    .uri("/api/v1/unrecognized")
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
