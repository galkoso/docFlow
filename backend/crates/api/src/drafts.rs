use crate::{ApiError, AppState, ErrorBody, auth};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Deserialize;
use specspace_application::draft::{
    self, DraftError, DraftSaved, DraftSnapshot, SaveDraft, ValidatedDraft,
};
use uuid::Uuid;

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(crate) struct DraftQuery {
    /// Omit to list buffer metadata without contents. Supplying an ID retrieves
    /// only the current user's recovery content for that document.
    document_id: Option<String>,
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(crate) struct DiscardQuery {
    document_id: String,
    expected_draft_version: i64,
}

#[utoipa::path(delete,path="/api/v1/proposals/{proposal}/draft",operation_id="discardProposalDraft",security(("sessionCookie"=[])),params(("proposal"=Uuid,Path),DiscardQuery),
    responses((status=204),(status=400,body=ErrorBody),(status=401,body=ErrorBody),(status=403,body=ErrorBody),(status=404,body=ErrorBody),(status=409,body=ErrorBody),(status=503,body=ErrorBody)))]
pub(crate) async fn discard(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, axum::extract::rejection::PathRejection>,
    query: Result<Query<DiscardQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let actor = match auth::authenticate(&state, &headers).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = auth::verify_write(&state, &headers, &actor) {
        return error.into_response();
    }
    let (Ok(Path(proposal)), Ok(Query(query))) = (path, query) else {
        return failure(DraftError::Invalid("INVALID_QUERY"));
    };
    if !draft::valid_document_id(&query.document_id) || query.expected_draft_version <= 0 {
        return failure(DraftError::Invalid("INVALID_DISCARD_QUERY"));
    }
    match state
        .drafts
        .discard(
            actor.user_id,
            proposal,
            &query.document_id,
            query.expected_draft_version,
        )
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => failure(error),
    }
}

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/api/v1/proposals/{proposal}/draft",
        get(read)
            .put(save)
            .delete(discard)
            .layer(DefaultBodyLimit::max(6 * draft::MAX_DRAFT_BYTES + 4096)),
    )
}

fn failure(error: DraftError) -> Response {
    let details = match &error {
        DraftError::VersionConflict { current_version } => {
            Some(serde_json::json!({"current_draft_version": current_version}))
        }
        DraftError::QuotaExceeded => Some(
            serde_json::json!({"max_recovery_bytes": draft::MAX_RECOVERY_BYTES, "max_documents": draft::MAX_RECOVERY_DOCUMENTS}),
        ),
        DraftError::Invalid("FILE_TOO_LARGE") => {
            Some(serde_json::json!({"max_document_bytes": draft::MAX_DRAFT_BYTES}))
        }
        _ => None,
    };
    let (status, code, message, retryable) = match error {
        DraftError::NotFound => (
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            "המשאב המבוקש לא נמצא.",
            false,
        ),
        DraftError::Forbidden => (
            StatusCode::FORBIDDEN,
            "DRAFT_FORBIDDEN",
            "אין הרשאה לשמור טיוטת התאוששות להצעה זו.",
            false,
        ),
        DraftError::VersionConflict { .. } => (
            StatusCode::CONFLICT,
            "DRAFT_VERSION_CONFLICT",
            "טיוטת ההתאוששות השתנתה. יש להשוות לגרסה השמורה לפני החלפה.",
            false,
        ),
        DraftError::SaveIdentityMismatch => (
            StatusCode::CONFLICT,
            "DRAFT_SAVE_ID_MISMATCH",
            "מזהה השמירה כבר שימש לתוכן אחר. יש לבדוק את הטיוטה לפני ניסיון חדש.",
            false,
        ),
        DraftError::QuotaExceeded => (
            StatusCode::PAYLOAD_TOO_LARGE,
            "DRAFT_QUOTA_EXCEEDED",
            "טיוטות ההתאוששות להצעה חורגות ממגבלת האחסון למשתמש.",
            false,
        ),
        DraftError::PolicyNotConfigured => (
            StatusCode::SERVICE_UNAVAILABLE,
            "RECOVERY_POLICY_NOT_CONFIGURED",
            "מדיניות שמירת טיוטות התאוששות טרם הוגדרה. השינוי עדיין לא נשמר בשרת.",
            false,
        ),
        DraftError::Invalid(code) => (
            if code == "FILE_TOO_LARGE" {
                StatusCode::PAYLOAD_TOO_LARGE
            } else {
                StatusCode::BAD_REQUEST
            },
            code,
            "פרטי טיוטת ההתאוששות אינם תקינים.",
            false,
        ),
        DraftError::Unavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            "SERVICE_UNAVAILABLE",
            "שמירת ההתאוששות אינה זמינה כרגע. אין אישור שהשינוי נשמר בשרת.",
            true,
        ),
    };
    (
        status,
        Json(ErrorBody {
            error: ApiError {
                code,
                message_he: message,
                trace_id: Uuid::new_v4(),
                retryable,
                field_errors: serde_json::json!({}),
                details,
            },
        }),
    )
        .into_response()
}

#[utoipa::path(get,path="/api/v1/proposals/{proposal}/draft",operation_id="readProposalDraft",security(("sessionCookie"=[])),params(("proposal"=Uuid,Path),DraftQuery),
    responses((status=200,body=DraftSnapshot),(status=400,body=ErrorBody),(status=401,body=ErrorBody),(status=404,body=ErrorBody),(status=413,body=ErrorBody),(status=503,body=ErrorBody)))]
pub(crate) async fn read(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, axum::extract::rejection::PathRejection>,
    query: Result<Query<DraftQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let actor = match auth::authenticate(&state, &headers).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    let (Ok(Path(proposal)), Ok(Query(query))) = (path, query) else {
        return failure(DraftError::Invalid("INVALID_QUERY"));
    };
    if query
        .document_id
        .as_deref()
        .is_some_and(|id| !draft::valid_document_id(id))
    {
        return failure(DraftError::Invalid("INVALID_DOCUMENT_ID"));
    }
    match state
        .drafts
        .read(actor.user_id, proposal, query.document_id.as_deref())
        .await
    {
        Ok(snapshot) => Json(snapshot).into_response(),
        Err(error) => failure(error),
    }
}

#[utoipa::path(put,path="/api/v1/proposals/{proposal}/draft",operation_id="saveProposalDraft",security(("sessionCookie"=[])),params(("proposal"=Uuid,Path)),request_body=SaveDraft,
    responses((status=200,body=DraftSaved),(status=400,body=ErrorBody),(status=401,body=ErrorBody),(status=403,body=ErrorBody),(status=404,body=ErrorBody),(status=409,body=ErrorBody),(status=413,body=ErrorBody),(status=503,body=ErrorBody)))]
pub(crate) async fn save(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, axum::extract::rejection::PathRejection>,
    body: Result<Json<SaveDraft>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let actor = match auth::authenticate(&state, &headers).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = auth::verify_write(&state, &headers, &actor) {
        return error.into_response();
    }
    let Ok(Path(proposal)) = path else {
        return failure(DraftError::Invalid("INVALID_QUERY"));
    };
    let request = match body {
        Ok(Json(request)) => request,
        Err(error) => {
            return failure(DraftError::Invalid(
                if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
                    "FILE_TOO_LARGE"
                } else {
                    "INVALID_JSON"
                },
            ));
        }
    };
    let draft = match ValidatedDraft::new(request) {
        Ok(draft) => draft,
        Err(error) => return failure(error),
    };
    match state.drafts.save(actor.user_id, proposal, &draft).await {
        Ok(saved) => Json(saved).into_response(),
        Err(error) => failure(error),
    }
}
