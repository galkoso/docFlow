use crate::{
    AppState, ErrorBody, auth,
    specification::{
        self, DocumentResponse, HighlightResponse, PngDisplay, SearchHit, SearchResponse,
        Specification,
    },
};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
    routing::get,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;
use specspace_application::{book::BookError, proposal_reader::ProposalBook};
use specspace_git_adapter::CommitSha;
use uuid::Uuid;

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
    commit: String,
    q: String,
    cursor: Option<String>,
    start_at: Option<usize>,
    #[param(minimum = 1, maximum = 500, default = 100)]
    limit: Option<usize>,
}
#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(crate) struct HighlightQuery {
    commit: String,
    q: String,
    windows: String,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/proposals/{proposal}/specification",
            get(specification),
        )
        .route(
            "/api/v1/proposals/{proposal}/documents/{document}",
            get(document),
        )
        .route("/api/v1/proposals/{proposal}/find", get(find))
        .route(
            "/api/v1/proposals/{proposal}/documents/{document}/highlights",
            get(highlights),
        )
        .route("/api/v1/proposals/{proposal}/assets/{asset}", get(asset))
}

fn invalid() -> Response {
    specification::failure(BookError::InvalidQuery("INVALID_QUERY"))
}

enum ReadError {
    Auth(auth::AuthFailure),
    Book(BookError),
}
impl IntoResponse for ReadError {
    fn into_response(self) -> Response {
        match self {
            Self::Auth(error) => error.into_response(),
            Self::Book(error) => specification::failure(error),
        }
    }
}

async fn load(
    state: &AppState,
    headers: &HeaderMap,
    proposal: Uuid,
    commit: Option<String>,
) -> Result<(Uuid, ProposalBook), ReadError> {
    let actor = auth::authenticate(state, headers)
        .await
        .map_err(ReadError::Auth)?;
    let commit = commit
        .map(CommitSha::parse)
        .transpose()
        .map_err(|_| ReadError::Book(BookError::InvalidQuery("INVALID_QUERY")))?;
    let book = state
        .proposal_books
        .at(actor.user_id, proposal, commit)
        .await
        .map_err(ReadError::Book)?;
    Ok((actor.user_id, book))
}

async fn compute<F>(
    state: &AppState,
    actor: Uuid,
    proposal: Uuid,
    snapshot: ProposalBook,
    operation: F,
) -> Response
where
    F: FnOnce(&specspace_application::book::BookSnapshot) -> Result<Response, BookError>
        + Send
        + 'static,
{
    let permit = match state.render_slots.clone().acquire_owned().await {
        Ok(permit) => permit,
        Err(_) => return specification::failure(BookError::ProcessingUnavailable),
    };
    let book = snapshot.book;
    let result = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        operation(&book)
    })
    .await;
    if let Err(error) = state
        .proposal_books
        .recheck(actor, proposal, &snapshot.scope)
        .await
    {
        return specification::failure(error);
    }
    match result {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => specification::failure(error),
        Err(_) => specification::failure(BookError::ProcessingUnavailable),
    }
}

#[utoipa::path(get, path = "/api/v1/proposals/{proposal}/specification", operation_id = "proposalSpecification", security(("sessionCookie" = [])), params(("proposal" = Uuid, Path), Selection), responses((status = 200, body = Specification), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody), (status = 413, body = ErrorBody), (status = 422, body = ErrorBody), (status = 502, body = ErrorBody), (status = 503, body = ErrorBody)))]
pub(crate) async fn specification(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, axum::extract::rejection::PathRejection>,
    query: Result<Query<Selection>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let (Ok(Path(proposal)), Ok(Query(query))) = (path, query) else {
        return invalid();
    };
    let selected = query.commit.is_some();
    let (_, snapshot) = match load(&state, &headers, proposal, query.commit).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let mut response = specification::metadata(snapshot.scope.repository, &snapshot.book, selected);
    response.branch = snapshot.scope.branch.as_str().into();
    // Never enqueue a proposal snapshot in the approved-book index.
    Json(response).into_response()
}

#[utoipa::path(get, path = "/api/v1/proposals/{proposal}/documents/{document}", operation_id = "proposalDocument", security(("sessionCookie" = [])), params(("proposal" = Uuid, Path), ("document" = String, Path), Version), responses((status = 200, body = DocumentResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody), (status = 413, body = ErrorBody), (status = 422, body = ErrorBody), (status = 502, body = ErrorBody), (status = 503, body = ErrorBody)))]
pub(crate) async fn document(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<(Uuid, String)>, axum::extract::rejection::PathRejection>,
    query: Result<Query<Version>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let (Ok(Path((proposal, document))), Ok(Query(query))) = (path, query) else {
        return invalid();
    };
    let (actor, snapshot) = match load(&state, &headers, proposal, Some(query.commit)).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    compute(&state, actor, proposal, snapshot, move |book| {
        specification::render_document(book, &document)
    })
    .await
}

#[utoipa::path(get, path = "/api/v1/proposals/{proposal}/find", operation_id = "proposalFind", security(("sessionCookie" = [])), params(("proposal" = Uuid, Path), FindQuery), responses((status = 200, body = SearchResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody), (status = 413, body = ErrorBody), (status = 422, body = ErrorBody), (status = 502, body = ErrorBody), (status = 503, body = ErrorBody)))]
pub(crate) async fn find(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, axum::extract::rejection::PathRejection>,
    query: Result<Query<FindQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let (Ok(Path(proposal)), Ok(Query(query))) = (path, query) else {
        return invalid();
    };
    let (actor, snapshot) = match load(&state, &headers, proposal, Some(query.commit)).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    if query.q.chars().count() > 200 {
        return specification::failure(BookError::InvalidQuery("QUERY_TOO_LONG"));
    }
    let limit = query.limit.unwrap_or(100);
    if !(1..=500).contains(&limit) {
        return specification::failure(BookError::InvalidQuery("INVALID_LIMIT"));
    }
    let offset = match (query.cursor.as_deref(), query.start_at) {
        (Some(_), Some(_)) => {
            return specification::failure(BookError::InvalidQuery("AMBIGUOUS_SEARCH_POSITION"));
        }
        (None, Some(offset)) => offset,
        (cursor, None) => {
            match specification::offset(cursor, proposal, &snapshot.book.commit, &query.q) {
                Ok(value) => value,
                Err(code) => return specification::failure(BookError::InvalidQuery(code)),
            }
        }
    };
    compute(&state, actor, proposal, snapshot, move |book| {
        let page = book.find(&query.q, offset, limit)?;
        let cursor = page
            .next_offset
            .map(|offset| {
                specification::next_cursor(proposal, &page.snapshot_sha, &query.q, offset)
            })
            .transpose()?;
        Ok(Json(SearchResponse {
            query: query.q,
            snapshot_sha: page.snapshot_sha,
            total_matches: page.total_matches,
            start_at: offset,
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
        .into_response())
    })
    .await
}

#[utoipa::path(get, path = "/api/v1/proposals/{proposal}/documents/{document}/highlights", operation_id = "proposalHighlights", security(("sessionCookie" = [])), params(("proposal" = Uuid, Path), ("document" = String, Path), HighlightQuery), responses((status = 200, body = HighlightResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody), (status = 413, body = ErrorBody), (status = 422, body = ErrorBody), (status = 502, body = ErrorBody), (status = 503, body = ErrorBody)))]
pub(crate) async fn highlights(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<(Uuid, String)>, axum::extract::rejection::PathRejection>,
    query: Result<Query<HighlightQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let (Ok(Path((proposal, document))), Ok(Query(query))) = (path, query) else {
        return invalid();
    };
    let (actor, snapshot) = match load(&state, &headers, proposal, Some(query.commit)).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    if query.q.chars().count() > 200 {
        return specification::failure(BookError::InvalidQuery("QUERY_TOO_LONG"));
    }
    if query.windows.len() > 8192 {
        return specification::failure(BookError::InvalidQuery("HIGHLIGHT_WINDOW_LIMIT"));
    }
    let windows = match URL_SAFE_NO_PAD
        .decode(query.windows)
        .ok()
        .and_then(|bytes| {
            serde_json::from_slice::<Vec<specspace_document_engine::find::MatchWindow>>(&bytes).ok()
        }) {
        Some(windows) if windows.len() <= 64 => windows,
        Some(_) => {
            return specification::failure(BookError::InvalidQuery("HIGHLIGHT_WINDOW_LIMIT"));
        }
        None => return specification::failure(BookError::InvalidQuery("INVALID_HIGHLIGHT_WINDOW")),
    };
    compute(&state, actor, proposal, snapshot, move |book| {
        Ok(Json(HighlightResponse {
            snapshot_sha: book.commit.as_str().into(),
            document_id: document.clone(),
            query: query.q.clone(),
            matches: book.highlights(&document, &query.q, &windows)?,
        })
        .into_response())
    })
    .await
}

#[utoipa::path(get, path = "/api/v1/proposals/{proposal}/assets/{asset}", operation_id = "proposalAsset", security(("sessionCookie" = [])), params(("proposal" = Uuid, Path), ("asset" = String, Path), Version), responses((status = 200, body = PngDisplay, content_type = "image/png"), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody), (status = 413, body = ErrorBody), (status = 422, body = ErrorBody), (status = 502, body = ErrorBody), (status = 503, body = ErrorBody)))]
pub(crate) async fn asset(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<(Uuid, String)>, axum::extract::rejection::PathRejection>,
    query: Result<Query<Version>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let actor = match auth::authenticate(&state, &headers).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    let (Ok(Path((proposal, filename))), Ok(Query(query))) = (path, query) else {
        return invalid();
    };
    let commit = match CommitSha::parse(query.commit) {
        Ok(commit) => commit,
        Err(_) => return invalid(),
    };
    match state
        .proposal_books
        .asset(
            actor.user_id,
            proposal,
            commit,
            format!("assets/{filename}"),
        )
        .await
    {
        Ok(image) => (
            [
                ("content-type", "image/png"),
                (
                    "content-disposition",
                    "inline; filename=\"specspace-display.png\"",
                ),
            ],
            image.png,
        )
            .into_response(),
        Err(error) => specification::failure(error),
    }
}
