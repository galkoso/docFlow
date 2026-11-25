use crate::{AppState, ErrorBody, auth, proposals, review_configuration::NoQuery};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use specspace_application::review_readiness::ReviewReadiness;
use specspace_infrastructure::review_readiness::{PgReviewReadiness, ReadinessError};
use uuid::Uuid;

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/api/v1/proposals/{proposal}/review/snapshots/{snapshot}/readiness",
        get(stored),
    )
}
#[utoipa::path(get,path="/api/v1/proposals/{proposal}/review/snapshots/{snapshot}/readiness",operation_id="proposalStoredReviewReadiness",security(("sessionCookie"=[])),
    description="Stored evidence only. Provider refs are unverified; this report never authorizes approval, check publication or merge.",
    params(("proposal"=Uuid,Path),("snapshot"=Uuid,Path),("X-Expected-Actor"=Uuid,Header)),
    responses((status=200,body=ReviewReadiness),(status=400,body=ErrorBody),(status=401,body=ErrorBody),(status=404,body=ErrorBody),(status=409,body=ErrorBody),(status=503,body=ErrorBody)))]
pub(crate) async fn stored(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<(Uuid, Uuid)>, axum::extract::rejection::PathRejection>,
    query: Result<Query<NoQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let actor = match auth::authenticate(&state, &headers).await {
        Ok(actor) => actor,
        Err(e) => return e.into_response(),
    };
    if let Err(e) = auth::verify_actor(&headers, &actor) {
        return e.into_response();
    }
    let (Ok(Path((proposal, snapshot))), Ok(_)) = (path, query) else {
        return proposals::invalid_query();
    };
    match PgReviewReadiness::new(state.database.clone())
        .stored(actor.user_id, proposal, snapshot)
        .await
    {
        Ok(report) => Json(report).into_response(),
        Err(error) => {
            let (status, code, retry) = match error {
                ReadinessError::Invalid => {
                    (StatusCode::BAD_REQUEST, "INVALID_REVIEW_READINESS", false)
                }
                ReadinessError::NotFound => (StatusCode::NOT_FOUND, "NOT_FOUND", false),
                ReadinessError::Unavailable => (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "REVIEW_READINESS_UNAVAILABLE",
                    true,
                ),
            };
            let mut response = proposals::problem(
                status,
                code,
                "לא ניתן לקרוא את ראיות מוכנות הסקירה.",
                retry,
                None,
            );
            if retry {
                response
                    .headers_mut()
                    .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
            }
            response
        }
    }
}
