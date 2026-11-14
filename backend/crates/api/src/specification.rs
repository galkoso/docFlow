use crate::{ApiError, AppState, ErrorBody, auth};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use specspace_application::book::{BookError, BookSnapshot};
use specspace_document_engine::markdown::TextBlock;
use specspace_git_adapter::{CommitSha, GitError};
use uuid::Uuid;

#[derive(Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct RepositorySummary {
    pub repository_id: Uuid,
    pub default_branch: String,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct RepositoryList {
    pub repositories: Vec<RepositorySummary>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct SectionSummary {
    pub section_id: String,
    pub title: String,
    pub level: u8,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct DocumentSummary {
    pub document_id: String,
    pub path: String,
    pub title: String,
    pub error_code: Option<String>,
    pub sections: Vec<SectionSummary>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct ReadFinding {
    pub path: String,
    pub code: String,
    pub source_start_byte: Option<usize>,
    pub source_end_byte: Option<usize>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct Specification {
    pub branch: String,
    pub repository_id: Uuid,
    pub snapshot_sha: String,
    pub version_selected: bool,
    pub updated_at: String,
    pub documents: Vec<DocumentSummary>,
    pub findings: Vec<ReadFinding>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct DocumentBlock {
    pub block_id: String,
    pub section_id: String,
    pub kind: String,
    pub text: String,
    pub heading_level: Option<u8>,
    pub source_start_byte: usize,
    pub source_end_byte: usize,
}

impl From<&TextBlock> for DocumentBlock {
    fn from(block: &TextBlock) -> Self {
        Self {
            block_id: block.block_id.clone(),
            section_id: block.section_id.clone(),
            kind: block.kind.into(),
            text: block.text.clone(),
            heading_level: block.heading_level,
            source_start_byte: block.source_start,
            source_end_byte: block.source_end,
        }
    }
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct DocumentResponse {
    pub snapshot_sha: String,
    pub document_id: String,
    pub title: String,
    pub markdown: Option<String>,
    pub sha256: Option<String>,
    pub error_code: Option<String>,
    pub blocks: Vec<DocumentBlock>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct SearchHit {
    pub match_id: String,
    pub document_id: String,
    pub section_id: String,
    pub block_id: String,
    pub start_utf16: u32,
    pub end_utf16: u32,
    pub excerpt: String,
    pub book_order: usize,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct SearchResponse {
    pub query: String,
    pub snapshot_sha: String,
    pub total_matches: usize,
    pub next_cursor: Option<String>,
    pub matches: Vec<SearchHit>,
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(crate) struct Selection {
    commit: Option<String>,
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(crate) struct Version {
    commit: String,
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(crate) struct FindQuery {
    q: String,
    commit: String,
    cursor: Option<String>,
    #[param(minimum = 1, maximum = 500, default = 100)]
    limit: Option<usize>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    repository: Uuid,
    commit: String,
    query_hash: String,
    offset: usize,
}

fn query_hash(query: &str) -> String {
    format!("{:x}", Sha256::digest(query.as_bytes()))
}

fn offset(
    cursor: Option<&str>,
    repository: Uuid,
    commit: &CommitSha,
    query: &str,
) -> Result<usize, &'static str> {
    let Some(cursor) = cursor else {
        return Ok(0);
    };
    if cursor.len() > 2048 {
        return Err("INVALID_CURSOR");
    }
    let decoded = URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| "INVALID_CURSOR")?;
    let cursor: Cursor = serde_json::from_slice(&decoded).map_err(|_| "INVALID_CURSOR")?;
    if cursor.repository != repository
        || cursor.commit != commit.as_str()
        || cursor.query_hash != query_hash(query)
    {
        return Err("INVALID_CURSOR");
    }
    Ok(cursor.offset)
}

fn next_cursor(
    repository: Uuid,
    commit: &str,
    query: &str,
    offset: usize,
) -> Result<String, BookError> {
    let cursor = Cursor {
        repository,
        commit: commit.into(),
        query_hash: query_hash(query),
        offset,
    };
    serde_json::to_vec(&cursor)
        .map(|bytes| URL_SAFE_NO_PAD.encode(bytes))
        .map_err(|_| BookError::ProcessingUnavailable)
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/repositories", get(repositories))
        .route(
            "/api/v1/repositories/{repository}/specification",
            get(specification),
        )
        .route(
            "/api/v1/repositories/{repository}/documents/{document}",
            get(document),
        )
        .route("/api/v1/repositories/{repository}/find", get(find))
}

fn failure(error: BookError) -> Response {
    let retry_after = match &error {
        BookError::Provider(GitError::RateLimited {
            retry_after_seconds,
        }) => *retry_after_seconds,
        _ => None,
    };
    let details = match &error {
        BookError::BookTooLarge => {
            Some(serde_json::json!({"max_book_bytes": 20 * 1024 * 1024, "max_documents": 1000}))
        }
        BookError::Provider(GitError::RateLimited { .. }) => {
            Some(serde_json::json!({"retry_after_seconds": retry_after}))
        }
        BookError::InvalidCatalog => Some(serde_json::json!({"path": ".specspace/catalog.json"})),
        _ => None,
    };
    let (status, code, message, retryable) = match error {
        BookError::NotFound => (
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            "המשאב המבוקש לא נמצא.",
            false,
        ),
        BookError::RepositoryNotConfigured => (
            StatusCode::SERVICE_UNAVAILABLE,
            "REPOSITORY_NOT_CONFIGURED",
            "חיבור מאגר האפיון עדיין לא הוגדר.",
            false,
        ),
        BookError::InvalidRepositoryConfiguration => (
            StatusCode::SERVICE_UNAVAILABLE,
            "REPOSITORY_CONFIGURATION_INVALID",
            "נדרש תיקון להגדרת מאגר האפיון.",
            false,
        ),
        BookError::InvalidCatalog => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "VALIDATION_FAILED",
            "קטלוג האפיון אינו תקין. נדרש תיקון סדר המסמכים והמזהים.",
            false,
        ),
        BookError::BookTooLarge => (
            StatusCode::PAYLOAD_TOO_LARGE,
            "FILE_TOO_LARGE",
            "ספר האפיון חורג ממגבלת 20MiB או 1,000 מסמכים.",
            false,
        ),
        BookError::SearchSnapshotNotReady => (
            StatusCode::CONFLICT,
            "SEARCH_SNAPSHOT_NOT_READY",
            "לא ניתן לחפש בגרסה זו עד לתיקון בעיות המבנה באפיון.",
            false,
        ),
        BookError::InvalidQuery(code) => (
            StatusCode::BAD_REQUEST,
            code,
            "פרטי הבקשה אינם תקינים. יש לרענן את הגרסה או את החיפוש.",
            false,
        ),
        BookError::Provider(GitError::RateLimited { .. }) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "PROVIDER_RATE_LIMIT",
            "ספק המאגר מגביל כרגע את קצב הבקשות. אפשר לנסות שוב בהמשך.",
            true,
        ),
        BookError::Provider(GitError::BranchMissing) => (
            StatusCode::NOT_FOUND,
            "BRANCH_MISSING",
            "ענף האפיון אינו זמין במאגר. יש לבדוק את חיבור המאגר.",
            false,
        ),
        BookError::Provider(_) => (
            StatusCode::BAD_GATEWAY,
            "PROVIDER_UNAVAILABLE",
            "ספק מאגר האפיון אינו זמין כרגע. אפשר לנסות שוב או לבדוק את החיבור.",
            true,
        ),
        BookError::AuthorizationUnavailable | BookError::ProcessingUnavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            "SERVICE_UNAVAILABLE",
            "האפיון אינו זמין כרגע. אפשר לנסות שוב בהמשך.",
            true,
        ),
    };
    let mut response = (
        status,
        Json(ErrorBody {
            error: ApiError {
                code,
                message_he: message,
                trace_id: Uuid::new_v4(),
                retryable,
                field_errors: serde_json::json!({}),
                details,
            },
        }),
    )
        .into_response();
    if let Some(seconds) = retry_after
        && let Ok(value) = axum::http::HeaderValue::from_str(&seconds.to_string())
    {
        response
            .headers_mut()
            .insert(axum::http::header::RETRY_AFTER, value);
    }
    response
}

fn invalid_query() -> Response {
    failure(BookError::InvalidQuery("INVALID_QUERY"))
}

#[utoipa::path(get, path = "/api/v1/repositories", security(("sessionCookie" = [])), responses((status = 200, body = RepositoryList), (status = 401, body = ErrorBody), (status = 503, body = ErrorBody)))]
pub(crate) async fn repositories(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let actor = match auth::authenticate(&state, &headers).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    let repositories = sqlx::query_as::<_, RepositorySummary>("SELECT r.id AS repository_id, r.default_branch FROM repositories r JOIN memberships m ON m.organization_id = r.organization_id JOIN users u ON u.id = m.user_id WHERE m.user_id = $1 AND u.status = 'ACTIVE' ORDER BY r.id LIMIT 201")
        .bind(actor.user_id).fetch_all(&state.database).await;
    match repositories {
        Ok(repositories) if repositories.len() <= 200 => {
            Json(RepositoryList { repositories }).into_response()
        }
        Ok(_) => failure(BookError::InvalidRepositoryConfiguration),
        Err(_) => failure(BookError::AuthorizationUnavailable),
    }
}

fn metadata(repository_id: Uuid, book: &BookSnapshot, version_selected: bool) -> Specification {
    Specification {
        branch: "main".into(),
        repository_id,
        snapshot_sha: book.commit.as_str().into(),
        version_selected,
        updated_at: book.updated_at.clone(),
        documents: book
            .documents
            .iter()
            .map(|doc| DocumentSummary {
                document_id: doc.document_id.clone(),
                path: doc.path.clone(),
                title: doc.content.as_ref().map_or_else(
                    || doc.document_id.clone(),
                    |content| content.metadata.title.clone(),
                ),
                error_code: doc.error_code.clone(),
                sections: doc.content.as_ref().map_or_else(Vec::new, |content| {
                    content
                        .blocks
                        .iter()
                        .filter(|block| block.kind == "heading" && !block.section_id.is_empty())
                        .map(|block| SectionSummary {
                            section_id: block.section_id.clone(),
                            title: block.text.clone(),
                            level: block.heading_level.unwrap_or(2),
                        })
                        .collect()
                }),
            })
            .collect(),
        findings: book
            .findings
            .iter()
            .map(|finding| ReadFinding {
                path: finding.path.clone(),
                code: finding.code.clone(),
                source_start_byte: finding.source_start,
                source_end_byte: finding.source_end,
            })
            .collect(),
    }
}

#[utoipa::path(get, path = "/api/v1/repositories/{repository}/specification", security(("sessionCookie" = [])), params(("repository" = Uuid, Path), Selection), responses((status = 200, body = Specification), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 404, body = ErrorBody), (status = 413, body = ErrorBody), (status = 422, body = ErrorBody), (status = 502, body = ErrorBody), (status = 503, body = ErrorBody)))]
pub(crate) async fn specification(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, axum::extract::rejection::PathRejection>,
    query: Result<Query<Selection>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let actor = match auth::authenticate(&state, &headers).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    let (Ok(Path(repository)), Ok(Query(query))) = (path, query) else {
        return invalid_query();
    };
    if let Err(error) = state.books.authorize(actor.user_id, repository).await {
        return failure(error);
    }
    let selected = query.commit.is_some();
    let result = match query.commit {
        Some(commit) => match CommitSha::parse(commit) {
            Ok(commit) => state.books.at(actor.user_id, repository, commit).await,
            Err(_) => return invalid_query(),
        },
        None => state.books.main(actor.user_id, repository).await,
    };
    match result {
        Ok(book) => Json(metadata(repository, &book, selected)).into_response(),
        Err(error) => failure(error),
    }
}

#[utoipa::path(get, path = "/api/v1/repositories/{repository}/documents/{document}", security(("sessionCookie" = [])), params(("repository" = Uuid, Path), ("document" = String, Path), Version), responses((status = 200, body = DocumentResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 404, body = ErrorBody), (status = 413, body = ErrorBody), (status = 422, body = ErrorBody), (status = 502, body = ErrorBody), (status = 503, body = ErrorBody)))]
pub(crate) async fn document(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<(Uuid, String)>, axum::extract::rejection::PathRejection>,
    query: Result<Query<Version>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let actor = match auth::authenticate(&state, &headers).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    let (Ok(Path((repository, document))), Ok(Query(query))) = (path, query) else {
        return invalid_query();
    };
    if let Err(error) = state.books.authorize(actor.user_id, repository).await {
        return failure(error);
    }
    let commit = match CommitSha::parse(query.commit) {
        Ok(commit) => commit,
        Err(_) => return invalid_query(),
    };
    let book = match state.books.at(actor.user_id, repository, commit).await {
        Ok(book) => book,
        Err(error) => return failure(error),
    };
    let matching: Vec<_> = book
        .documents
        .iter()
        .filter(|doc| doc.document_id == document)
        .collect();
    let doc = match matching.as_slice() {
        [] => return failure(BookError::NotFound),
        [doc] => doc,
        _ => return failure(BookError::InvalidCatalog),
    };
    Json(DocumentResponse {
        snapshot_sha: book.commit.as_str().into(),
        document_id: doc.document_id.clone(),
        title: doc.content.as_ref().map_or_else(
            || doc.document_id.clone(),
            |content| content.metadata.title.clone(),
        ),
        markdown: doc.content.as_ref().map(|content| content.source.clone()),
        sha256: doc.content.as_ref().map(|content| content.sha256.clone()),
        error_code: doc.error_code.clone(),
        blocks: doc.content.as_ref().map_or_else(Vec::new, |content| {
            content.blocks.iter().map(DocumentBlock::from).collect()
        }),
    })
    .into_response()
}

#[utoipa::path(get, path = "/api/v1/repositories/{repository}/find", security(("sessionCookie" = [])), params(("repository" = Uuid, Path), FindQuery), responses((status = 200, body = SearchResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody), (status = 413, body = ErrorBody), (status = 422, body = ErrorBody), (status = 502, body = ErrorBody), (status = 503, body = ErrorBody)))]
pub(crate) async fn find(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, axum::extract::rejection::PathRejection>,
    query: Result<Query<FindQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let actor = match auth::authenticate(&state, &headers).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    let (Ok(Path(repository)), Ok(Query(query))) = (path, query) else {
        return invalid_query();
    };
    if let Err(error) = state.books.authorize(actor.user_id, repository).await {
        return failure(error);
    }
    let commit = match CommitSha::parse(query.commit) {
        Ok(commit) => commit,
        Err(_) => return invalid_query(),
    };
    if query.q.chars().count() > 200 {
        return failure(BookError::InvalidQuery("QUERY_TOO_LONG"));
    }
    if !(1..=500).contains(&query.limit.unwrap_or(100)) {
        return failure(BookError::InvalidQuery("INVALID_LIMIT"));
    }
    let offset = match offset(query.cursor.as_deref(), repository, &commit, &query.q) {
        Ok(offset) => offset,
        Err(code) => return failure(BookError::InvalidQuery(code)),
    };
    match state
        .books
        .find(
            actor.user_id,
            repository,
            commit,
            query.q.clone(),
            offset,
            query.limit.unwrap_or(100),
        )
        .await
    {
        Ok(page) => {
            let cursor = match page
                .next_offset
                .map(|offset| next_cursor(repository, &page.snapshot_sha, &query.q, offset))
                .transpose()
            {
                Ok(cursor) => cursor,
                Err(error) => return failure(error),
            };
            Json(SearchResponse {
                query: query.q,
                snapshot_sha: page.snapshot_sha,
                total_matches: page.total_matches,
                next_cursor: cursor,
                matches: page
                    .matches
                    .into_iter()
                    .map(|hit| SearchHit {
                        match_id: hit.match_id,
                        document_id: hit.document_id,
                        section_id: hit.section_id,
                        block_id: hit.block_id,
                        start_utf16: hit.start_utf16,
                        end_utf16: hit.end_utf16,
                        excerpt: hit.excerpt,
                        book_order: hit.book_order,
                    })
                    .collect(),
            })
            .into_response()
        }
        Err(error) => failure(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursors_cannot_cross_repository_query_or_snapshot_boundaries() {
        let repository = Uuid::new_v4();
        let commit = CommitSha::parse("1111111111111111111111111111111111111111").unwrap();
        let cursor = next_cursor(repository, commit.as_str(), "מחיקה", 100).unwrap();
        assert_eq!(offset(Some(&cursor), repository, &commit, "מחיקה"), Ok(100));
        assert!(offset(Some(&cursor), Uuid::new_v4(), &commit, "מחיקה").is_err());
        assert!(offset(Some(&cursor), repository, &commit, "other").is_err());
        assert!(
            offset(
                Some(&cursor),
                repository,
                &CommitSha::parse("2222222222222222222222222222222222222222").unwrap(),
                "מחיקה"
            )
            .is_err()
        );
    }
}
