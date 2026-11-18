//! Sealed after-image media, including unchanged assets referenced by the review.
use super::*;
use crate::review_configuration::NoQuery;
use specspace_application::comparison_file::image::ImageSide;
use specspace_infrastructure::review_files::PgReviewMediaAuthorization;
use std::sync::Arc;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/proposals/{proposal}/review/snapshots/{snapshot}/media/{asset}/image", get(read))
        .route("/api/v1/proposals/{proposal}/review/snapshots/{snapshot}/files/{file}/before-media/{asset}/image", get(before))
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

/// Before-media derives from a verified document reference and pinned base-tree
/// entry. It is not an after receipt or a newly published review manifest.
#[derive(Serialize, utoipa::ToSchema)]
pub struct ReviewBeforeMediaEvidence {
    pub asset_path: String,
    pub media_validation_version: u32,
    pub document_path: String,
    pub document_blob_oid: String,
    pub document_sha256: String,
    pub image: image::ImageEvidence,
}
#[utoipa::path(get,path="/api/v1/proposals/{proposal}/review/snapshots/{snapshot}/files/{file}/before-media/{asset}/image",operation_id="reviewSnapshotBeforeMediaImage",security(("sessionCookie"=[])),
    params(("proposal"=Uuid,Path),("snapshot"=Uuid,Path),("file"=String,Path),("asset"=String,Path),("X-Expected-Actor"=Uuid,Header)),
    responses((status=200,body=crate::specification::PngDisplay,content_type="image/png",description="Verified before-image referenced by the exact before-document, read from the frozen merge base",
        headers(("X-Review-Before-Media"=String,description="Unpadded base64url JSON ReviewBeforeMediaEvidence; document reference, pinned base source and verified display identities"))),
    (status=400,body=ErrorBody),(status=401,body=ErrorBody),(status=404,body=ErrorBody),(status=409,body=ErrorBody),
    (status=413,body=ErrorBody),(status=422,body=ErrorBody),(status=503,body=ErrorBody)))]
pub(crate) async fn before(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<(Uuid, Uuid, String, String)>, axum::extract::rejection::PathRejection>,
    query: Result<Query<NoQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let actor = match auth::authenticate(&state, &headers).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = auth::verify_actor(&headers, &actor) {
        return error.into_response();
    }
    let (Ok(Path((proposal, snapshot, file, asset))), Ok(_)) = (path, query) else {
        return proposals::invalid_query();
    };
    let reader = &state.review_files;
    let loaded = match reader
        .load_before_image(
            actor.user_id,
            proposal,
            snapshot,
            &file,
            &format!("assets/{asset}"),
        )
        .await
    {
        Ok(loaded) => loaded,
        Err(specspace_application::comparison_file::FileReadError::UnsupportedKind) => {
            return proposals::problem(
                StatusCode::UNPROCESSABLE_ENTITY,
                "BEFORE_MEDIA_DOCUMENT_REQUIRED",
                "יש לבחור מסמך מהגרסה המשותפת כדי לבדוק תמונה בצד לפני.",
                false,
                None,
            );
        }
        Err(specspace_application::comparison_file::FileReadError::InvalidImage(
            code @ ("BEFORE_MEDIA_SIDE_ABSENT"
            | "BEFORE_MEDIA_REFERENCE_NOT_FOUND"
            | "BEFORE_MEDIA_SOURCE_MISSING"
            | "BEFORE_MEDIA_REFERENCE_UNAVAILABLE"
            | "BEFORE_MEDIA_TREE_LIMIT"),
        )) => {
            return proposals::problem(
                StatusCode::UNPROCESSABLE_ENTITY,
                code,
                "לא ניתן לאמת את התמונה מתוך המסמך וגרסת הבסיס בצד לפני. ניתן לבדוק את המקור הגולמי.",
                false,
                None,
            );
        }
        Err(error) => return image::image_failure(error),
    };
    let Some(document) = loaded.image().context().change.before.as_ref() else {
        return image::image_failure(
            specspace_application::comparison_file::FileReadError::InvalidEvidence,
        );
    };
    let Some(document_sha256) = &document.sha256 else {
        return image::image_failure(
            specspace_application::comparison_file::FileReadError::InvalidEvidence,
        );
    };
    let evidence = match image::evidence_for_source(actor.user_id, loaded.image(), loaded.source())
    {
        Ok(evidence) => evidence,
        Err(error) => return image::image_failure(error),
    };
    let evidence = ReviewBeforeMediaEvidence {
        asset_path: loaded.source().path.clone(),
        media_validation_version: specspace_document_engine::media::VALIDATION_VERSION,
        document_path: document.path.clone(),
        document_blob_oid: document.blob_oid.clone(),
        document_sha256: document_sha256.clone(),
        image: evidence,
    };
    image::display_response(
        reader,
        actor.user_id,
        loaded.into_image(),
        "X-Review-Before-Media",
        &evidence,
    )
    .await
}
