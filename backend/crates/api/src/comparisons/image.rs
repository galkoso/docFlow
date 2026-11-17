use super::*;
use crate::specification::PngDisplay;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use specspace_application::comparison_file::{FileReadError, image::ImageSide};

pub(super) fn routes() -> Router<AppState> {
    Router::new().route(
        "/api/v1/proposals/{proposal}/diff/files/{file}/image",
        get(read),
    )
}
#[derive(Deserialize, utoipa::IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in=Query)]
pub struct ImageQuery {
    pub snapshot: Uuid,
    pub side: ImageSide,
}
/// ASCII-only identities and bounded display metadata. Serialized as unpadded
/// base64url JSON in X-Comparison-Image; the body is only re-encoded PNG pixels.
#[derive(Serialize, utoipa::ToSchema)]
pub struct ImageEvidence {
    pub actor_id: Uuid,
    pub proposal_id: Uuid,
    pub snapshot_id: Uuid,
    pub content_id: String,
    pub file_id: String,
    pub side: ImageSide,
    pub merge_base_sha: String,
    pub head_sha: String,
    pub source_blob_oid: String,
    pub source_sha256: String,
    pub source_bytes: u64,
    pub display_sha256: String,
    pub display_bytes: usize,
    pub width: u32,
    pub height: u32,
}
#[utoipa::path(get,path="/api/v1/proposals/{proposal}/diff/files/{file}/image",operation_id="proposalComparisonImage",security(("sessionCookie"=[])),
    params(("proposal"=Uuid,Path),("file"=String,Path),("X-Expected-Actor"=Uuid,Header),ImageQuery),
    responses((status=200,body=PngDisplay,content_type="image/png",description="Verified metadata-free PNG display derivative, never original bytes",
        headers(("X-Comparison-Image"=String,description="Unpadded base64url JSON ImageEvidence binding this body to actor/snapshot/file/side and source/display SHA-256"))),
    (status=400,body=ErrorBody),(status=401,body=ErrorBody),(status=404,body=ErrorBody),(status=409,body=ErrorBody),
    (status=413,body=ErrorBody),(status=422,body=ErrorBody),(status=503,body=ErrorBody)))]
pub(crate) async fn read(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<(Uuid, String)>, axum::extract::rejection::PathRejection>,
    query: Result<Query<ImageQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let actor = match auth::authenticate(&state, &headers).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = auth::verify_actor(&headers, &actor) {
        return error.into_response();
    }
    let (Ok(Path((proposal, file))), Ok(Query(query))) = (path, query) else {
        return invalid();
    };
    if query.snapshot.is_nil() {
        return invalid();
    }
    let loaded = match state
        .comparison_files
        .load_image(actor.user_id, proposal, query.snapshot, &file, query.side)
        .await
    {
        Ok(loaded) => loaded,
        Err(error) => return image_failure(error),
    };
    let context = loaded.context();
    let source = match query.side {
        ImageSide::Before => context.change.before.as_ref(),
        ImageSide::After => context.change.after.as_ref(),
    };
    let Some(source) = source else {
        return image_failure(FileReadError::InvalidEvidence);
    };
    let evidence = ImageEvidence {
        actor_id: actor.user_id,
        proposal_id: proposal,
        snapshot_id: context.snapshot,
        content_id: context.content_id.clone(),
        file_id: context.change.id.clone(),
        side: loaded.side(),
        merge_base_sha: context.merge_base.as_str().into(),
        head_sha: context.head.as_str().into(),
        source_blob_oid: source.blob_oid.clone(),
        source_sha256: loaded.source_sha256().into(),
        source_bytes: source.bytes,
        display_sha256: loaded.display_sha256().into(),
        display_bytes: loaded.image().png.len(),
        width: loaded.image().width,
        height: loaded.image().height,
    };
    let value = match serde_json::to_vec(&evidence)
        .ok()
        .and_then(|json| HeaderValue::from_str(&URL_SAFE_NO_PAD.encode(json)).ok())
    {
        Some(value) => value,
        None => return image_failure(FileReadError::Unavailable),
    };
    if let Err(error) = state
        .comparison_files
        .recheck_image(actor.user_id, &loaded)
        .await
    {
        return image_failure(error);
    }
    let mut response = loaded.into_image().png.into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("image/png"));
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("inline; filename=\"specspace-comparison.png\""),
    );
    response.headers_mut().insert("X-Comparison-Image", value);
    response
}
fn invalid() -> Response {
    proposals::problem(
        StatusCode::BAD_REQUEST,
        "INVALID_COMPARISON_IMAGE",
        "יש לבחור צד וגרסת השוואה שמורה לתמונה.",
        false,
        None,
    )
}
fn image_failure(error: FileReadError) -> Response {
    match error {
        FileReadError::UnsupportedKind => proposals::problem(
            StatusCode::UNPROCESSABLE_ENTITY,
            "COMPARISON_FILE_NOT_IMAGE",
            "הקובץ שנבחר אינו תמונה להשוואה.",
            false,
            None,
        ),
        FileReadError::InvalidImage(code) => proposals::problem(
            StatusCode::UNPROCESSABLE_ENTITY,
            code,
            "לא ניתן להציג תמונה זו: סוג הקובץ, נתיב התוכן או נתוני התמונה אינם תקינים.",
            false,
            None,
        ),
        FileReadError::TooLarge => proposals::problem(
            StatusCode::PAYLOAD_TOO_LARGE,
            "COMPARISON_IMAGE_TOO_LARGE",
            "התמונה גדולה ממגבלת התצוגה המותרת.",
            false,
            None,
        ),
        error => raw::file_failure(error),
    }
}
