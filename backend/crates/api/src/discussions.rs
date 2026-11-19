use crate::{AppState, ErrorBody, auth, proposals};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Deserialize;
use specspace_application::discussion::read::{DiscussionComments, DiscussionThreads};
use specspace_infrastructure::discussions::DiscussionStoreError;
use uuid::Uuid;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/proposals/{proposal}/threads", get(threads))
        .route(
            "/api/v1/proposals/{proposal}/threads/{thread}",
            get(comments),
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

async fn identity(state: &AppState, headers: &HeaderMap) -> Result<Uuid, auth::AuthFailure> {
    let user = auth::authenticate(state, headers).await?;
    auth::verify_actor(headers, &user)?;
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
    let actor = match identity(&state, &headers).await {
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
    let actor = match identity(&state, &headers).await {
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
