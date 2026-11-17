use crate::{
    AppState, ErrorBody, auth,
    proposals::{self, JobStatus},
};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use serde::{Deserialize, Serialize};
use specspace_application::save::{MAX_SAVE_BYTES, SaveError, SaveRequest, ValidatedSave};
use specspace_git_adapter::CommitSha;
use specspace_infrastructure::saves::SaveJournalError;
use uuid::Uuid;

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/api/v1/proposals/{proposal}/commits",
        post(submit).layer(DefaultBodyLimit::max(6 * MAX_SAVE_BYTES + 65536)),
    )
}
#[derive(Serialize, utoipa::ToSchema)]
pub struct SaveAccepted {
    pub proposal_id: Uuid,
    pub operation_id: Uuid,
    pub job_id: Uuid,
}
#[utoipa::path(post,path="/api/v1/proposals/{proposal}/commits",operation_id="saveProposalCommit",security(("sessionCookie"=[])),
    params(("proposal"=Uuid,Path),("X-Expected-Actor"=Uuid,Header,description="Actor captured with this immutable intent"),("Origin"=String,Header),("X-XSRF-TOKEN"=String,Header),("Idempotency-Key"=String,Header)),request_body=SaveRequest,
    responses((status=202,body=SaveAccepted,description="Queued only; no Git success is claimed",headers(("Location"=String,description="Actor-bound job status URL"))),(status=400,body=ErrorBody),(status=401,body=ErrorBody),(status=403,body=ErrorBody),(status=404,body=ErrorBody),(status=409,body=ErrorBody),(status=413,body=ErrorBody),(status=422,body=ErrorBody),(status=503,body=ErrorBody)))]
pub(crate) async fn submit(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, axum::extract::rejection::PathRejection>,
    body: Result<Json<SaveRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let actor = match auth::authenticate(&state, &headers).await {
        Ok(actor) => actor,
        Err(e) => return e.into_response(),
    };
    if let Err(e) = auth::verify_actor(&headers, &actor) {
        return e.into_response();
    }
    if let Err(e) = auth::verify_write(&state, &headers, &actor) {
        return e.into_response();
    }
    let Ok(Path(proposal)) = path else {
        return proposals::invalid_query();
    };
    let request = match body {
        Ok(Json(body)) => body,
        Err(e) => {
            return problem(
                if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
                    StatusCode::PAYLOAD_TOO_LARGE
                } else {
                    StatusCode::BAD_REQUEST
                },
                "INVALID_SAVE_REQUEST",
                false,
            );
        }
    };
    let key = match proposals::operation_key(&headers) {
        Ok(key) => key,
        Err(e) => return e.into_response(),
    };
    let repository = match state
        .saves
        .submission_repository(actor.user_id, proposal)
        .await
    {
        Ok(id) => id,
        Err(e) => return failure(e),
    };
    let permit = match state.render_slots.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => return failure(SaveJournalError::Unavailable),
    };
    let planned = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        ValidatedSave::new(actor.user_id, proposal, repository, request)
    })
    .await;
    let intent = match planned {
        Ok(Ok(intent)) => intent,
        Ok(Err(SaveError::Invalid(code))) => {
            return problem(
                if matches!(code, "FILE_TOO_LARGE" | "SAVE_BYTE_LIMIT") {
                    StatusCode::PAYLOAD_TOO_LARGE
                } else {
                    StatusCode::UNPROCESSABLE_ENTITY
                },
                code,
                false,
            );
        }
        Ok(Err(_)) => {
            return problem(
                StatusCode::UNPROCESSABLE_ENTITY,
                "INVALID_SAVE_REQUEST",
                false,
            );
        }
        Err(_) => return failure(SaveJournalError::Unavailable),
    };
    match state.saves.reserve(&intent, &key).await {
        Ok(ticket) => {
            let location = format!("/api/v1/jobs/{}", ticket.job_id);
            let mut response = (
                StatusCode::ACCEPTED,
                Json(SaveAccepted {
                    proposal_id: ticket.proposal_id,
                    operation_id: ticket.operation_id,
                    job_id: ticket.job_id,
                }),
            )
                .into_response();
            if let Ok(value) = HeaderValue::from_str(&location) {
                response.headers_mut().insert(header::LOCATION, value);
            }
            response
        }
        Err(e) => failure(e),
    }
}
fn problem(status: StatusCode, code: &'static str, retryable: bool) -> Response {
    proposals::problem(
        status,
        code,
        "לא ניתן להשלים את השמירה. יש לבדוק את המצב לפני ניסיון נוסף.",
        retryable,
        None,
    )
}
fn failure(error: SaveJournalError) -> Response {
    let (status, code, retryable) = match error {
        SaveJournalError::NotFound => (StatusCode::NOT_FOUND, "NOT_FOUND", false),
        SaveJournalError::Forbidden => (StatusCode::FORBIDDEN, "SAVE_FORBIDDEN", false),
        SaveJournalError::StateChanged => (StatusCode::CONFLICT, "SAVE_CONTEXT_CHANGED", false),
        SaveJournalError::Busy => (StatusCode::CONFLICT, "GIT_OPERATION_PENDING", false),
        SaveJournalError::IdempotencyMismatch => {
            (StatusCode::CONFLICT, "IDEMPOTENCY_MISMATCH", false)
        }
        SaveJournalError::InvalidPlan => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "INVALID_SAVE_REQUEST",
            false,
        ),
        _ => (StatusCode::SERVICE_UNAVAILABLE, "SAVE_UNAVAILABLE", true),
    };
    problem(status, code, retryable)
}
#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct SavedDocument {
    pub document_id: String,
    pub path: String,
    pub sha256: String,
    pub bytes: usize,
    pub normalization_changed: bool,
    pub normalization_only: bool,
}
#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct SaveResult {
    pub proposal_id: Uuid,
    pub head_sha: String,
    pub proposal_version: i64,
    pub committed_at: Option<String>,
    pub no_op: bool,
    pub reconciled: bool,
    pub documents: Vec<SavedDocument>,
}
#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct SaveFinding {
    pub code: String,
    pub path: String,
    pub path_truncated: bool,
    pub severity: String,
    pub source_start_byte: Option<usize>,
    pub source_end_byte: Option<usize>,
}
#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct SaveValidationReport {
    pub findings: Vec<SaveFinding>,
    pub findings_total: usize,
    pub findings_truncated: bool,
    pub blockers_total: usize,
    pub complete: bool,
}
#[derive(Serialize, utoipa::ToSchema)]
pub struct SaveFailure {
    pub document_id: Option<String>,
    pub validation_code: Option<String>,
    pub validation: Option<SaveValidationReport>,
}
#[derive(sqlx::FromRow)]
struct Row {
    job_id: Uuid,
    proposal_id: Uuid,
    state: String,
    attempts: i32,
    operation_state: String,
    error_code: Option<String>,
    result_sha: Option<String>,
    result: Option<serde_json::Value>,
}
/// The generic jobs endpoint delegates only actor-owned, currently authorized
/// save jobs here. No request source or installation settings enter this query.
pub(crate) async fn job_status(
    state: &AppState,
    headers: &HeaderMap,
    actor: &specspace_infrastructure::sessions::SessionIdentity,
    job: Uuid,
) -> Option<Response> {
    let row=sqlx::query_as::<_,Row>("SELECT j.id AS job_id,p.id AS proposal_id,j.state,j.attempts,o.state AS operation_state,o.error_code,o.result_sha,j.result FROM jobs j JOIN git_operations o ON o.id=j.operation_id AND o.organization_id=j.organization_id AND o.kind='COMMIT' JOIN proposal_saves s ON s.operation_id=o.id AND s.organization_id=o.organization_id AND s.actor_id=o.actor_id AND s.proposal_id=o.proposal_id JOIN proposals p ON p.id=s.proposal_id AND p.organization_id=s.organization_id AND p.repository_id=s.repository_id JOIN memberships m ON m.organization_id=p.organization_id JOIN users u ON u.id=m.user_id WHERE j.id=$1 AND j.kind='SAVE_PROPOSAL' AND o.actor_id=$2 AND m.user_id=$2 AND u.status='ACTIVE' AND m.role<>'READER' AND (p.owner_id=$2 OR m.role='ADMIN' OR EXISTS(SELECT 1 FROM proposal_collaborators c WHERE c.proposal_id=p.id AND c.organization_id=p.organization_id AND c.user_id=$2 AND c.access='EDIT'))")
        .bind(job).bind(actor.user_id).fetch_optional(&state.database).await;
    let row = match row {
        Ok(Some(row)) => row,
        Ok(None) => return None,
        Err(_) => return Some(failure(SaveJournalError::Unavailable)),
    };
    if let Err(error) = auth::verify_actor(headers, actor) {
        return Some(error.into_response());
    }
    let save = if row.state == "SUCCEEDED" {
        let value = row
            .result
            .clone()
            .and_then(|v| serde_json::from_value::<SaveResult>(v).ok());
        match value {
            Some(value)
                if row.operation_state == "SUCCEEDED"
                    && value.proposal_id == row.proposal_id
                    && row.result_sha.as_deref() == Some(value.head_sha.as_str())
                    && CommitSha::parse(&value.head_sha).is_ok()
                    && value.proposal_version > 0
                    && value.documents.len() <= 50
                    && (if value.no_op {
                        value.committed_at.is_none() && value.documents.is_empty()
                    } else {
                        value
                            .committed_at
                            .as_ref()
                            .is_some_and(|t| !t.is_empty() && t.len() <= 64)
                            && !value.documents.is_empty()
                    }) =>
            {
                Some(value)
            }
            _ => return Some(failure(SaveJournalError::Unavailable)),
        }
    } else {
        None
    };
    let phase = if row.operation_state == "UNKNOWN" {
        "RECONCILIATION_REQUIRED"
    } else if row.state == "SUCCEEDED" {
        "READY"
    } else if row.state == "FAILED" || row.state == "CANCELLED" {
        "FAILED"
    } else if row.operation_state == "RUNNING" {
        "SAVING"
    } else if row.state == "RUNNING" {
        "PREFLIGHT"
    } else {
        "QUEUED"
    };
    let details = row.result.as_ref().and_then(|v| v.get("details"));
    let save_failure = details.filter(|v| v.is_object()).map(|v| SaveFailure {
        document_id: v
            .get("document_id")
            .and_then(|v| v.as_str())
            .filter(|v| specspace_application::draft::valid_document_id(v))
            .map(str::to_owned),
        validation_code: v
            .get("code")
            .and_then(|v| v.as_str())
            .filter(|v| valid_code(v))
            .map(str::to_owned),
        validation: serde_json::from_value::<SaveValidationReport>(v.clone())
            .ok()
            .filter(|r| {
                r.findings.len() <= 1000
                    && r.findings.iter().all(|f| {
                        valid_code(&f.code)
                            && f.path.chars().count() <= 240
                            && matches!(f.severity.as_str(), "warning" | "blocker")
                    })
            }),
    });
    Some(
        Json(JobStatus {
            job_id: row.job_id,
            kind: "SAVE_PROPOSAL".into(),
            state: row.state,
            attempts: row.attempts,
            proposal_id: Some(row.proposal_id),
            phase: phase.into(),
            error_code: row.error_code.filter(|c| valid_code(c)),
            creation: None,
            save,
            save_failure,
            comparison: None,
            review: None,
        })
        .into_response(),
    )
}
fn valid_code(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= 100
        && code
            .bytes()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_')
}
