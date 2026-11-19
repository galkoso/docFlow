use crate::{AppState, ErrorBody, auth, proposals, review_configuration::NoQuery};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use specspace_application::discussion::read::{DiscussionComments, DiscussionThreads};
use specspace_application::discussion::{
    DiscussionRecorded, OpenThreadRequest, ThreadMutationRequest,
};
use specspace_infrastructure::discussions::DiscussionStoreError;
use uuid::Uuid;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/proposals/{proposal}/threads",
            get(threads)
                .post(open)
                .layer(DefaultBodyLimit::max(128 * 1024)),
        )
        .route(
            "/api/v1/proposals/{proposal}/threads/{thread}",
            get(comments),
        )
        .route(
            "/api/v1/proposals/{proposal}/threads/{thread}/actions",
            post(mutate).layer(DefaultBodyLimit::max(128 * 1024)),
        )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThreadsQuery {
    after: Option<Uuid>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommentsQuery {
    after: Option<Uuid>,
    expected_thread_version: Option<i64>,
}

async fn identity(
    state: &AppState,
    headers: &HeaderMap,
    write: bool,
) -> Result<Uuid, auth::AuthFailure> {
    let user = auth::authenticate(state, headers).await?;
    auth::verify_actor(headers, &user)?;
    if write {
        auth::verify_write(state, headers, &user)?;
    }
    Ok(user.user_id)
}

#[utoipa::path(get,path="/api/v1/proposals/{proposal}/threads",operation_id="proposalDiscussionThreads",security(("sessionCookie"=[])),
    params(("proposal"=Uuid,Path),("X-Expected-Actor"=Uuid,Header),("after"=Option<Uuid>,Query)),
    responses((status=200,body=DiscussionThreads),(status=400,body=ErrorBody),(status=401,body=ErrorBody),(status=404,body=ErrorBody),(status=409,body=ErrorBody),(status=503,body=ErrorBody)))]
pub(crate) async fn threads(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, axum::extract::rejection::PathRejection>,
    query: Result<Query<ThreadsQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let actor = match identity(&state, &headers, false).await {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let (Ok(Path(proposal)), Ok(Query(query))) = (path, query) else {
        return proposals::invalid_query();
    };
    match state
        .discussions
        .threads(actor, proposal, query.after)
        .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => failure(e),
    }
}

#[utoipa::path(get,path="/api/v1/proposals/{proposal}/threads/{thread}",operation_id="proposalDiscussionComments",security(("sessionCookie"=[])),
    params(("proposal"=Uuid,Path),("thread"=Uuid,Path),("X-Expected-Actor"=Uuid,Header),("after"=Option<Uuid>,Query),("expected_thread_version"=Option<i64>,Query,description="Required with after; rejects paging across a changed thread")),
    responses((status=200,body=DiscussionComments),(status=400,body=ErrorBody),(status=401,body=ErrorBody),(status=404,body=ErrorBody),(status=409,body=ErrorBody),(status=503,body=ErrorBody)))]
pub(crate) async fn comments(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<(Uuid, Uuid)>, axum::extract::rejection::PathRejection>,
    query: Result<Query<CommentsQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let actor = match identity(&state, &headers, false).await {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let (Ok(Path((proposal, thread))), Ok(Query(query))) = (path, query) else {
        return proposals::invalid_query();
    };
    match state
        .discussions
        .comments(
            actor,
            proposal,
            thread,
            query.after,
            query.expected_thread_version,
        )
        .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => failure(e),
    }
}

fn recorded(receipt: DiscussionRecorded) -> Response {
    (
        if receipt.replayed {
            StatusCode::OK
        } else {
            StatusCode::CREATED
        },
        Json(receipt),
    )
        .into_response()
}
fn invalid_body(error: axum::extract::rejection::JsonRejection) -> Response {
    proposals::problem(
        if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
            StatusCode::PAYLOAD_TOO_LARGE
        } else {
            StatusCode::BAD_REQUEST
        },
        "INVALID_DISCUSSION",
        "פרטי פעולת הדיון אינם תקינים.",
        false,
        None,
    )
}

#[utoipa::path(post,path="/api/v1/proposals/{proposal}/threads",operation_id="openProposalDiscussion",security(("sessionCookie"=[])),
    params(("proposal"=Uuid,Path),("X-Expected-Actor"=Uuid,Header),("Origin"=String,Header),("X-XSRF-TOKEN"=String,Header)),request_body=OpenThreadRequest,
    responses((status=201,body=DiscussionRecorded),(status=200,body=DiscussionRecorded,description="Exact original receipt replay; current discussion state is read separately"),
    (status=400,body=ErrorBody),(status=401,body=ErrorBody),(status=403,body=ErrorBody),(status=404,body=ErrorBody),(status=409,body=ErrorBody),(status=413,body=ErrorBody),(status=429,body=ErrorBody),(status=503,body=ErrorBody)))]
pub(crate) async fn open(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, axum::extract::rejection::PathRejection>,
    query: Result<Query<NoQuery>, axum::extract::rejection::QueryRejection>,
    body: Result<Json<OpenThreadRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let actor = match identity(&state, &headers, true).await {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let (Ok(Path(proposal)), Ok(_)) = (path, query) else {
        return proposals::invalid_query();
    };
    let request = match body {
        Ok(Json(v)) => v,
        Err(e) => return invalid_body(e),
    };
    match state
        .discussions
        .open_thread(actor, proposal, request, state.review_files.as_ref())
        .await
    {
        Ok(v) => recorded(v),
        Err(e) => failure(e),
    }
}

#[utoipa::path(post,path="/api/v1/proposals/{proposal}/threads/{thread}/actions",operation_id="mutateProposalDiscussion",security(("sessionCookie"=[])),
    params(("proposal"=Uuid,Path),("thread"=Uuid,Path),("X-Expected-Actor"=Uuid,Header),("Origin"=String,Header),("X-XSRF-TOKEN"=String,Header)),request_body=ThreadMutationRequest,
    responses((status=201,body=DiscussionRecorded),(status=200,body=DiscussionRecorded,description="Original action receipt replayed without applying a second change"),
    (status=400,body=ErrorBody),(status=401,body=ErrorBody),(status=403,body=ErrorBody),(status=404,body=ErrorBody),(status=409,body=ErrorBody),(status=413,body=ErrorBody),(status=429,body=ErrorBody),(status=503,body=ErrorBody)))]
pub(crate) async fn mutate(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<(Uuid, Uuid)>, axum::extract::rejection::PathRejection>,
    query: Result<Query<NoQuery>, axum::extract::rejection::QueryRejection>,
    body: Result<Json<ThreadMutationRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let actor = match identity(&state, &headers, true).await {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let (Ok(Path((proposal, thread))), Ok(_)) = (path, query) else {
        return proposals::invalid_query();
    };
    let request = match body {
        Ok(Json(v)) => v,
        Err(e) => return invalid_body(e),
    };
    match state
        .discussions
        .mutate(actor, proposal, thread, request)
        .await
    {
        Ok(v) => recorded(v),
        Err(e) => failure(e),
    }
}
fn failure(error: DiscussionStoreError) -> Response {
    use DiscussionStoreError::*;
    let (status, code, retry) = match error {
        NotFound => (StatusCode::NOT_FOUND, "NOT_FOUND", None),
        Forbidden => (StatusCode::FORBIDDEN, "DISCUSSION_FORBIDDEN", None),
        IndependentReviewerRequired => {
            (StatusCode::FORBIDDEN, "INDEPENDENT_REVIEWER_REQUIRED", None)
        }
        Invalid => (StatusCode::BAD_REQUEST, "INVALID_DISCUSSION", None),
        NotReady => (StatusCode::CONFLICT, "DISCUSSION_NOT_READY", None),
        Stale => (StatusCode::CONFLICT, "DISCUSSION_CONTEXT_CHANGED", None),
        Tombstoned => (StatusCode::CONFLICT, "COMMENT_TOMBSTONED", None),
        IdempotencyMismatch => (StatusCode::CONFLICT, "IDEMPOTENCY_MISMATCH", None),
        Quota => (StatusCode::TOO_MANY_REQUESTS, "DISCUSSION_LIMIT", Some(600)),
        ProviderRateLimited(delay) => (
            StatusCode::TOO_MANY_REQUESTS,
            "DISCUSSION_PROVIDER_RATE_LIMIT",
            Some(delay),
        ),
        SourceUnavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            "DISCUSSION_SOURCE_UNAVAILABLE",
            Some(30),
        ),
        InvalidSource => (
            StatusCode::SERVICE_UNAVAILABLE,
            "DISCUSSION_SOURCE_INVALID",
            Some(30),
        ),
        Capacity => (
            StatusCode::SERVICE_UNAVAILABLE,
            "DISCUSSION_SOURCE_CAPACITY",
            Some(1),
        ),
        Unavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            "DISCUSSION_UNAVAILABLE",
            Some(1),
        ),
    };
    let mut response = proposals::problem(
        status,
        code,
        "לא ניתן להשלים את פעולת הדיון. יש לבדוק גרסה והרשאות.",
        retry.is_some(),
        None,
    );
    if let Some(seconds) = retry {
        response.headers_mut().insert(
            header::RETRY_AFTER,
            HeaderValue::from_str(&seconds.to_string()).expect("integer delay"),
        );
    }
    response
}
