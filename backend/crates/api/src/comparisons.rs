pub(crate) mod image;
pub(crate) mod raw;
pub(crate) mod readable;

use crate::{
    AppState, ErrorBody, auth,
    proposals::{self, JobStatus},
};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::{Deserialize, Serialize};
use specspace_application::{
    book::BookError,
    comparison::Change,
    comparison_reader::{ComparisonReadError, ComparisonSummary, Selection},
    proposal::IdempotencyKey,
};
use specspace_git_adapter::CommitSha;
use specspace_infrastructure::{
    comparisons::{ComparisonStatus, ComparisonStoreError},
    sessions::SessionIdentity,
};
use uuid::Uuid;
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/proposals/{proposal}/diff", get(read))
        .merge(image::routes())
        .merge(raw::routes())
        .merge(readable::routes())
}
#[derive(Deserialize, utoipa::IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in=Query)]
pub struct DiffQuery {
    /// Exact replay identity for a new derived read; mutually exclusive with snapshot.
    pub request_id: Option<Uuid>,
    /// Durable snapshot returned by the initial request. Pages never reselect refs.
    pub snapshot: Option<Uuid>,
    pub head: Option<String>,
    pub target: Option<String>,
    pub offset: Option<usize>,
    pub limit: Option<usize>,
}
#[derive(Serialize, utoipa::ToSchema)]
pub struct DiffFiles {
    pub snapshot_id: Uuid,
    pub content_id: String,
    pub offset: usize,
    pub total: usize,
    pub next_offset: Option<usize>,
    pub files: Vec<Change>,
}
#[derive(Serialize, utoipa::ToSchema)]
pub struct DiffResponse {
    pub proposal_id: Uuid,
    pub snapshot_id: Uuid,
    pub job_id: Uuid,
    pub state: String,
    pub job_state: String,
    pub phase: String,
    pub attempts: i32,
    pub error_code: Option<String>,
    pub completed_at: Option<String>,
    pub current_head_sha: String,
    pub current_proposal_version: i64,
    pub current_proposal_state: String,
    pub context_changed: bool,
    pub historical_head: Option<bool>,
    pub summary: Option<ComparisonSummary>,
    pub page: Option<DiffFiles>,
}
#[derive(Serialize, utoipa::ToSchema)]
pub struct ComparisonJobResult {
    pub snapshot_id: Uuid,
    pub content_id: Option<String>,
    pub progress_path: String,
}
#[utoipa::path(get,path="/api/v1/proposals/{proposal}/diff",operation_id="proposalDiff",security(("sessionCookie"=[])),
    params(("proposal"=Uuid,Path),("X-Expected-Actor"=Uuid,Header,description="Captured owner of this derived read"),DiffQuery),
    responses((status=200,body=DiffResponse,description="Immutable ready page or explicit terminal failure status"),
    (status=202,body=DiffResponse,description="Derived comparison queued/loading; no complete result is claimed",headers(("Location"=String,description="Canonical snapshot status/page URL"),("Retry-After"=String,description="Seconds before polling"))),
    (status=400,body=ErrorBody),(status=401,body=ErrorBody),(status=404,body=ErrorBody),(status=409,body=ErrorBody),(status=429,body=ErrorBody),(status=503,body=ErrorBody)))]
pub(crate) async fn read(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, axum::extract::rejection::PathRejection>,
    query: Result<Query<DiffQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let actor = match auth::authenticate(&state, &headers).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = auth::verify_actor(&headers, &actor) {
        return error.into_response();
    }
    let (Ok(Path(proposal)), Ok(Query(query))) = (path, query) else {
        return invalid();
    };
    let (snapshot, offset, limit) = if let Some(snapshot) = query.snapshot {
        if snapshot.is_nil()
            || query.request_id.is_some()
            || query.head.is_some()
            || query.target.is_some()
        {
            return invalid();
        }
        let offset = query.offset.unwrap_or(0);
        let limit = query.limit.unwrap_or(50);
        if offset > 40000 || !(1..=100).contains(&limit) {
            return invalid();
        }
        (snapshot, offset, limit)
    } else {
        let Some(request) = query.request_id.filter(|id| !id.is_nil()) else {
            return invalid();
        };
        if query.offset.is_some() || query.limit.is_some() {
            return invalid();
        }
        let parse = |text: Option<String>| text.map(CommitSha::parse).transpose();
        let (Ok(head), Ok(target)) = (parse(query.head), parse(query.target)) else {
            return invalid();
        };
        let key = IdempotencyKey::parse(request.to_string()).expect("UUID is a valid request key");
        let ticket = match state
            .comparison_jobs
            .reserve(actor.user_id, proposal, &key, Selection { head, target })
            .await
        {
            Ok(ticket) => ticket,
            Err(error) => return failure(error),
        };
        (ticket.snapshot_id, 0, 50)
    };
    let mut status = match state
        .comparison_jobs
        .status(actor.user_id, proposal, snapshot)
        .await
    {
        Ok(status) => status,
        Err(error) => return failure(error),
    };
    let page = if status.state == "READY" {
        let page = match state
            .comparison_jobs
            .page(actor.user_id, proposal, snapshot, offset, limit)
            .await
        {
            Ok(page) => page,
            Err(error) => return failure(error),
        };
        // Pages are immutable; recheck current access/context after this await.
        status = match state
            .comparison_jobs
            .status(actor.user_id, proposal, snapshot)
            .await
        {
            Ok(status) => status,
            Err(error) => return failure(error),
        };
        if status.summary.as_ref().is_none_or(|summary| {
            summary.content_id != page.content_id || summary.changes_total != page.total
        }) || page.snapshot_id != snapshot
        {
            return failure(ComparisonStoreError::InvalidSnapshot);
        }
        Some(DiffFiles {
            snapshot_id: page.snapshot_id,
            content_id: page.content_id,
            offset: page.offset,
            total: page.total,
            next_offset: page.next_offset,
            files: page.files,
        })
    } else {
        None
    };
    let pending = status.state == "PENDING";
    let context_changed = status.summary.as_ref().is_some_and(|s| {
        s.proposal_version != status.current_version
            || s.recorded_head_sha != status.current_head_sha
            || s.proposal_state != status.current_state
    });
    let historical_head = status
        .summary
        .as_ref()
        .map(|s| s.head_sha != status.current_head_sha);
    let response = DiffResponse {
        proposal_id: proposal,
        snapshot_id: snapshot,
        job_id: status.ticket.job_id,
        state: status.state,
        job_state: status.job_state,
        phase: status.phase,
        attempts: status.attempts,
        error_code: status.error_code,
        completed_at: status.completed_at,
        current_head_sha: status.current_head_sha,
        current_proposal_version: status.current_version,
        current_proposal_state: status.current_state,
        context_changed,
        historical_head,
        summary: status.summary,
        page,
    };
    let mut response = (
        if pending {
            StatusCode::ACCEPTED
        } else {
            StatusCode::OK
        },
        Json(response),
    )
        .into_response();
    set_location(&mut response, proposal, snapshot);
    if pending {
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("2"));
    }
    response
}
fn progress_path(proposal: Uuid, snapshot: Uuid) -> String {
    format!("/api/v1/proposals/{proposal}/diff?snapshot={snapshot}")
}
fn set_location(response: &mut Response, proposal: Uuid, snapshot: Uuid) {
    if let Ok(location) = HeaderValue::from_str(&progress_path(proposal, snapshot)) {
        response.headers_mut().insert(header::LOCATION, location);
    }
}
fn invalid() -> Response {
    proposals::problem(
        StatusCode::BAD_REQUEST,
        "INVALID_COMPARISON_QUERY",
        "פרטי ההשוואה אינם תקינים. יש לבחור בקשה חדשה או גרסה שמורה אחת.",
        false,
        None,
    )
}
pub(crate) fn failure(error: ComparisonStoreError) -> Response {
    let (status, code, message, retryable) = match error {
        ComparisonStoreError::NotFound
        | ComparisonStoreError::Read(ComparisonReadError::NotFound) => (
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            "ההשוואה המבוקשת לא נמצאה.",
            false,
        ),
        ComparisonStoreError::Read(ComparisonReadError::ContextChanged) => (
            StatusCode::CONFLICT,
            "COMPARISON_CONTEXT_CHANGED",
            "ההצעה או חיבור המאגר השתנו. יש לבדוק את הגרסה לפני בקשה חדשה.",
            false,
        ),
        ComparisonStoreError::Read(ComparisonReadError::ProposalNotReady) => (
            StatusCode::CONFLICT,
            "PROPOSAL_NOT_READY",
            "ענף ההצעה טרם מוכן להשוואה.",
            false,
        ),
        ComparisonStoreError::IdempotencyMismatch => (
            StatusCode::CONFLICT,
            "IDEMPOTENCY_MISMATCH",
            "מזהה הבקשה כבר שימש לבחירת גרסאות אחרת.",
            false,
        ),
        ComparisonStoreError::QuotaExceeded => (
            StatusCode::TOO_MANY_REQUESTS,
            "COMPARISON_LIMIT",
            "יש בקשות השוואה ממתינות או שהגעת למגבלת הבקשות. אפשר לבדוק בקשה קיימת.",
            true,
        ),
        ComparisonStoreError::InvalidQuery => return invalid(),
        ComparisonStoreError::NotReady => (
            StatusCode::CONFLICT,
            "COMPARISON_NOT_READY",
            "ההשוואה עדיין אינה מוכנה לקריאה.",
            true,
        ),
        ComparisonStoreError::InvalidSnapshot => (
            StatusCode::SERVICE_UNAVAILABLE,
            "COMPARISON_EVIDENCE_INVALID",
            "לא ניתן לאמת את נתוני ההשוואה. יש להכין השוואה חדשה.",
            false,
        ),
        ComparisonStoreError::Read(
            ComparisonReadError::InvalidConfiguration
            | ComparisonReadError::Source(
                BookError::RepositoryNotConfigured | BookError::InvalidRepositoryConfiguration,
            ),
        ) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "COMPARISON_SETUP_REQUIRED",
            "חיבור המאגר אינו מוכן להשוואה. יש לבדוק את ההגדרות.",
            false,
        ),
        _ => (
            StatusCode::SERVICE_UNAVAILABLE,
            "COMPARISON_UNAVAILABLE",
            "לא ניתן לבדוק כרגע את ההשוואה. אפשר לבדוק שוב את אותה בקשה.",
            true,
        ),
    };
    let mut response = proposals::problem(status, code, message, retryable, None);
    if status == StatusCode::TOO_MANY_REQUESTS {
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("60"));
    }
    response
}
pub(crate) async fn job_status(
    state: &AppState,
    headers: &HeaderMap,
    actor: &SessionIdentity,
    job: Uuid,
) -> Option<Response> {
    let row:Result<Option<(Uuid,Uuid)>,_>=sqlx::query_as("SELECT s.proposal_id,s.id FROM comparison_snapshots s JOIN jobs j ON j.id=s.job_id AND j.organization_id=s.organization_id AND j.kind='COMPARE_PROPOSAL' WHERE j.id=$1 AND s.actor_id=$2")
        .bind(job).bind(actor.user_id).fetch_optional(&state.database).await;
    let (proposal, snapshot) = match row {
        Ok(Some(row)) => row,
        Ok(None) => return None,
        Err(_) => return Some(failure(ComparisonStoreError::Unavailable)),
    };
    if let Err(error) = auth::verify_actor(headers, actor) {
        return Some(error.into_response());
    }
    let status: ComparisonStatus = match state
        .comparison_jobs
        .status(actor.user_id, proposal, snapshot)
        .await
    {
        Ok(status) => status,
        Err(error) => return Some(failure(error)),
    };
    Some(
        Json(JobStatus {
            job_id: job,
            kind: "COMPARE_PROPOSAL".into(),
            state: status.job_state,
            attempts: status.attempts,
            proposal_id: Some(proposal),
            phase: status.phase,
            error_code: status.error_code,
            creation: None,
            save: None,
            save_failure: None,
            comparison: Some(ComparisonJobResult {
                snapshot_id: snapshot,
                content_id: status.summary.map(|s| s.content_id),
                progress_path: progress_path(proposal, snapshot),
            }),
        })
        .into_response(),
    )
}
