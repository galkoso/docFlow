use crate::{AppState, ErrorBody, auth, proposals};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Deserialize;
use specspace_application::review_configuration::{
    ConfigurationUpdated, ReviewConfiguration, ReviewConfigurationRequest,
};
use specspace_infrastructure::review_configuration::{ConfigurationError, PgReviewConfiguration};
use uuid::Uuid;
pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/api/v1/proposals/{proposal}/review/configuration",
        get(read)
            .put(update)
            .layer(DefaultBodyLimit::max(128 * 1024)),
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NoQuery {}
#[utoipa::path(get,path="/api/v1/proposals/{proposal}/review/configuration",operation_id="proposalReviewConfiguration",security(("sessionCookie"=[])),
    params(("proposal"=Uuid,Path),("X-Expected-Actor"=Uuid,Header)),
    responses((status=200,body=ReviewConfiguration),(status=400,body=ErrorBody),(status=401,body=ErrorBody),(status=404,body=ErrorBody),(status=409,body=ErrorBody),(status=503,body=ErrorBody)))]
pub(crate) async fn read(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, axum::extract::rejection::PathRejection>,
    query: Result<Query<NoQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let actor = match auth::authenticate(&state, &headers).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = auth::verify_actor(&headers, &actor) {
        return error.into_response();
    }
    let (Ok(Path(proposal)), Ok(_)) = (path, query) else {
        return proposals::invalid_query();
    };
    match PgReviewConfiguration::new(state.database)
        .read(actor.user_id, proposal)
        .await
    {
        Ok(configuration) => Json(configuration).into_response(),
        Err(error) => failure(error),
    }
}
#[utoipa::path(put,path="/api/v1/proposals/{proposal}/review/configuration",operation_id="updateProposalReviewConfiguration",security(("sessionCookie"=[])),
    params(("proposal"=Uuid,Path),("X-Expected-Actor"=Uuid,Header),("Origin"=String,Header),("X-XSRF-TOKEN"=String,Header)),request_body=ReviewConfigurationRequest,
    responses((status=200,body=ConfigurationUpdated,description="Versioned metadata saved or identical request replayed; no submission/approval is claimed"),
    (status=400,body=ErrorBody),(status=401,body=ErrorBody),(status=403,body=ErrorBody),(status=404,body=ErrorBody),(status=409,body=ErrorBody),(status=413,body=ErrorBody),(status=422,body=ErrorBody),(status=503,body=ErrorBody)))]
pub(crate) async fn update(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, axum::extract::rejection::PathRejection>,
    query: Result<Query<NoQuery>, axum::extract::rejection::QueryRejection>,
    body: Result<Json<ReviewConfigurationRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let actor = match auth::authenticate(&state, &headers).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = auth::verify_actor(&headers, &actor) {
        return error.into_response();
    }
    if let Err(error) = auth::verify_write(&state, &headers, &actor) {
        return error.into_response();
    }
    let (Ok(Path(proposal)), Ok(_)) = (path, query) else {
        return proposals::invalid_query();
    };
    let request = match body {
        Ok(Json(request)) => request,
        Err(error) => {
            return proposals::problem(
                if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
                    StatusCode::PAYLOAD_TOO_LARGE
                } else {
                    StatusCode::BAD_REQUEST
                },
                "INVALID_REVIEW_CONFIGURATION",
                "פרטי הגדרת הסקירה אינם תקינים.",
                false,
                None,
            );
        }
    };
    match PgReviewConfiguration::new(state.database)
        .update(actor.user_id, proposal, request)
        .await
    {
        Ok(result) => Json(result).into_response(),
        Err(error) => failure(error),
    }
}
fn failure(error: ConfigurationError) -> Response {
    let (status, code, message, retry) = match error {
        ConfigurationError::NotFound => (
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            "ההצעה המבוקשת לא נמצאה.",
            false,
        ),
        ConfigurationError::Forbidden => (
            StatusCode::FORBIDDEN,
            "REVIEW_CONFIGURATION_FORBIDDEN",
            "אין הרשאה לשנות את הגדרת הסקירה בהצעה זו.",
            false,
        ),
        ConfigurationError::Stale => (
            StatusCode::CONFLICT,
            "REVIEW_CONFIGURATION_STALE",
            "ההצעה או הגדרת הסקירה השתנו. יש לטעון את המצב הנוכחי לפני שמירה חדשה.",
            false,
        ),
        ConfigurationError::Busy => (
            StatusCode::CONFLICT,
            "PROPOSAL_BUSY",
            "פעולת שמירה או פרסום קודמת טרם הושלמה. יש לבדוק את תוצאתה לפני שינוי ההגדרה.",
            false,
        ),
        ConfigurationError::StateChanged => (
            StatusCode::CONFLICT,
            "PROPOSAL_NOT_EDITABLE",
            "מצב ההצעה אינו מאפשר שינוי הגדרת סקירה.",
            false,
        ),
        ConfigurationError::KeyConflict => (
            StatusCode::CONFLICT,
            "IDEMPOTENCY_KEY_REUSED",
            "מזהה הבקשה כבר משויך להגדרה אחרת. יש להשתמש במזהה חדש לשינוי חדש.",
            false,
        ),
        ConfigurationError::IneligibleReviewer => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "REVIEWER_NOT_ELIGIBLE",
            "אחד הסוקרים אינו פעיל, אינו כשיר בתחום שנבחר או הוא מחבר ההצעה.",
            false,
        ),
        ConfigurationError::Invalid(code) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            code,
            "פרטי הגדרת הסקירה אינם תקינים. יש לבדוק את התחומים ואת נימוק השפעת הממשק.",
            false,
        ),
        ConfigurationError::Unavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            "REVIEW_CONFIGURATION_UNAVAILABLE",
            "לא ניתן לבדוק כרגע את תוצאת ההגדרה. אפשר לנסות שוב את אותה בקשה.",
            true,
        ),
    };
    proposals::problem(status, code, message, retry, None)
}
