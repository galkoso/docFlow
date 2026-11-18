//! Shared review body routes select their authority explicitly. Their immutable
//! snapshot is part of the path; query strings cannot replace it or select refs.
use super::*;
use specspace_application::comparison_file::image::ImageSide;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/proposals/{proposal}/review/snapshots/{snapshot}/files/{file}/raw", get(raw))
        .route("/api/v1/proposals/{proposal}/review/snapshots/{snapshot}/files/{file}/readable", get(page))
        .route("/api/v1/proposals/{proposal}/review/snapshots/{snapshot}/files/{file}/readable/units/{unit}", get(unit))
        .route("/api/v1/proposals/{proposal}/review/snapshots/{snapshot}/files/{file}/image", get(image))
}
#[derive(Deserialize, utoipa::IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in=Query)]
pub struct RawQuery {
    pub chunk: Option<usize>,
    pub offset: Option<usize>,
    /// Source text budget across both sides (1024–65536 bytes).
    pub max_bytes: Option<usize>,
}
#[derive(Deserialize, utoipa::IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in=Query)]
pub struct PageQuery {
    pub offset: Option<usize>,
    /// Immutable paired-unit identity; excludes offset.
    pub row: Option<String>,
    pub limit: Option<usize>,
}
#[derive(Deserialize, utoipa::IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in=Query)]
pub struct UnitQuery {
    pub side: readable::ReadableComparisonSide,
    pub event: Option<usize>,
    pub offset: Option<usize>,
    pub max_bytes: Option<usize>,
}
#[derive(Deserialize, utoipa::IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in=Query)]
pub struct ImageQuery {
    pub side: ImageSide,
}
async fn identity(state: &AppState, headers: &HeaderMap) -> Result<Uuid, auth::AuthFailure> {
    let actor = auth::authenticate(state, headers).await?;
    auth::verify_actor(headers, &actor)?;
    Ok(actor.user_id)
}
#[utoipa::path(get,path="/api/v1/proposals/{proposal}/review/snapshots/{snapshot}/files/{file}/raw",operation_id="reviewSnapshotRawFile",security(("sessionCookie"=[])),
    params(("proposal"=Uuid,Path),("snapshot"=Uuid,Path),("file"=String,Path),("X-Expected-Actor"=Uuid,Header),RawQuery),
    responses((status=200,body=raw::RawFileResponse,description="Verified shared snapshot source window; current organization membership required"),
    (status=400,body=ErrorBody),(status=401,body=ErrorBody),(status=404,body=ErrorBody),(status=409,body=ErrorBody),
    (status=413,body=ErrorBody),(status=422,body=ErrorBody),(status=503,body=ErrorBody)))]
pub(crate) async fn raw(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<(Uuid, Uuid, String)>, axum::extract::rejection::PathRejection>,
    query: Result<Query<RawQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let actor = match identity(&state, &headers).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    let (Ok(Path((proposal, snapshot, file))), Ok(Query(query))) = (path, query) else {
        return proposals::invalid_query();
    };
    raw::respond(
        &state,
        &state.review_files,
        actor,
        proposal,
        &file,
        raw::RawQuery {
            snapshot,
            chunk: query.chunk,
            offset: query.offset,
            max_bytes: query.max_bytes,
        },
    )
    .await
}
#[utoipa::path(get,path="/api/v1/proposals/{proposal}/review/snapshots/{snapshot}/files/{file}/readable",operation_id="reviewSnapshotReadableFile",security(("sessionCookie"=[])),
    params(("proposal"=Uuid,Path),("snapshot"=Uuid,Path),("file"=String,Path),("X-Expected-Actor"=Uuid,Header),PageQuery),
    responses((status=200,body=readable::ReadableFileResponse,description="Bounded shared readable metadata with explicit raw fallback"),
    (status=400,body=ErrorBody),(status=401,body=ErrorBody),(status=404,body=ErrorBody),(status=409,body=ErrorBody),
    (status=413,body=ErrorBody),(status=422,body=ErrorBody),(status=503,body=ErrorBody)))]
pub(crate) async fn page(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<(Uuid, Uuid, String)>, axum::extract::rejection::PathRejection>,
    query: Result<Query<PageQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let actor = match identity(&state, &headers).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    let (Ok(Path((proposal, snapshot, file))), Ok(Query(query))) = (path, query) else {
        return proposals::invalid_query();
    };
    readable::page_response(
        &state,
        &state.review_files,
        actor,
        proposal,
        &file,
        readable::PageQuery {
            snapshot,
            offset: query.offset,
            row: query.row,
            limit: query.limit,
        },
    )
    .await
}
#[utoipa::path(get,path="/api/v1/proposals/{proposal}/review/snapshots/{snapshot}/files/{file}/readable/units/{unit}",operation_id="reviewSnapshotReadableUnit",security(("sessionCookie"=[])),
    params(("proposal"=Uuid,Path),("snapshot"=Uuid,Path),("file"=String,Path),("unit"=usize,Path),("X-Expected-Actor"=Uuid,Header),UnitQuery),
    responses((status=200,body=readable::ReadableUnitResponse,description="Verified bounded shared readable events, absence or raw fallback"),
    (status=400,body=ErrorBody),(status=401,body=ErrorBody),(status=404,body=ErrorBody),(status=409,body=ErrorBody),
    (status=413,body=ErrorBody),(status=422,body=ErrorBody),(status=503,body=ErrorBody)))]
pub(crate) async fn unit(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<(Uuid, Uuid, String, usize)>, axum::extract::rejection::PathRejection>,
    query: Result<Query<UnitQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let actor = match identity(&state, &headers).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    let (Ok(Path((proposal, snapshot, file, index))), Ok(Query(query))) = (path, query) else {
        return proposals::invalid_query();
    };
    readable::unit_response(
        &state,
        &state.review_files,
        actor,
        proposal,
        &file,
        index,
        readable::UnitQuery {
            snapshot,
            side: query.side,
            event: query.event,
            offset: query.offset,
            max_bytes: query.max_bytes,
        },
    )
    .await
}
#[utoipa::path(get,path="/api/v1/proposals/{proposal}/review/snapshots/{snapshot}/files/{file}/image",operation_id="reviewSnapshotImage",security(("sessionCookie"=[])),
    params(("proposal"=Uuid,Path),("snapshot"=Uuid,Path),("file"=String,Path),("X-Expected-Actor"=Uuid,Header),ImageQuery),
    responses((status=200,body=crate::specification::PngDisplay,content_type="image/png",description="Verified shared metadata-free PNG; after side must match sealed media receipt",
        headers(("X-Comparison-Image"=String,description="Unpadded base64url JSON ImageEvidence binding body to actor, snapshot, file, side and source/display SHA-256"))),
    (status=400,body=ErrorBody),(status=401,body=ErrorBody),(status=404,body=ErrorBody),(status=409,body=ErrorBody),
    (status=413,body=ErrorBody),(status=422,body=ErrorBody),(status=503,body=ErrorBody)))]
pub(crate) async fn image(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<(Uuid, Uuid, String)>, axum::extract::rejection::PathRejection>,
    query: Result<Query<ImageQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let actor = match identity(&state, &headers).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    let (Ok(Path((proposal, snapshot, file))), Ok(Query(query))) = (path, query) else {
        return proposals::invalid_query();
    };
    image::respond(
        &state.review_files,
        actor,
        proposal,
        &file,
        image::ImageQuery {
            snapshot,
            side: query.side,
        },
    )
    .await
}
