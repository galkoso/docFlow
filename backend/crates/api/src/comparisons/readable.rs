use super::*;
use specspace_application::comparison_file::{FileReadError, LoadedFile};
use specspace_document_engine::diff::readable::{
    pair::{ReadablePair, ReadableSide},
    window::{EventCursor, EventWindow, ReadablePage, ReadableWindowError, UnitSummary},
};
use std::time::Duration;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/proposals/{proposal}/diff/files/{file}/readable",
            get(page),
        )
        .route(
            "/api/v1/proposals/{proposal}/diff/files/{file}/readable/units/{unit}",
            get(unit),
        )
}
#[derive(Deserialize, utoipa::IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in=Query)]
pub struct PageQuery {
    pub snapshot: Uuid,
    pub offset: Option<usize>,
    /// Select a page by immutable paired-unit identity; excludes offset.
    pub row: Option<String>,
    pub limit: Option<usize>,
}
#[derive(Clone, Copy, Deserialize, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReadableComparisonSide {
    Before,
    After,
}
#[derive(Deserialize, utoipa::IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in=Query)]
pub struct UnitQuery {
    pub snapshot: Uuid,
    pub side: ReadableComparisonSide,
    pub event: Option<usize>,
    /// UTF-8 offset in decoded event text, not original Markdown source.
    pub offset: Option<usize>,
    pub max_bytes: Option<usize>,
}
#[derive(Serialize, utoipa::ToSchema)]
pub struct ReadableFileResponse {
    pub proposal_id: Uuid,
    pub snapshot_id: Uuid,
    pub content_id: String,
    pub merge_base_sha: String,
    pub head_sha: String,
    pub file: Change,
    pub page: ReadablePage,
}
#[derive(Serialize, utoipa::ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReadableUnitState {
    Ready,
    Absent,
    RawRequired,
}
#[derive(Serialize, utoipa::ToSchema)]
pub struct ReadableUnitResponse {
    pub proposal_id: Uuid,
    pub snapshot_id: Uuid,
    pub content_id: String,
    pub merge_base_sha: String,
    pub head_sha: String,
    pub file: Change,
    pub side: ReadableComparisonSide,
    pub unit_index: usize,
    pub state: ReadableUnitState,
    pub error_code: Option<String>,
    pub unit: Option<UnitSummary>,
    pub window: Option<EventWindow>,
}
#[utoipa::path(get,path="/api/v1/proposals/{proposal}/diff/files/{file}/readable",operation_id="proposalReadableDiffFile",security(("sessionCookie"=[])),
    params(("proposal"=Uuid,Path),("file"=String,Path),("X-Expected-Actor"=Uuid,Header),PageQuery),
    responses((status=200,body=ReadableFileResponse,description="Bounded readable metadata or explicit RAW_REQUIRED side state"),
    (status=400,body=ErrorBody),(status=401,body=ErrorBody),(status=404,body=ErrorBody),(status=409,body=ErrorBody),
    (status=413,body=ErrorBody),(status=422,body=ErrorBody),(status=503,body=ErrorBody)))]
pub(crate) async fn page(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<(Uuid, String)>, axum::extract::rejection::PathRejection>,
    query: Result<Query<PageQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let actor = match identity(&state, &headers).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    let (Ok(Path((proposal, file))), Ok(Query(query))) = (path, query) else {
        return invalid();
    };
    let offset = query.offset.unwrap_or(0);
    let limit = query.limit.unwrap_or(50);
    if query.snapshot.is_nil()
        || offset > 20004
        || !(1..=100).contains(&limit)
        || query.row.as_ref().is_some_and(|row| {
            query.offset.is_some()
                || row.len() != 64
                || !row
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
    {
        return invalid();
    }
    let (loaded, page) = match render(
        &state,
        actor,
        proposal,
        query.snapshot,
        &file,
        move |pair| match query.row {
            Some(row) => pair.page_containing(&row, limit),
            None => pair.page(offset, limit),
        },
    )
    .await
    {
        Ok(result) => result,
        Err(error) => return error.into_response(),
    };
    Json(ReadableFileResponse {
        proposal_id: proposal,
        snapshot_id: loaded.snapshot(),
        content_id: loaded.content_id().into(),
        merge_base_sha: loaded.merge_base().as_str().into(),
        head_sha: loaded.head().as_str().into(),
        file: loaded.change().clone(),
        page,
    })
    .into_response()
}
#[utoipa::path(get,path="/api/v1/proposals/{proposal}/diff/files/{file}/readable/units/{unit}",operation_id="proposalReadableDiffUnit",security(("sessionCookie"=[])),
    params(("proposal"=Uuid,Path),("file"=String,Path),("unit"=usize,Path),("X-Expected-Actor"=Uuid,Header),UnitQuery),
    responses((status=200,body=ReadableUnitResponse,description="Bounded event window, explicit absence or RAW_REQUIRED fallback; no approval is implied"),
    (status=400,body=ErrorBody),(status=401,body=ErrorBody),(status=404,body=ErrorBody),(status=409,body=ErrorBody),
    (status=413,body=ErrorBody),(status=422,body=ErrorBody),(status=503,body=ErrorBody)))]
pub(crate) async fn unit(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<(Uuid, String, usize)>, axum::extract::rejection::PathRejection>,
    query: Result<Query<UnitQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let actor = match identity(&state, &headers).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    let (Ok(Path((proposal, file, index))), Ok(Query(query))) = (path, query) else {
        return invalid();
    };
    let cursor = EventCursor {
        event: query.event.unwrap_or(0),
        offset: query.offset.unwrap_or(0),
    };
    let limit = query.max_bytes.unwrap_or(32768);
    if query.snapshot.is_nil()
        || index >= 10002
        || cursor.event > 96000
        || cursor.offset > 8 * 1024 * 1024
        || !(1024..=65536).contains(&limit)
    {
        return invalid();
    }
    let (loaded, result) = match render(
        &state,
        actor,
        proposal,
        query.snapshot,
        &file,
        move |pair| {
            let side = match query.side {
                ReadableComparisonSide::Before => &pair.before,
                ReadableComparisonSide::After => &pair.after,
            };
            match side {
                ReadableSide::Absent => Ok((ReadableUnitState::Absent, None, None, None)),
                ReadableSide::Unavailable { error, .. } => Ok((
                    ReadableUnitState::RawRequired,
                    Some(error.code().into()),
                    None,
                    None,
                )),
                ReadableSide::Ready(document) => {
                    let summary = UnitSummary::from_unit(
                        index,
                        document
                            .units
                            .get(index)
                            .ok_or(ReadableWindowError::InvalidCursor)?,
                    );
                    match document.event_window(index, cursor, limit) {
                        Ok(window) => {
                            Ok((ReadableUnitState::Ready, None, Some(summary), Some(window)))
                        }
                        Err(ReadableWindowError::ProtectedSource) => Ok((
                            ReadableUnitState::RawRequired,
                            Some("READABLE_PROTECTED_SOURCE".into()),
                            Some(summary),
                            None,
                        )),
                        Err(ReadableWindowError::MetadataTooLarge) => Ok((
                            ReadableUnitState::RawRequired,
                            Some("READABLE_WINDOW_TOO_LARGE".into()),
                            Some(summary),
                            None,
                        )),
                        Err(error) => Err(error),
                    }
                }
            }
        },
    )
    .await
    {
        Ok(result) => result,
        Err(error) => return error.into_response(),
    };
    let (status, error_code, summary, window) = result;
    Json(ReadableUnitResponse {
        proposal_id: proposal,
        snapshot_id: loaded.snapshot(),
        content_id: loaded.content_id().into(),
        merge_base_sha: loaded.merge_base().as_str().into(),
        head_sha: loaded.head().as_str().into(),
        file: loaded.change().clone(),
        side: query.side,
        unit_index: index,
        state: status,
        error_code,
        unit: summary,
        window,
    })
    .into_response()
}
async fn identity(state: &AppState, headers: &HeaderMap) -> Result<Uuid, auth::AuthFailure> {
    let actor = auth::authenticate(state, headers).await?;
    auth::verify_actor(headers, &actor)?;
    Ok(actor.user_id)
}
enum RenderFailure {
    File(FileReadError),
    Window(ReadableWindowError),
}
impl IntoResponse for RenderFailure {
    fn into_response(self) -> Response {
        match self {
            Self::File(error) => raw::file_failure(error),
            Self::Window(error) => window_failure(error),
        }
    }
}
async fn render<T: Send + 'static>(
    state: &AppState,
    actor: Uuid,
    proposal: Uuid,
    snapshot: Uuid,
    file: &str,
    build: impl FnOnce(&ReadablePair) -> Result<T, ReadableWindowError> + Send + 'static,
) -> Result<(LoadedFile, T), RenderFailure> {
    let loaded = state
        .comparison_files
        .load(actor, proposal, snapshot, file)
        .await
        .map_err(RenderFailure::File)?;
    let permit = state
        .render_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| RenderFailure::File(FileReadError::Busy))?;
    let cpu = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let pair = ReadablePair::prepare(loaded.before(), loaded.after());
        let before = loaded
            .change()
            .before
            .as_ref()
            .and_then(|side| side.sha256.as_deref());
        let after = loaded
            .change()
            .after
            .as_ref()
            .and_then(|side| side.sha256.as_deref());
        let result = if pair.before.sha256() != before || pair.after.sha256() != after {
            Err(ReadableWindowError::InvalidEvidence)
        } else {
            build(&pair)
        };
        (loaded, result)
    });
    let (loaded, result) = tokio::time::timeout(Duration::from_secs(30), cpu)
        .await
        .map_err(|_| RenderFailure::File(FileReadError::TimedOut))?
        .map_err(|_| RenderFailure::File(FileReadError::Unavailable))?;
    state
        .comparison_files
        .recheck(actor, &loaded)
        .await
        .map_err(RenderFailure::File)?;
    Ok((loaded, result.map_err(RenderFailure::Window)?))
}
fn invalid() -> Response {
    proposals::problem(
        StatusCode::BAD_REQUEST,
        "INVALID_READABLE_QUERY",
        "פרטי ההשוואה הקריאה אינם תקינים. יש להשתמש בגרסה ובמיקום שהוחזרו מההשוואה.",
        false,
        None,
    )
}
fn window_failure(error: ReadableWindowError) -> Response {
    match error {
        ReadableWindowError::InvalidCursor | ReadableWindowError::InvalidLimit => invalid(),
        ReadableWindowError::UnknownRow => proposals::problem(
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            "היחידה המבוקשת אינה קיימת בהשוואה השמורה.",
            false,
            None,
        ),
        ReadableWindowError::InvalidEvidence => raw::file_failure(FileReadError::InvalidEvidence),
        _ => proposals::problem(
            StatusCode::UNPROCESSABLE_ENTITY,
            "READABLE_RAW_REQUIRED",
            "לא ניתן להציג מקטע זה בתצוגה הקריאה. אפשר לפתוח את המקור הגולמי של הגרסה השמורה.",
            false,
            None,
        ),
    }
}
