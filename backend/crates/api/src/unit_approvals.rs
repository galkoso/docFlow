use crate::{AppState, ErrorBody, auth, proposals, review_configuration::NoQuery};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Deserialize;
use specspace_application::{review_configuration::ReviewArea, unit_approval::*};
use specspace_infrastructure::{
    official_reviews::OfficialReviewError, unit_approvals::UnitApprovalError,
};
use uuid::Uuid;

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/api/v1/proposals/{proposal}/review/snapshots/{snapshot}/unit-approvals",
        get(page).post(approve).layer(DefaultBodyLimit::max(4096)),
    )
}
#[derive(Deserialize, utoipa::IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in=Query)]
pub struct ApprovalQuery {
    expected_fingerprint: String,
    domain: ReviewArea,
    offset: Option<i32>,
    limit: Option<i32>,
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
#[utoipa::path(get,path="/api/v1/proposals/{proposal}/review/snapshots/{snapshot}/unit-approvals",operation_id="proposalUnitApprovalStatus",security(("sessionCookie"=[])),
 params(("proposal"=Uuid,Path),("snapshot"=Uuid,Path),("X-Expected-Actor"=Uuid,Header),ApprovalQuery),
 responses((status=200,body=UnitApprovalPage,description="Caller/domain individual approval status pinned to the sealed snapshot; no merge authority"),(status=400,body=ErrorBody),(status=401,body=ErrorBody),(status=403,body=ErrorBody),(status=404,body=ErrorBody),(status=409,body=ErrorBody),(status=503,body=ErrorBody)))]
pub(crate) async fn page(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<(Uuid, Uuid)>, axum::extract::rejection::PathRejection>,
    query: Result<Query<ApprovalQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let actor = match identity(&state, &headers, false).await {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let (Ok(Path((proposal, snapshot))), Ok(Query(q))) = (path, query) else {
        return proposals::invalid_query();
    };
    let request = UnitApprovalPageRequest {
        expected_fingerprint: q.expected_fingerprint,
        domain: q.domain,
        offset: q.offset.unwrap_or(0),
        limit: q.limit.unwrap_or(20),
    };
    match state
        .unit_approvals
        .page(actor, proposal, snapshot, request)
        .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => failure(e),
    }
}
#[utoipa::path(post,path="/api/v1/proposals/{proposal}/review/snapshots/{snapshot}/unit-approvals",operation_id="approveProposalReviewUnit",security(("sessionCookie"=[])),
 params(("proposal"=Uuid,Path),("snapshot"=Uuid,Path),("X-Expected-Actor"=Uuid,Header),("Origin"=String,Header),("X-XSRF-TOKEN"=String,Header)),request_body=UnitApprovalRequest,
 responses((status=201,body=UnitApprovalRecorded,description="Explicit approval of one fully acknowledged mandatory unit; no proposal state or Git changes"),(status=200,body=UnitApprovalRecorded,description="Original receipt replayed; current approval validity remains separate"),(status=400,body=ErrorBody),(status=401,body=ErrorBody),(status=403,body=ErrorBody),(status=404,body=ErrorBody),(status=409,body=ErrorBody),(status=413,body=ErrorBody),(status=429,body=ErrorBody),(status=503,body=ErrorBody)))]
pub(crate) async fn approve(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<(Uuid, Uuid)>, axum::extract::rejection::PathRejection>,
    query: Result<Query<NoQuery>, axum::extract::rejection::QueryRejection>,
    body: Result<Json<UnitApprovalRequest>, axum::extract::rejection::JsonRejection>,
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
                "INVALID_UNIT_APPROVAL",
                None,
            );
        }
    };
    match state
        .unit_approvals
        .approve(actor, proposal, snapshot, request)
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
fn failure(error: UnitApprovalError) -> Response {
    use OfficialReviewError as O;
    use UnitApprovalError as E;
    let (status, code, retry) = match error {
        E::NotFound | E::Authority(O::NotFound) => (StatusCode::NOT_FOUND, "NOT_FOUND", None),
        E::Authority(O::Forbidden) => (StatusCode::FORBIDDEN, "REVIEW_FORBIDDEN", None),
        E::Authority(O::Stale) => (StatusCode::CONFLICT, "REVIEW_CONTEXT_CHANGED", None),
        E::Invalid | E::Authority(O::Invalid) => {
            (StatusCode::BAD_REQUEST, "INVALID_UNIT_APPROVAL", None)
        }
        E::IncompleteCoverage => (StatusCode::CONFLICT, "REVIEW_UNIT_UNVIEWED", None),
        E::EvidenceUnavailable => (
            StatusCode::CONFLICT,
            "REVIEW_UNIT_EVIDENCE_UNAVAILABLE",
            None,
        ),
        E::Conflict => (StatusCode::CONFLICT, "UNIT_APPROVAL_CONFLICT", None),
        E::Quota => (
            StatusCode::TOO_MANY_REQUESTS,
            "UNIT_APPROVAL_LIMIT",
            Some(600),
        ),
        E::Unavailable | E::Authority(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "UNIT_APPROVAL_UNAVAILABLE",
            Some(1),
        ),
    };
    problem(status, code, retry)
}
fn problem(status: StatusCode, code: &'static str, retry: Option<u32>) -> Response {
    let mut response = proposals::problem(
        status,
        code,
        "לא ניתן להשלים את אישור הפריט. יש לבדוק גרסה והרשאות ולהשלים קריאה לפני אישור.",
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
