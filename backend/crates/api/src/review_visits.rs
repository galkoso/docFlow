use crate::{AppState, ErrorBody, auth, proposals, review_configuration::NoQuery};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use specspace_application::{
    comparison_file::{FileReadError, discussion::AnchorSide, review_unit::UnitReadError},
    comparison_reader::ComparisonReadError,
    review_configuration::ReviewArea,
    review_visits::{
        ReviewCoverage, ReviewCoverageRequest, ReviewSourceRequest, ReviewSourceWindow,
        ReviewVisitRecorded, ReviewVisitRequest,
    },
};
use specspace_infrastructure::{official_reviews::OfficialReviewError, review_visits::VisitError};
use uuid::Uuid;
pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/proposals/{proposal}/review/snapshots/{snapshot}/units/{unit}/source",
            get(source),
        )
        .route(
            "/api/v1/proposals/{proposal}/review/snapshots/{snapshot}/coverage",
            get(coverage),
        )
        .route(
            "/api/v1/proposals/{proposal}/review/snapshots/{snapshot}/visits",
            post(acknowledge).layer(DefaultBodyLimit::max(4096)),
        )
}
#[derive(Deserialize, utoipa::IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in=Query)]
pub struct SourceQuery {
    expected_fingerprint: String,
    domain: ReviewArea,
    side: AnchorSide,
    offset: usize,
}
#[derive(Deserialize, utoipa::IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in=Query)]
pub struct CoverageQuery {
    expected_fingerprint: String,
    domain: ReviewArea,
    offset: Option<i32>,
    limit: Option<i32>,
}
#[utoipa::path(get,path="/api/v1/proposals/{proposal}/review/snapshots/{snapshot}/coverage",operation_id="proposalReviewCoverage",security(("sessionCookie"=[])),
 params(("proposal"=Uuid,Path),("snapshot"=Uuid,Path),("X-Expected-Actor"=Uuid,Header),CoverageQuery),
 responses((status=200,body=ReviewCoverage,description="Caller/domain acknowledged-byte coverage only; no approval or merge authority"),(status=400,body=ErrorBody),(status=401,body=ErrorBody),(status=403,body=ErrorBody),(status=404,body=ErrorBody),(status=409,body=ErrorBody),(status=503,body=ErrorBody)))]
pub(crate) async fn coverage(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<(Uuid, Uuid)>, axum::extract::rejection::PathRejection>,
    query: Result<Query<CoverageQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let actor = match identity(&state, &headers, false).await {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let (Ok(Path((proposal, snapshot))), Ok(Query(q))) = (path, query) else {
        return proposals::invalid_query();
    };
    let request = ReviewCoverageRequest {
        expected_fingerprint: q.expected_fingerprint,
        domain: q.domain,
        offset: q.offset.unwrap_or(0),
        limit: q.limit.unwrap_or(20),
    };
    match state
        .review_visits
        .coverage(actor, proposal, snapshot, request)
        .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => failure(e),
    }
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
#[utoipa::path(get,path="/api/v1/proposals/{proposal}/review/snapshots/{snapshot}/units/{unit}/source",operation_id="proposalReviewSourceWindow",security(("sessionCookie"=[])),
 params(("proposal"=Uuid,Path),("snapshot"=Uuid,Path),("unit"=String,Path),("X-Expected-Actor"=Uuid,Header),SourceQuery),
 responses((status=200,body=ReviewSourceWindow,description="Verified bounded source delivery; no visit or approval is recorded"),(status=400,body=ErrorBody),(status=401,body=ErrorBody),(status=403,body=ErrorBody),(status=404,body=ErrorBody),(status=409,body=ErrorBody),(status=422,body=ErrorBody),(status=429,body=ErrorBody),(status=503,body=ErrorBody)))]
pub(crate) async fn source(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<(Uuid, Uuid, String)>, axum::extract::rejection::PathRejection>,
    query: Result<Query<SourceQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let actor = match identity(&state, &headers, false).await {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let (Ok(Path((proposal, snapshot, unit))), Ok(Query(q))) = (path, query) else {
        return proposals::invalid_query();
    };
    let request = ReviewSourceRequest {
        expected_fingerprint: q.expected_fingerprint,
        domain: q.domain,
        unit_id: unit,
        side: q.side,
        offset: q.offset,
    };
    match state
        .review_visits
        .deliver(actor, proposal, snapshot, request)
        .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => failure(e),
    }
}
#[utoipa::path(post,path="/api/v1/proposals/{proposal}/review/snapshots/{snapshot}/visits",operation_id="acknowledgeProposalReviewSource",security(("sessionCookie"=[])),
 params(("proposal"=Uuid,Path),("snapshot"=Uuid,Path),("X-Expected-Actor"=Uuid,Header),("Origin"=String,Header),("X-XSRF-TOKEN"=String,Header)),request_body=ReviewVisitRequest,
 responses((status=201,body=ReviewVisitRecorded,description="Explicit acknowledgement of exactly one delivered source window; no approval or whole-unit coverage implied"),(status=200,body=ReviewVisitRecorded,description="Exact original receipt replayed"),(status=400,body=ErrorBody),(status=401,body=ErrorBody),(status=403,body=ErrorBody),(status=404,body=ErrorBody),(status=409,body=ErrorBody),(status=410,body=ErrorBody),(status=413,body=ErrorBody),(status=429,body=ErrorBody),(status=503,body=ErrorBody)))]
pub(crate) async fn acknowledge(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<(Uuid, Uuid)>, axum::extract::rejection::PathRejection>,
    query: Result<Query<NoQuery>, axum::extract::rejection::QueryRejection>,
    body: Result<Json<ReviewVisitRequest>, axum::extract::rejection::JsonRejection>,
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
            return problem(
                if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
                    StatusCode::PAYLOAD_TOO_LARGE
                } else {
                    StatusCode::BAD_REQUEST
                },
                "INVALID_REVIEW_VISIT",
                None,
            );
        }
    };
    match state
        .review_visits
        .acknowledge(actor, proposal, snapshot, request)
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
fn failure(error: VisitError) -> Response {
    use ComparisonReadError as C;
    use FileReadError as F;
    use OfficialReviewError as O;
    let (status, code, retry) = match error {
        VisitError::Authority(O::NotFound)
        | VisitError::Source(UnitReadError::Source(F::NotFound | F::Access(C::NotFound))) => {
            (StatusCode::NOT_FOUND, "NOT_FOUND", None)
        }
        VisitError::Authority(O::Forbidden) => (StatusCode::FORBIDDEN, "REVIEW_FORBIDDEN", None),
        VisitError::Authority(O::Stale)
        | VisitError::Source(UnitReadError::Source(F::Access(C::ContextChanged))) => {
            (StatusCode::CONFLICT, "REVIEW_CONTEXT_CHANGED", None)
        }
        VisitError::Source(UnitReadError::Source(F::NotReady)) => {
            (StatusCode::CONFLICT, "REVIEW_SOURCE_NOT_READY", None)
        }
        VisitError::Source(UnitReadError::Source(F::UnsupportedKind | F::TooLarge)) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "REVIEW_SOURCE_UNSUPPORTED",
            None,
        ),
        VisitError::Invalid | VisitError::Source(UnitReadError::InvalidWindow) => {
            (StatusCode::BAD_REQUEST, "INVALID_REVIEW_SOURCE", None)
        }
        VisitError::Conflict => (StatusCode::CONFLICT, "REVIEW_VISIT_CONFLICT", None),
        VisitError::Expired => (StatusCode::GONE, "REVIEW_DELIVERY_EXPIRED", None),
        VisitError::Quota => (
            StatusCode::TOO_MANY_REQUESTS,
            "REVIEW_SOURCE_LIMIT",
            Some(600),
        ),
        VisitError::Backoff(delay) => (
            StatusCode::TOO_MANY_REQUESTS,
            "REVIEW_PROVIDER_RATE_LIMIT",
            Some(delay),
        ),
        VisitError::Source(UnitReadError::Source(F::Busy | F::Access(C::Busy))) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "REVIEW_SOURCE_CAPACITY",
            Some(1),
        ),
        VisitError::Source(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "REVIEW_SOURCE_UNAVAILABLE",
            Some(30),
        ),
        VisitError::Unavailable | VisitError::Authority(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "REVIEW_VISIT_UNAVAILABLE",
            Some(1),
        ),
    };
    problem(status, code, retry)
}
fn problem(status: StatusCode, code: &'static str, retry: Option<u32>) -> Response {
    let mut response = proposals::problem(
        status,
        code,
        "לא ניתן להשלים את קריאת המקור או אישור הצפייה. יש לבדוק גרסה, תחום סקירה והרשאות.",
        retry.is_some(),
        None,
    );
    if let Some(delay) = retry {
        response.headers_mut().insert(
            header::RETRY_AFTER,
            HeaderValue::from_str(&delay.to_string()).expect("integer retry delay"),
        );
    }
    response
}
