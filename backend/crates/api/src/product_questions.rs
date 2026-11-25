use crate::{AppState, ErrorBody, auth, proposals, review_configuration::NoQuery};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use specspace_application::product_question::*;
use specspace_infrastructure::product_questions::{PgProductQuestions, ProductQuestionError};
use uuid::Uuid;
pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/proposals/{proposal}/questions",
            get(list)
                .post(create)
                .layer(DefaultBodyLimit::max(256 * 1024)),
        )
        .route(
            "/api/v1/proposals/{proposal}/questions/{question}",
            get(history),
        )
        .route(
            "/api/v1/proposals/{proposal}/questions/{question}/actions",
            post(change).layer(DefaultBodyLimit::max(128 * 1024)),
        )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListQuery {
    after: Option<Uuid>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryQuery {
    after: Option<i64>,
}
async fn identity(
    state: &AppState,
    headers: &HeaderMap,
    write: bool,
) -> Result<Uuid, auth::AuthFailure> {
    let actor = auth::authenticate(state, headers).await?;
    auth::verify_actor(headers, &actor)?;
    if write {
        auth::verify_write(state, headers, &actor)?;
    }
    Ok(actor.user_id)
}
#[utoipa::path(get,path="/api/v1/proposals/{proposal}/questions",operation_id="proposalProductQuestions",security(("sessionCookie"=[])),
    params(("proposal"=Uuid,Path),("X-Expected-Actor"=Uuid,Header),("after"=Option<Uuid>,Query)),
    responses((status=200,body=ProductQuestions),(status=400,body=ErrorBody),(status=401,body=ErrorBody),(status=404,body=ErrorBody),(status=409,body=ErrorBody),(status=503,body=ErrorBody)))]
pub(crate) async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, axum::extract::rejection::PathRejection>,
    query: Result<Query<ListQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let actor = match identity(&state, &headers, false).await {
        Ok(a) => a,
        Err(e) => return e.into_response(),
    };
    let (Ok(Path(proposal)), Ok(Query(query))) = (path, query) else {
        return proposals::invalid_query();
    };
    match PgProductQuestions::new(state.database.clone())
        .list(actor, proposal, query.after)
        .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => failure(e),
    }
}
#[utoipa::path(get,path="/api/v1/proposals/{proposal}/questions/{question}",operation_id="proposalProductQuestionHistory",security(("sessionCookie"=[])),
    params(("proposal"=Uuid,Path),("question"=Uuid,Path),("X-Expected-Actor"=Uuid,Header),("after"=Option<i64>,Query,description="Append-only revision version cursor, at least 1")),
    responses((status=200,body=ProductQuestionHistory),(status=400,body=ErrorBody),(status=401,body=ErrorBody),(status=404,body=ErrorBody),(status=409,body=ErrorBody),(status=503,body=ErrorBody)))]
pub(crate) async fn history(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<(Uuid, Uuid)>, axum::extract::rejection::PathRejection>,
    query: Result<Query<HistoryQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let actor = match identity(&state, &headers, false).await {
        Ok(a) => a,
        Err(e) => return e.into_response(),
    };
    let (Ok(Path((proposal, question))), Ok(Query(query))) = (path, query) else {
        return proposals::invalid_query();
    };
    match PgProductQuestions::new(state.database.clone())
        .history(actor, proposal, question, query.after)
        .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => failure(e),
    }
}
fn receipt(v: ProductQuestionRecorded) -> Response {
    (
        if v.replayed {
            StatusCode::OK
        } else {
            StatusCode::CREATED
        },
        Json(v),
    )
        .into_response()
}
fn invalid_body(e: axum::extract::rejection::JsonRejection) -> Response {
    proposals::problem(
        if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
            StatusCode::PAYLOAD_TOO_LARGE
        } else {
            StatusCode::BAD_REQUEST
        },
        "INVALID_PRODUCT_QUESTION",
        "פרטי שאלת המוצר אינם תקינים.",
        false,
        None,
    )
}
#[utoipa::path(post,path="/api/v1/proposals/{proposal}/questions",operation_id="createProposalProductQuestion",security(("sessionCookie"=[])),
    params(("proposal"=Uuid,Path),("X-Expected-Actor"=Uuid,Header),("Origin"=String,Header),("X-XSRF-TOKEN"=String,Header)),request_body=CreateProductQuestion,
    responses((status=201,body=ProductQuestionRecorded),(status=200,body=ProductQuestionRecorded,description="Original receipt replayed; current question state is read separately"),
    (status=400,body=ErrorBody),(status=401,body=ErrorBody),(status=403,body=ErrorBody),(status=404,body=ErrorBody),(status=409,body=ErrorBody),(status=413,body=ErrorBody),(status=429,body=ErrorBody),(status=503,body=ErrorBody)))]
pub(crate) async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, axum::extract::rejection::PathRejection>,
    query: Result<Query<NoQuery>, axum::extract::rejection::QueryRejection>,
    body: Result<Json<CreateProductQuestion>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let actor = match identity(&state, &headers, true).await {
        Ok(a) => a,
        Err(e) => return e.into_response(),
    };
    let (Ok(Path(proposal)), Ok(_)) = (path, query) else {
        return proposals::invalid_query();
    };
    let request = match body {
        Ok(Json(v)) => v,
        Err(e) => return invalid_body(e),
    };
    match PgProductQuestions::new(state.database.clone())
        .create(actor, proposal, request)
        .await
    {
        Ok(v) => receipt(v),
        Err(e) => failure(e),
    }
}
#[utoipa::path(post,path="/api/v1/proposals/{proposal}/questions/{question}/actions",operation_id="changeProposalProductQuestion",security(("sessionCookie"=[])),
    params(("proposal"=Uuid,Path),("question"=Uuid,Path),("X-Expected-Actor"=Uuid,Header),("Origin"=String,Header),("X-XSRF-TOKEN"=String,Header)),request_body=ChangeProductQuestion,
    responses((status=201,body=ProductQuestionRecorded),(status=200,body=ProductQuestionRecorded,description="Exact original action receipt, not a new state transition"),
    (status=400,body=ErrorBody),(status=401,body=ErrorBody),(status=403,body=ErrorBody),(status=404,body=ErrorBody),(status=409,body=ErrorBody),(status=413,body=ErrorBody),(status=429,body=ErrorBody),(status=503,body=ErrorBody)))]
pub(crate) async fn change(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<(Uuid, Uuid)>, axum::extract::rejection::PathRejection>,
    query: Result<Query<NoQuery>, axum::extract::rejection::QueryRejection>,
    body: Result<Json<ChangeProductQuestion>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let actor = match identity(&state, &headers, true).await {
        Ok(a) => a,
        Err(e) => return e.into_response(),
    };
    let (Ok(Path((proposal, question))), Ok(_)) = (path, query) else {
        return proposals::invalid_query();
    };
    let request = match body {
        Ok(Json(v)) => v,
        Err(e) => return invalid_body(e),
    };
    match PgProductQuestions::new(state.database.clone())
        .change(actor, proposal, question, request)
        .await
    {
        Ok(v) => receipt(v),
        Err(e) => failure(e),
    }
}
fn failure(e: ProductQuestionError) -> Response {
    use ProductQuestionError::*;
    let (status, code, retry) = match e {
        Invalid => (StatusCode::BAD_REQUEST, "INVALID_PRODUCT_QUESTION", None),
        NotFound => (StatusCode::NOT_FOUND, "NOT_FOUND", None),
        Forbidden => (StatusCode::FORBIDDEN, "QUESTION_FORBIDDEN", None),
        ProductRequired => (StatusCode::FORBIDDEN, "QUESTION_PRODUCT_REQUIRED", None),
        NotEditable => (StatusCode::CONFLICT, "PROPOSAL_NOT_EDITABLE", None),
        MergeInProgress => (StatusCode::CONFLICT, "QUESTION_MERGE_IN_PROGRESS", None),
        Conflict => (StatusCode::CONFLICT, "QUESTION_ACTION_UNAVAILABLE", None),
        Stale => (StatusCode::CONFLICT, "QUESTION_VERSION_MISMATCH", None),
        IdempotencyMismatch => (StatusCode::CONFLICT, "IDEMPOTENCY_MISMATCH", None),
        Quota => (
            StatusCode::TOO_MANY_REQUESTS,
            "PRODUCT_QUESTION_LIMIT",
            Some(600),
        ),
        Unavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            "PRODUCT_QUESTION_UNAVAILABLE",
            Some(1),
        ),
    };
    let mut response = proposals::problem(
        status,
        code,
        "לא ניתן להשלים את פעולת שאלת המוצר.",
        retry.is_some(),
        None,
    );
    if let Some(seconds) = retry {
        response.headers_mut().insert(
            header::RETRY_AFTER,
            HeaderValue::from_str(&seconds.to_string()).expect("bounded retry"),
        );
    }
    response
}
