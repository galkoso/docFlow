use crate::{AppState, ErrorBody, auth, proposals, review_configuration::NoQuery};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use specspace_application::review_check_operations::{
    ReviewCheckOperation, ReviewCheckRecoveryAccepted, ReviewCheckRecoveryRequest,
};
use specspace_infrastructure::review_checks::{CheckRecoveryMode, CheckStoreError as E};
use uuid::Uuid;
pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/proposals/{proposal}/review/checks/{operation}",
            get(read),
        )
        .route(
            "/api/v1/proposals/{proposal}/review/checks/{operation}/recover",
            post(recover).layer(DefaultBodyLimit::max(1024)),
        )
}
async fn actor(
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
#[utoipa::path(get,path="/api/v1/proposals/{proposal}/review/checks/{operation}",operation_id="reviewCheckOperation",security(("sessionCookie"=[])),
    params(("proposal"=Uuid,Path),("operation"=Uuid,Path),("X-Expected-Actor"=Uuid,Header)),
    responses((status=200,body=ReviewCheckOperation,description="Admin-only durable journal status; provider state is not refreshed."),(status=400,body=ErrorBody),(status=401,body=ErrorBody),(status=403,body=ErrorBody),(status=404,body=ErrorBody),(status=409,body=ErrorBody),(status=503,body=ErrorBody)))]
pub(crate) async fn read(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<(Uuid, Uuid)>, axum::extract::rejection::PathRejection>,
    query: Result<Query<NoQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let actor = match actor(&state, &headers, false).await {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let (Ok(Path((proposal, operation))), Ok(_)) = (path, query) else {
        return proposals::invalid_query();
    };
    match state.review_checks.status(actor, proposal, operation).await {
        Ok(value) => Json(value).into_response(),
        Err(e) => failure(e),
    }
}
#[utoipa::path(post,path="/api/v1/proposals/{proposal}/review/checks/{operation}/recover",operation_id="recoverReviewCheckOperation",security(("sessionCookie"=[])),
    params(("proposal"=Uuid,Path),("operation"=Uuid,Path),("X-Expected-Actor"=Uuid,Header),("Origin"=String,Header),("X-XSRF-TOKEN"=String,Header)),request_body=ReviewCheckRecoveryRequest,
    responses((status=202,body=ReviewCheckRecoveryAccepted,description="Admin recovery accepted or exactly replayed; original publication authority and markers remain in force.",headers(("Location"=String),("Retry-After"=String))),
    (status=400,body=ErrorBody),(status=401,body=ErrorBody),(status=403,body=ErrorBody),(status=404,body=ErrorBody),(status=409,body=ErrorBody),(status=413,body=ErrorBody),(status=429,body=ErrorBody),(status=503,body=ErrorBody)))]
pub(crate) async fn recover(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<(Uuid, Uuid)>, axum::extract::rejection::PathRejection>,
    query: Result<Query<NoQuery>, axum::extract::rejection::QueryRejection>,
    body: Result<Json<ReviewCheckRecoveryRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let actor = match actor(&state, &headers, true).await {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let (Ok(Path((proposal, operation))), Ok(_)) = (path, query) else {
        return proposals::invalid_query();
    };
    let request = match body {
        Ok(Json(v)) => v,
        Err(e) => {
            return problem(
                if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
                    StatusCode::PAYLOAD_TOO_LARGE
                } else {
                    StatusCode::BAD_REQUEST
                },
                "INVALID_CHECK_RECOVERY",
                false,
            );
        }
    };
    match state
        .review_checks
        .recover(
            actor,
            proposal,
            operation,
            request.request_id,
            &request.expected_request_hash,
        )
        .await
    {
        Ok(receipt) => {
            let accepted = ReviewCheckRecoveryAccepted {
                proposal_id: proposal,
                operation_id: operation,
                request_id: receipt.request_id,
                mode: match receipt.mode {
                    CheckRecoveryMode::RetryUnstarted => "RETRY_UNSTARTED",
                    CheckRecoveryMode::ObserveStarted => "OBSERVE_STARTED",
                }
                .to_owned(),
                max_attempts: receipt.max_attempts,
                replayed: receipt.replayed,
            };
            let mut response = (StatusCode::ACCEPTED, Json(accepted)).into_response();
            response.headers_mut().insert(
                header::LOCATION,
                HeaderValue::from_str(&format!(
                    "/api/v1/proposals/{proposal}/review/checks/{operation}"
                ))
                .expect("UUID path"),
            );
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("2"));
            response
        }
        Err(e) => failure(e),
    }
}
fn problem(status: StatusCode, code: &'static str, retryable: bool) -> Response {
    proposals::problem(
        status,
        code,
        "לא ניתן להשלים את פעולת הבדיקה. יש לבדוק את המצב וההרשאות לפני ניסיון נוסף.",
        retryable,
        None,
    )
}
fn failure(error: E) -> Response {
    let (status, code, retry, delay) = match error {
        E::Invalid => (
            StatusCode::BAD_REQUEST,
            "INVALID_CHECK_OPERATION",
            false,
            None,
        ),
        E::NotFound => (
            StatusCode::NOT_FOUND,
            "CHECK_OPERATION_NOT_FOUND",
            false,
            None,
        ),
        E::Forbidden => (
            StatusCode::FORBIDDEN,
            "CHECK_OPERATION_FORBIDDEN",
            false,
            None,
        ),
        E::Stale | E::NotConfigured => (
            StatusCode::CONFLICT,
            "CHECK_RECOVERY_CONTEXT_CHANGED",
            false,
            None,
        ),
        E::Busy => (
            StatusCode::CONFLICT,
            "CHECK_RECOVERY_NOT_STOPPED",
            false,
            None,
        ),
        E::IdempotencyMismatch => (
            StatusCode::CONFLICT,
            "CHECK_RECOVERY_IDEMPOTENCY_MISMATCH",
            false,
            None,
        ),
        E::Quota => (
            StatusCode::TOO_MANY_REQUESTS,
            "CHECK_RECOVERY_LIMIT",
            true,
            Some(600),
        ),
        E::Cooldown(n) => (
            StatusCode::TOO_MANY_REQUESTS,
            "CHECK_PROVIDER_COOLDOWN",
            true,
            Some(n),
        ),
        _ => (
            StatusCode::SERVICE_UNAVAILABLE,
            "CHECK_OPERATION_UNAVAILABLE",
            true,
            None,
        ),
    };
    let mut response = problem(status, code, retry);
    if let Some(delay) = delay {
        response.headers_mut().insert(
            header::RETRY_AFTER,
            HeaderValue::from_str(&delay.to_string()).expect("bounded integer"),
        );
    }
    response
}
