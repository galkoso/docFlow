use crate::{ApiError, AppState, ErrorBody, auth, specification};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Serialize;
use specspace_application::{
    book::BookError,
    proposal::{CreateProposal, IdempotencyKey, ProposalError, ValidatedProposal},
};
use specspace_domain::BranchName;
use specspace_git_adapter::{CommitSha, GitError};
use specspace_infrastructure::proposals::{CreationTicket, PgProposals};
use uuid::Uuid;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/proposals",
            post(create).layer(DefaultBodyLimit::max(128 * 1024)),
        )
        .route("/api/v1/jobs/{job}", get(job_status))
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct CreationAccepted {
    pub proposal_id: Uuid,
    pub number: i64,
    pub operation_id: Uuid,
    pub job_id: Uuid,
}
fn accepted(ticket: CreationTicket) -> Response {
    let location = format!("/api/v1/jobs/{}", ticket.job_id);
    let mut response = (
        StatusCode::ACCEPTED,
        Json(CreationAccepted {
            proposal_id: ticket.proposal_id,
            number: ticket.number,
            operation_id: ticket.operation_id,
            job_id: ticket.job_id,
        }),
    )
        .into_response();
    if let Ok(location) = HeaderValue::from_str(&location) {
        response.headers_mut().insert(header::LOCATION, location);
    }
    response
}

#[utoipa::path(post, path = "/api/v1/proposals", security(("sessionCookie" = [])), request_body = CreateProposal,
    params(("Origin" = String, Header), ("X-XSRF-TOKEN" = String, Header), ("Idempotency-Key" = String, Header)),
    responses((status = 202, body = CreationAccepted, description = "Reserved proposal and provisioning job; Git creation is not yet complete", headers(("Location" = String, description = "Authorized job status URL"))),
        (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody), (status = 413, body = ErrorBody), (status = 422, body = ErrorBody), (status = 502, body = ErrorBody), (status = 503, body = ErrorBody)))]
pub(crate) async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Result<Json<CreateProposal>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let actor = match auth::authenticate(&state, &headers).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = auth::verify_write(&state, &headers, &actor) {
        return error.into_response();
    }
    let request = match payload {
        Ok(Json(request)) => request,
        Err(error) => {
            return problem(
                if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
                    StatusCode::PAYLOAD_TOO_LARGE
                } else {
                    StatusCode::BAD_REQUEST
                },
                "INVALID_CREATE_REQUEST",
                "פרטי ההצעה אינם תקינים. יש לבדוק את השדות ולנסות שוב.",
                false,
                None,
            );
        }
    };
    let mut keys = headers.get_all("Idempotency-Key").iter();
    let key = match (keys.next().and_then(|key| key.to_str().ok()), keys.next()) {
        (Some(key), None) => match IdempotencyKey::parse(key) {
            Ok(key) => key,
            Err(error) => return failure(error),
        },
        _ => {
            return problem(
                StatusCode::BAD_REQUEST,
                "IDEMPOTENCY_KEY_REQUIRED",
                "נדרש מזהה פעולה תקין ליצירת ההצעה.",
                false,
                None,
            );
        }
    };
    let intent = match ValidatedProposal::validate(request, actor.user_id) {
        Ok(intent) => intent,
        Err(error) => return failure(error),
    };
    let store = PgProposals::new(state.database.clone());
    match store.existing(actor.user_id, &intent, &key).await {
        Ok(Some(ticket)) => return accepted(ticket),
        Ok(None) => {}
        Err(error) => return failure(error),
    }
    let provider = match state
        .book_source
        .provider(intent.request().repository_id)
        .await
    {
        Ok(provider) => provider,
        Err(error) => return specification::failure(error),
    };
    let current = match provider.main_head().await {
        Ok(current) => current,
        Err(error) => return specification::failure(BookError::Provider(error)),
    };
    if let Err(error) = store
        .authorize_create(actor.user_id, intent.request().repository_id)
        .await
    {
        return failure(error);
    }
    if current != *intent.source() {
        return problem(
            StatusCode::CONFLICT,
            "SOURCE_MOVED",
            "האפיון המאושר השתנה מאז פתיחת הטופס. יש לבדוק את הגרסה החדשה לפני יצירה.",
            false,
            Some(
                serde_json::json!({"expected_source_sha":intent.source().as_str(),"actual_source_sha":current.as_str()}),
            ),
        );
    }
    match provider.branch_head(intent.branch()).await {
        Ok(_) => return failure(ProposalError::BranchTaken),
        Err(GitError::BranchMissing) => {}
        Err(error) => return specification::failure(BookError::Provider(error)),
    }
    match store.reserve(actor.user_id, &intent, &key).await {
        Ok(ticket) => accepted(ticket),
        Err(error) => failure(error),
    }
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct CreationResult {
    pub proposal_id: Uuid,
    pub branch: String,
    pub head_sha: String,
}
#[derive(Serialize, utoipa::ToSchema)]
pub struct JobStatus {
    pub job_id: Uuid,
    pub kind: String,
    pub state: String,
    pub attempts: i32,
    pub proposal_id: Option<Uuid>,
    pub phase: String,
    pub error_code: Option<String>,
    pub creation: Option<CreationResult>,
}
#[derive(sqlx::FromRow)]
struct JobRow {
    job_id: Uuid,
    kind: String,
    state: String,
    attempts: i32,
    proposal_id: Option<Uuid>,
    branch: Option<String>,
    result_sha: Option<String>,
    operation_state: Option<String>,
    error_code: Option<String>,
    result: Option<serde_json::Value>,
    index_state: Option<String>,
    index_error: Option<String>,
}
#[utoipa::path(get, path = "/api/v1/jobs/{job}", security(("sessionCookie" = [])), params(("job" = Uuid, Path)),
    responses((status = 200, body = JobStatus), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 404, body = ErrorBody), (status = 503, body = ErrorBody)))]
pub(crate) async fn job_status(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, axum::extract::rejection::PathRejection>,
) -> Response {
    let actor = match auth::authenticate(&state, &headers).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    let Ok(Path(job)) = path else {
        return invalid_query();
    };
    // Resolve scope through linked durable records, never client-controlled JSON
    // repository IDs. Do not expose raw job payloads or provider configuration.
    let row = sqlx::query_as::<_, JobRow>("SELECT j.id AS job_id,j.kind,j.state,j.attempts,p.id AS proposal_id,p.branch,o.result_sha,o.state AS operation_state,o.error_code,j.result,s.state AS index_state,s.error_code AS index_error FROM jobs j LEFT JOIN git_operations o ON o.id=j.operation_id AND o.organization_id=j.organization_id AND o.kind='CREATE' LEFT JOIN proposals p ON p.id=o.proposal_id AND p.organization_id=j.organization_id LEFT JOIN search_index_snapshots s ON s.job_id=j.id AND s.organization_id=j.organization_id JOIN repositories r ON r.organization_id=j.organization_id AND r.id=CASE WHEN j.kind='CREATE_PROPOSAL' THEN p.repository_id WHEN j.kind='INDEX_SNAPSHOT' THEN s.repository_id END JOIN memberships m ON m.organization_id=r.organization_id JOIN users u ON u.id=m.user_id WHERE j.id=$1 AND m.user_id=$2 AND u.status='ACTIVE' AND j.kind IN ('CREATE_PROPOSAL','INDEX_SNAPSHOT')")
        .bind(job).bind(actor.user_id).fetch_optional(&state.database).await;
    let row = match row {
        Ok(Some(row)) => row,
        Ok(None) => return failure(ProposalError::NotFound),
        Err(_) => return failure(ProposalError::Unavailable),
    };
    if row.kind == "INDEX_SNAPSHOT"
        && row.state == "SUCCEEDED"
        && row.index_state.as_deref() != Some("READY")
    {
        return failure(ProposalError::Unavailable);
    }
    let creation = if row.kind == "CREATE_PROPOSAL" && row.state == "SUCCEEDED" {
        match (
            row.proposal_id,
            row.branch
                .as_deref()
                .and_then(|branch| BranchName::parse(branch).ok()),
            row.result_sha
                .as_deref()
                .and_then(|sha| CommitSha::parse(sha).ok()),
        ) {
            (Some(proposal_id), Some(branch), Some(sha))
                if row.operation_state.as_deref() == Some("SUCCEEDED") =>
            {
                Some(CreationResult {
                    proposal_id,
                    branch: branch.as_str().into(),
                    head_sha: sha.as_str().into(),
                })
            }
            _ => return failure(ProposalError::Unavailable),
        }
    } else {
        None
    };
    let phase = if row.operation_state.as_deref() == Some("UNKNOWN") {
        "RECONCILIATION_REQUIRED"
    } else if row.state == "SUCCEEDED" {
        "READY"
    } else if row.state == "FAILED" || row.state == "CANCELLED" {
        "FAILED"
    } else if row.kind == "INDEX_SNAPSHOT"
        && row.index_state.as_deref() == Some("PENDING")
        && row.state == "RUNNING"
    {
        "INDEXING"
    } else if row.operation_state.as_deref() == Some("RUNNING") {
        "CREATING_BRANCH"
    } else if row.state == "RUNNING" {
        "PREFLIGHT"
    } else {
        "QUEUED"
    };
    let error_code = row
        .error_code
        .or(row.index_error)
        .or_else(|| {
            row.result
                .as_ref()?
                .get("error_code")?
                .as_str()
                .map(str::to_owned)
        })
        .filter(|code| {
            !code.is_empty()
                && code.len() <= 100
                && code
                    .bytes()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_')
        });
    Json(JobStatus {
        job_id: row.job_id,
        kind: row.kind,
        state: row.state,
        attempts: row.attempts,
        proposal_id: row.proposal_id,
        phase: phase.into(),
        error_code,
        creation,
    })
    .into_response()
}

pub(crate) fn invalid_query() -> Response {
    problem(
        StatusCode::BAD_REQUEST,
        "INVALID_QUERY",
        "פרטי הבקשה אינם תקינים.",
        false,
        None,
    )
}
pub(crate) fn failure(error: ProposalError) -> Response {
    if let ProposalError::Validation(errors) = error {
        let mut fields = serde_json::Map::new();
        for error in errors {
            fields.insert(error.field.into(), serde_json::json!({"code":error.code}));
        }
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(ErrorBody {
                error: ApiError {
                    code: "VALIDATION_FAILED",
                    message_he: "יש לתקן את שדות ההצעה המסומנים.",
                    trace_id: Uuid::new_v4(),
                    retryable: false,
                    field_errors: fields.into(),
                    details: None,
                },
            }),
        )
            .into_response();
    }
    let (status, code, message, retryable) = match error {
        ProposalError::NotFound => (
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            "המשאב המבוקש לא נמצא.",
            false,
        ),
        ProposalError::Forbidden => (
            StatusCode::FORBIDDEN,
            "FORBIDDEN",
            "אין הרשאה ליצור הצעה במאגר זה.",
            false,
        ),
        ProposalError::BranchTaken => (
            StatusCode::CONFLICT,
            "BRANCH_EXISTS",
            "שם הענף כבר קיים או שמור להצעה קודמת. יש לבחור שם אחר.",
            false,
        ),
        ProposalError::IdempotencyMismatch => (
            StatusCode::CONFLICT,
            "IDEMPOTENCY_MISMATCH",
            "מזהה הפעולה כבר שימש לפרטים אחרים. יש לחזור להצעה הקיימת או להתחיל פעולה חדשה.",
            false,
        ),
        _ => (
            StatusCode::SERVICE_UNAVAILABLE,
            "TEMPORARILY_UNAVAILABLE",
            "הפעולה אינה זמינה כרגע. אפשר לנסות שוב עם אותו מזהה פעולה.",
            true,
        ),
    };
    problem(status, code, message, retryable, None)
}
pub(crate) fn problem(
    status: StatusCode,
    code: &'static str,
    message_he: &'static str,
    retryable: bool,
    details: Option<serde_json::Value>,
) -> Response {
    (
        status,
        Json(ErrorBody {
            error: ApiError {
                code,
                message_he,
                trace_id: Uuid::new_v4(),
                retryable,
                field_errors: serde_json::json!({}),
                details,
            },
        }),
    )
        .into_response()
}
