use crate::{AppState, ErrorBody, auth, proposals, review_configuration::NoQuery};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Deserialize;
use specspace_application::official_review::{
    OfficialReviewHistory, OfficialReviewRecorded, OfficialReviewRequest,
};
use specspace_infrastructure::official_reviews::OfficialReviewError;
use uuid::Uuid;

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/api/v1/proposals/{proposal}/review/snapshots/{snapshot}/decisions",
        get(history)
            .post(record)
            .layer(DefaultBodyLimit::max(128 * 1024)),
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryQuery {
    after: Option<Uuid>,
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
#[utoipa::path(get,path="/api/v1/proposals/{proposal}/review/snapshots/{snapshot}/decisions",operation_id="proposalOfficialReviewHistory",security(("sessionCookie"=[])),
    params(("proposal"=Uuid,Path),("snapshot"=Uuid,Path),("X-Expected-Actor"=Uuid,Header),("after"=Option<Uuid>,Query)),
    responses((status=200,body=OfficialReviewHistory),(status=400,body=ErrorBody),(status=401,body=ErrorBody),(status=404,body=ErrorBody),(status=409,body=ErrorBody),(status=503,body=ErrorBody)))]
pub(crate) async fn history(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<(Uuid, Uuid)>, axum::extract::rejection::PathRejection>,
    query: Result<Query<HistoryQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let actor = match identity(&state, &headers, false).await {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let (Ok(Path((proposal, snapshot))), Ok(Query(query))) = (path, query) else {
        return proposals::invalid_query();
    };
    match state
        .official_reviews
        .history(actor, proposal, snapshot, query.after)
        .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => failure(e),
    }
}
#[utoipa::path(post,path="/api/v1/proposals/{proposal}/review/snapshots/{snapshot}/decisions",operation_id="recordProposalOfficialReview",security(("sessionCookie"=[])),
    params(("proposal"=Uuid,Path),("snapshot"=Uuid,Path),("X-Expected-Actor"=Uuid,Header),("Origin"=String,Header),("X-XSRF-TOKEN"=String,Header)),request_body=OfficialReviewRequest,
    responses((status=201,body=OfficialReviewRecorded),(status=200,body=OfficialReviewRecorded,description="Original decision receipt replayed; current approval validity is separate"),
    (status=400,body=ErrorBody),(status=401,body=ErrorBody),(status=403,body=ErrorBody),(status=404,body=ErrorBody),(status=409,body=ErrorBody),(status=413,body=ErrorBody),(status=429,body=ErrorBody),(status=503,body=ErrorBody)))]
pub(crate) async fn record(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<(Uuid, Uuid)>, axum::extract::rejection::PathRejection>,
    query: Result<Query<NoQuery>, axum::extract::rejection::QueryRejection>,
    body: Result<Json<OfficialReviewRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let actor = match identity(&state, &headers, true).await {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let (Ok(Path((proposal, snapshot))), Ok(_)) = (path, query) else {
        return proposals::invalid_query();
    };
    let request = match body {
        Ok(Json(v)) => v,
        Err(e) => {
            return proposals::problem(
                if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
                    StatusCode::PAYLOAD_TOO_LARGE
                } else {
                    StatusCode::BAD_REQUEST
                },
                "INVALID_OFFICIAL_REVIEW",
                "פרטי החלטת הסקירה אינם תקינים.",
                false,
                None,
            );
        }
    };
    match state
        .official_reviews
        .submit(
            actor,
            proposal,
            snapshot,
            request,
            state.bound_source.as_ref(),
        )
        .await
    {
        Ok(v) => (
            if v.replayed {
                StatusCode::OK
            } else {
                StatusCode::CREATED
            },
            Json(v),
        )
            .into_response(),
        Err(e) => failure(e),
    }
}
fn failure(error: OfficialReviewError) -> Response {
    use OfficialReviewError::*;
    let (status, code, retry) = match error {
        NotFound => (StatusCode::NOT_FOUND, "NOT_FOUND", None),
        Forbidden => (StatusCode::FORBIDDEN, "REVIEW_FORBIDDEN", None),
        Invalid => (StatusCode::BAD_REQUEST, "INVALID_OFFICIAL_REVIEW", None),
        Stale => (StatusCode::CONFLICT, "REVIEW_CONTEXT_CHANGED", None),
        BaseOutdated => (StatusCode::CONFLICT, "REVIEW_BASE_OUTDATED", None),
        UiUndecided => (StatusCode::CONFLICT, "UI_IMPACT_UNDECIDED", None),
        SectionsIncomplete => (StatusCode::CONFLICT, "REVIEW_SECTIONS_INCOMPLETE", None),
        IdempotencyMismatch => (StatusCode::CONFLICT, "IDEMPOTENCY_MISMATCH", None),
        Quota => (
            StatusCode::TOO_MANY_REQUESTS,
            "OFFICIAL_REVIEW_LIMIT",
            Some(600),
        ),
        ProviderRateLimited(delay) => (
            StatusCode::TOO_MANY_REQUESTS,
            "REVIEW_PROVIDER_RATE_LIMIT",
            Some(delay),
        ),
        Capacity => (StatusCode::SERVICE_UNAVAILABLE, "REVIEW_CAPACITY", Some(1)),
        ProviderUnavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            "REVIEW_PROVIDER_UNAVAILABLE",
            Some(30),
        ),
        Unavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            "OFFICIAL_REVIEW_UNAVAILABLE",
            Some(1),
        ),
    };
    let mut response = proposals::problem(
        status,
        code,
        "לא ניתן להשלים את החלטת הסקירה. יש לבדוק גרסה, תחום סקירה והרשאות.",
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
