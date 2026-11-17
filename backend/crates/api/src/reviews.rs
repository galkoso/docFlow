use crate::{
    AppState, ErrorBody, auth,
    proposals::{self, JobStatus},
    review_configuration::NoQuery,
};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use specspace_application::{
    comparison_reader::ComparisonReadError,
    review_context::ReviewContextError,
    review_snapshot::{
        ReviewSnapshot, ReviewSnapshotFiles, ReviewSnapshotHistory, ReviewSnapshotMedia,
    },
    review_submission::{
        ReviewSubmissionAccepted, ReviewSubmissionRequest, ValidatedReviewSubmission,
    },
};
use specspace_infrastructure::{
    review_snapshots::SnapshotError, review_submissions::SubmissionError, sessions::SessionIdentity,
};
use uuid::Uuid;
pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/proposals/{proposal}/submit",
            post(submit).layer(DefaultBodyLimit::max(8192)),
        )
        .route(
            "/api/v1/proposals/{proposal}/review/snapshots",
            get(history),
        )
        .route(
            "/api/v1/proposals/{proposal}/review/snapshots/{snapshot}",
            get(snapshot),
        )
        .route(
            "/api/v1/proposals/{proposal}/review/snapshots/{snapshot}/files",
            get(files),
        )
        .route(
            "/api/v1/proposals/{proposal}/review/snapshots/{snapshot}/media",
            get(media),
        )
}
#[derive(Deserialize, utoipa::IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in=Query)]
pub struct PageQuery {
    pub offset: Option<i32>,
    pub limit: Option<i32>,
}
#[derive(Deserialize, utoipa::IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in=Query)]
pub struct HistoryQuery {
    pub after: Option<Uuid>,
    pub limit: Option<i32>,
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
#[utoipa::path(post,path="/api/v1/proposals/{proposal}/submit",operation_id="submitProposalReview",security(("sessionCookie"=[])),
    params(("proposal"=Uuid,Path),("X-Expected-Actor"=Uuid,Header),("Origin"=String,Header),("X-XSRF-TOKEN"=String,Header),("Idempotency-Key"=String,Header)),request_body=ReviewSubmissionRequest,
    responses((status=202,body=ReviewSubmissionAccepted,description="Submission queued or its immutable receipt replayed. Snapshot, PR and approval completion are separate.",headers(("Location"=String,description="Private actor-bound job status URL"),("Retry-After"=String))),
    (status=400,body=ErrorBody),(status=401,body=ErrorBody),(status=403,body=ErrorBody),(status=404,body=ErrorBody),(status=409,body=ErrorBody),(status=413,body=ErrorBody),(status=422,body=ErrorBody),(status=429,body=ErrorBody),(status=503,body=ErrorBody)))]
pub(crate) async fn submit(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, axum::extract::rejection::PathRejection>,
    query: Result<Query<NoQuery>, axum::extract::rejection::QueryRejection>,
    body: Result<Json<ReviewSubmissionRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let actor = match actor(&state, &headers, true).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    let (Ok(Path(proposal)), Ok(_)) = (path, query) else {
        return proposals::invalid_query();
    };
    let request = match body {
        Ok(Json(body)) => body,
        Err(error) => {
            return problem(
                if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
                    StatusCode::PAYLOAD_TOO_LARGE
                } else {
                    StatusCode::BAD_REQUEST
                },
                "INVALID_REVIEW_SUBMISSION",
                false,
            );
        }
    };
    let key = match proposals::operation_key(&headers) {
        Ok(key) => key,
        Err(error) => return error.into_response(),
    };
    let intent = match ValidatedReviewSubmission::new(actor, proposal, request) {
        Ok(intent) => intent,
        Err(code) => return problem(StatusCode::UNPROCESSABLE_ENTITY, code, false),
    };
    match state.review_submissions.reserve(&intent, &key).await {
        Ok(ticket) => {
            let location = format!("/api/v1/jobs/{}", ticket.job_id);
            let mut response = (StatusCode::ACCEPTED, Json(ticket)).into_response();
            response.headers_mut().insert(
                header::LOCATION,
                HeaderValue::from_str(&location).expect("UUID location is a valid header"),
            );
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("2"));
            response
        }
        Err(error) => failure(error),
    }
}
pub(crate) async fn job_status(
    state: &AppState,
    headers: &HeaderMap,
    actor: &SessionIdentity,
    job: Uuid,
) -> Option<Response> {
    let exists:Result<bool,_>=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM review_submissions s JOIN jobs j ON j.id=s.job_id AND j.organization_id=s.organization_id AND j.kind='SUBMIT_REVIEW' WHERE s.job_id=$1 AND s.actor_id=$2)")
        .bind(job).bind(actor.user_id).fetch_one(&state.database).await;
    match exists {
        Ok(false) => return None,
        Err(_) => return Some(failure(SubmissionError::Unavailable)),
        _ => {}
    }
    if let Err(error) = auth::verify_actor(headers, actor) {
        return Some(error.into_response());
    }
    Some(
        match state.review_submissions.status(actor.user_id, job).await {
            Ok(status) => Json(JobStatus {
                job_id: job,
                kind: "SUBMIT_REVIEW".into(),
                state: status.state,
                attempts: status.attempts,
                proposal_id: Some(status.proposal_id),
                phase: status.phase,
                error_code: status.error_code,
                creation: None,
                save: None,
                save_failure: None,
                comparison: None,
                review: status.publication,
            })
            .into_response(),
            Err(error) => failure(error),
        },
    )
}
#[utoipa::path(get,path="/api/v1/proposals/{proposal}/review/snapshots",operation_id="proposalReviewSnapshots",security(("sessionCookie"=[])),
    params(("proposal"=Uuid,Path),("X-Expected-Actor"=Uuid,Header),HistoryQuery),responses((status=200,body=ReviewSnapshotHistory),(status=400,body=ErrorBody),(status=401,body=ErrorBody),(status=404,body=ErrorBody),(status=409,body=ErrorBody),(status=503,body=ErrorBody)))]
pub(crate) async fn history(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, axum::extract::rejection::PathRejection>,
    query: Result<Query<HistoryQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let actor = match actor(&state, &headers, false).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    let (Ok(Path(proposal)), Ok(Query(query))) = (path, query) else {
        return proposals::invalid_query();
    };
    match state
        .review_snapshots
        .history(actor, proposal, query.after, query.limit.unwrap_or(20))
        .await
    {
        Ok(history) => Json(history).into_response(),
        Err(error) => snapshot_failure(error),
    }
}
#[utoipa::path(get,path="/api/v1/proposals/{proposal}/review/snapshots/{snapshot}",operation_id="proposalReviewSnapshot",security(("sessionCookie"=[])),
    params(("proposal"=Uuid,Path),("snapshot"=Uuid,Path),("X-Expected-Actor"=Uuid,Header)),responses((status=200,body=ReviewSnapshot),(status=400,body=ErrorBody),(status=401,body=ErrorBody),(status=404,body=ErrorBody),(status=409,body=ErrorBody),(status=503,body=ErrorBody)))]
pub(crate) async fn snapshot(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<(Uuid, Uuid)>, axum::extract::rejection::PathRejection>,
    query: Result<Query<NoQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let actor = match actor(&state, &headers, false).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    let (Ok(Path((proposal, snapshot))), Ok(_)) = (path, query) else {
        return proposals::invalid_query();
    };
    match state
        .review_snapshots
        .get(actor, proposal, snapshot)
        .await
        .and_then(|snapshot| snapshot.into_read_model())
    {
        Ok(snapshot) => Json(snapshot).into_response(),
        Err(error) => snapshot_failure(error),
    }
}
#[utoipa::path(get,path="/api/v1/proposals/{proposal}/review/snapshots/{snapshot}/files",operation_id="proposalReviewSnapshotFiles",security(("sessionCookie"=[])),
    params(("proposal"=Uuid,Path),("snapshot"=Uuid,Path),("X-Expected-Actor"=Uuid,Header),PageQuery),responses((status=200,body=ReviewSnapshotFiles),(status=400,body=ErrorBody),(status=401,body=ErrorBody),(status=404,body=ErrorBody),(status=409,body=ErrorBody),(status=503,body=ErrorBody)))]
pub(crate) async fn files(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<(Uuid, Uuid)>, axum::extract::rejection::PathRejection>,
    query: Result<Query<PageQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let actor = match actor(&state, &headers, false).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    let (Ok(Path((proposal, snapshot))), Ok(Query(query))) = (path, query) else {
        return proposals::invalid_query();
    };
    match state
        .review_snapshots
        .files(
            actor,
            proposal,
            snapshot,
            query.offset.unwrap_or(0),
            query.limit.unwrap_or(50),
        )
        .await
    {
        Ok(page) => {
            let end = page.offset + page.files.len() as i32;
            Json(ReviewSnapshotFiles {
                snapshot_id: page.snapshot_id,
                offset: page.offset,
                total: page.total,
                next_offset: (end < page.total).then_some(end),
                files: page.files,
            })
            .into_response()
        }
        Err(error) => snapshot_failure(error),
    }
}
#[utoipa::path(get,path="/api/v1/proposals/{proposal}/review/snapshots/{snapshot}/media",operation_id="proposalReviewSnapshotMedia",security(("sessionCookie"=[])),
    params(("proposal"=Uuid,Path),("snapshot"=Uuid,Path),("X-Expected-Actor"=Uuid,Header),PageQuery),responses((status=200,body=ReviewSnapshotMedia),(status=400,body=ErrorBody),(status=401,body=ErrorBody),(status=404,body=ErrorBody),(status=409,body=ErrorBody),(status=503,body=ErrorBody)))]
pub(crate) async fn media(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<(Uuid, Uuid)>, axum::extract::rejection::PathRejection>,
    query: Result<Query<PageQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let actor = match actor(&state, &headers, false).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    let (Ok(Path((proposal, snapshot))), Ok(Query(query))) = (path, query) else {
        return proposals::invalid_query();
    };
    match state
        .review_snapshots
        .media(
            actor,
            proposal,
            snapshot,
            query.offset.unwrap_or(0),
            query.limit.unwrap_or(50),
        )
        .await
    {
        Ok(page) => Json(page).into_response(),
        Err(error) => snapshot_failure(error),
    }
}
fn snapshot_failure(error: SnapshotError) -> Response {
    match error {
        SnapshotError::Submission(error) => failure(error),
        _ => problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "REVIEW_EVIDENCE_INVALID",
            false,
        ),
    }
}
fn failure(error: SubmissionError) -> Response {
    if error == SubmissionError::QuotaExceeded {
        let mut response = problem(
            StatusCode::TOO_MANY_REQUESTS,
            "REVIEW_SUBMISSION_LIMIT",
            true,
        );
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("60"));
        return response;
    }
    let (status, code, retry) = match error {
        SubmissionError::NotFound
        | SubmissionError::Context(ReviewContextError::Read(ComparisonReadError::NotFound)) => {
            (StatusCode::NOT_FOUND, "NOT_FOUND", false)
        }
        SubmissionError::Forbidden | SubmissionError::Context(ReviewContextError::Forbidden) => {
            (StatusCode::FORBIDDEN, "REVIEW_FORBIDDEN", false)
        }
        SubmissionError::Busy | SubmissionError::Context(ReviewContextError::Busy) => {
            (StatusCode::CONFLICT, "PROPOSAL_BUSY", false)
        }
        SubmissionError::IdempotencyMismatch => {
            (StatusCode::CONFLICT, "IDEMPOTENCY_MISMATCH", false)
        }
        SubmissionError::Context(ReviewContextError::Stale) | SubmissionError::LeaseLost => {
            (StatusCode::CONFLICT, "REVIEW_CONTEXT_CHANGED", false)
        }
        SubmissionError::Context(ReviewContextError::StateChanged) => {
            (StatusCode::CONFLICT, "PROPOSAL_NOT_EDITABLE", false)
        }
        SubmissionError::Context(ReviewContextError::OwnerNotEligible) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "REVIEW_OWNER_NOT_ELIGIBLE",
            false,
        ),
        SubmissionError::InvalidQuery => (StatusCode::BAD_REQUEST, "INVALID_QUERY", false),
        SubmissionError::InvalidIntent => (
            StatusCode::SERVICE_UNAVAILABLE,
            "REVIEW_EVIDENCE_INVALID",
            false,
        ),
        SubmissionError::Context(
            ReviewContextError::InvalidConfiguration
            | ReviewContextError::Read(ComparisonReadError::InvalidConfiguration),
        ) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "REVIEW_SETUP_REQUIRED",
            false,
        ),
        _ => (StatusCode::SERVICE_UNAVAILABLE, "REVIEW_UNAVAILABLE", true),
    };
    problem(status, code, retry)
}
fn problem(status: StatusCode, code: &'static str, retryable: bool) -> Response {
    proposals::problem(
        status,
        code,
        "לא ניתן להשלים את פעולת הסקירה. יש לבדוק את הגרסה וההרשאות לפני ניסיון נוסף.",
        retryable,
        None,
    )
}
