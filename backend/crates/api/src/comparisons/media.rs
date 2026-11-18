//! Sealed after-image media, including unchanged assets referenced by the review.
use super::*;
use crate::review_configuration::NoQuery;
use specspace_application::comparison_file::image::ImageSide;
use specspace_infrastructure::review_files::PgReviewMediaAuthorization;
use std::sync::Arc;

pub(super) fn routes() -> Router<AppState> {
    Router::new().route(
        "/api/v1/proposals/{proposal}/review/snapshots/{snapshot}/media/{asset}/image",
        get(read),
    )
}
/// Source identities are server-resolved from the sealed media receipt. The
/// image.file_id is a domain-separated media identity, not a changed-file ID.
#[derive(Serialize, utoipa::ToSchema)]
pub struct ReviewMediaEvidence {
    pub asset_path: String,
    pub media_validation_version: u32,
    pub image: image::ImageEvidence,
}
#[utoipa::path(get,path="/api/v1/proposals/{proposal}/review/snapshots/{snapshot}/media/{asset}/image",operation_id="reviewSnapshotMediaImage",security(("sessionCookie"=[])),
    params(("proposal"=Uuid,Path),("snapshot"=Uuid,Path),("asset"=String,Path,description="Sealed content-addressed filename: SHA-256 and png/jpg/jpeg/webp extension"),("X-Expected-Actor"=Uuid,Header)),
    responses((status=200,body=crate::specification::PngDisplay,content_type="image/png",description="Verified after-image from a sealed review media receipt, including referenced unchanged assets",
        headers(("X-Review-Media"=String,description="Unpadded base64url JSON ReviewMediaEvidence; source path, validation version and actor/snapshot/source/display binding"))),
    (status=400,body=ErrorBody),(status=401,body=ErrorBody),(status=404,body=ErrorBody),(status=409,body=ErrorBody),
    (status=413,body=ErrorBody),(status=422,body=ErrorBody),(status=503,body=ErrorBody)))]
pub(crate) async fn read(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<(Uuid, Uuid, String)>, axum::extract::rejection::PathRejection>,
    query: Result<Query<NoQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let actor = match auth::authenticate(&state, &headers).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = auth::verify_actor(&headers, &actor) {
        return error.into_response();
    }
    let (Ok(Path((proposal, snapshot, asset))), Ok(_)) = (path, query) else {
        return proposals::invalid_query();
    };
    let authorization =
        match PgReviewMediaAuthorization::new(state.database.clone(), format!("assets/{asset}")) {
            Ok(authorization) => Arc::new(authorization),
            Err(error) => return image::image_failure(error),
        };
    let file = authorization.file_id().to_owned();
    let reader = state.review_files.with_authorization(authorization);
    let loaded = match reader
        .load_image(actor.user_id, proposal, snapshot, &file, ImageSide::After)
        .await
    {
        Ok(image) => image,
        Err(error) => return image::image_failure(error),
    };
    let (Some(source), Some(proof)) = (
        loaded.context().change.after.as_ref(),
        loaded.context().published_after_image.as_ref(),
    ) else {
        return image::image_failure(
            specspace_application::comparison_file::FileReadError::InvalidEvidence,
        );
    };
    let image = match image::evidence(actor.user_id, &loaded) {
        Ok(image) => image,
        Err(error) => return image::image_failure(error),
    };
    let evidence = ReviewMediaEvidence {
        asset_path: source.path.clone(),
        media_validation_version: proof.validation_version,
        image,
    };
    image::display_response(&reader, actor.user_id, loaded, "X-Review-Media", &evidence).await
}
