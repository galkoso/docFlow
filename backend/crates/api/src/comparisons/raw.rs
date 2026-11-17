use super::*;
use specspace_application::comparison_file::FileReadError;
use specspace_document_engine::diff::window::{
    MAX_WINDOW_BYTES, MIN_WINDOW_BYTES, RawCursor, RawWindow, WindowError,
};
use specspace_git_adapter::GitError;

pub(super) fn routes() -> Router<AppState> {
    Router::new().route("/api/v1/proposals/{proposal}/diff/files/{file}", get(read))
}
#[derive(Deserialize, utoipa::IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in=Query)]
pub struct RawQuery {
    pub snapshot: Uuid,
    pub chunk: Option<usize>,
    /// Byte offset within the selected alignment chunk, from the previous window.
    pub offset: Option<usize>,
    /// Source text budget across both sides, before JSON escaping (1024–65536).
    pub max_bytes: Option<usize>,
}
#[derive(Serialize, utoipa::ToSchema)]
pub struct RawFileResponse {
    pub proposal_id: Uuid,
    pub snapshot_id: Uuid,
    pub content_id: String,
    pub merge_base_sha: String,
    pub head_sha: String,
    pub file: Change,
    pub raw: RawWindow,
}
#[utoipa::path(get,path="/api/v1/proposals/{proposal}/diff/files/{file}",operation_id="proposalRawDiffFile",security(("sessionCookie"=[])),
    params(("proposal"=Uuid,Path),("file"=String,Path,description="Immutable file identity from the inventory"),("X-Expected-Actor"=Uuid,Header,description="Captured owner of the comparison"),RawQuery),
    responses((status=200,body=RawFileResponse,description="Verified bounded raw source window, with immutable snapshot and file identities"),
    (status=400,body=ErrorBody),(status=401,body=ErrorBody),(status=404,body=ErrorBody),(status=409,body=ErrorBody),
    (status=413,body=ErrorBody),(status=422,body=ErrorBody),(status=503,body=ErrorBody)))]
pub(crate) async fn read(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<(Uuid, String)>, axum::extract::rejection::PathRejection>,
    query: Result<Query<RawQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let actor = match auth::authenticate(&state, &headers).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = auth::verify_actor(&headers, &actor) {
        return error.into_response();
    }
    let (Ok(Path((proposal, file))), Ok(Query(query))) = (path, query) else {
        return invalid_window();
    };
    let cursor = RawCursor {
        chunk: query.chunk.unwrap_or(0),
        offset: query.offset.unwrap_or(0),
    };
    let limit = query.max_bytes.unwrap_or(32 * 1024);
    if query.snapshot.is_nil()
        || cursor.chunk > 1024
        || cursor.offset > 2 * 1024 * 1024
        || !(MIN_WINDOW_BYTES..=MAX_WINDOW_BYTES).contains(&limit)
    {
        return invalid_window();
    }
    let loaded = match state
        .comparison_files
        .load(actor.user_id, proposal, query.snapshot, &file)
        .await
    {
        Ok(loaded) => loaded,
        Err(error) => return file_failure(error),
    };
    let permit = match state.render_slots.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => return file_failure(FileReadError::Busy),
    };
    let rendered = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let window = loaded.diff().window(
            loaded.before().unwrap_or(""),
            loaded.after().unwrap_or(""),
            cursor,
            limit,
        );
        (loaded, window)
    })
    .await;
    let (loaded, window) = match rendered {
        Ok(result) => result,
        Err(_) => return file_failure(FileReadError::Unavailable),
    };
    let raw = match window {
        Ok(window) => window,
        Err(WindowError::InvalidCursor | WindowError::InvalidLimit) => return invalid_window(),
        Err(WindowError::InvalidEvidence) => return file_failure(FileReadError::InvalidEvidence),
    };
    if let Err(error) = state.comparison_files.recheck(actor.user_id, &loaded).await {
        return file_failure(error);
    }
    Json(RawFileResponse {
        proposal_id: proposal,
        snapshot_id: loaded.snapshot(),
        content_id: loaded.content_id().into(),
        merge_base_sha: loaded.merge_base().as_str().into(),
        head_sha: loaded.head().as_str().into(),
        file: loaded.change().clone(),
        raw,
    })
    .into_response()
}
fn invalid_window() -> Response {
    proposals::problem(
        StatusCode::BAD_REQUEST,
        "INVALID_COMPARISON_WINDOW",
        "פרטי מקטע ההשוואה אינם תקינים. יש להשתמש במיקום שהוחזר מההשוואה השמורה.",
        false,
        None,
    )
}
pub(super) fn file_failure(error: FileReadError) -> Response {
    let (status, code, message, retryable, delay) = match error {
        FileReadError::Access(error) => return failure(ComparisonStoreError::Read(error)),
        FileReadError::NotFound => return failure(ComparisonStoreError::NotFound),
        FileReadError::NotReady => return failure(ComparisonStoreError::NotReady),
        FileReadError::InvalidEvidence => return failure(ComparisonStoreError::InvalidSnapshot),
        FileReadError::UnsupportedKind => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "COMPARISON_FILE_NOT_TEXT",
            "קובץ זה אינו מסמך טקסט להשוואה גולמית.",
            false,
            None,
        ),
        FileReadError::TooLarge => (
            StatusCode::PAYLOAD_TOO_LARGE,
            "COMPARISON_FILE_TOO_LARGE",
            "הקובץ גדול מדי להשוואת טקסט.",
            false,
            None,
        ),
        FileReadError::Busy => (
            StatusCode::SERVICE_UNAVAILABLE,
            "COMPARISON_BUSY",
            "עיבוד ההשוואה עמוס כרגע. אפשר לנסות שוב את אותו מקטע.",
            true,
            Some(2),
        ),
        FileReadError::TimedOut => (
            StatusCode::SERVICE_UNAVAILABLE,
            "COMPARISON_TIMED_OUT",
            "קריאת קובץ ההשוואה ארכה זמן רב מדי. אפשר לנסות שוב את אותו מקטע.",
            true,
            Some(2),
        ),
        FileReadError::Provider(GitError::RateLimited {
            retry_after_seconds,
        }) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "COMPARISON_SOURCE_BACKOFF",
            "ספק המאגר ביקש להמתין לפני קריאת הקובץ.",
            true,
            retry_after_seconds,
        ),
        FileReadError::Provider(GitError::FileMissing) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "COMPARISON_SOURCE_MISSING",
            "לא ניתן לקרוא כרגע את הקובץ שבגרסה השמורה.",
            false,
            None,
        ),
        _ => (
            StatusCode::SERVICE_UNAVAILABLE,
            "COMPARISON_UNAVAILABLE",
            "לא ניתן לקרוא כרגע את קובץ ההשוואה. אפשר לנסות שוב את אותו מקטע.",
            true,
            None,
        ),
    };
    let mut response = proposals::problem(status, code, message, retryable, None);
    if let Some(delay) = delay.and_then(|delay| HeaderValue::from_str(&delay.to_string()).ok()) {
        response.headers_mut().insert(header::RETRY_AFTER, delay);
    }
    response
}
