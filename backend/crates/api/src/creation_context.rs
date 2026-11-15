use crate::{
    AppState, auth,
    proposals::{failure, invalid_query, operation_key},
    specification,
};
use axum::{
    Json,
    extract::State,
    http::HeaderMap,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use specspace_application::book::BookError;
use specspace_infrastructure::{
    creation_context::{CreationContext, PgCreationContexts},
    proposals::PgProposals,
};
use uuid::Uuid;

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PrepareCreation {
    pub repository_id: Uuid,
}
#[derive(Serialize, utoipa::ToSchema)]
pub struct PreparedCreation {
    pub context_id: Uuid,
    pub repository_id: Uuid,
    pub number: i64,
    pub source_branch: &'static str,
    pub source_sha: String,
    pub expires_at: String,
}
fn prepared(context: CreationContext) -> Response {
    Json(PreparedCreation {
        context_id: context.context_id,
        repository_id: context.repository_id,
        number: context.number,
        source_branch: "main",
        source_sha: context.source_sha,
        expires_at: context.expires_at,
    })
    .into_response()
}
#[utoipa::path(post,path="/api/v1/proposals/context",security(("sessionCookie"=[])),request_body=PrepareCreation,
    params(("Origin"=String,Header),("X-XSRF-TOKEN"=String,Header),("Idempotency-Key"=String,Header)),
    responses((status=200,body=PreparedCreation),(status=400,body=crate::ErrorBody),(status=401,body=crate::ErrorBody),(status=403,body=crate::ErrorBody),(status=404,body=crate::ErrorBody),(status=409,body=crate::ErrorBody),(status=422,body=crate::ErrorBody),(status=502,body=crate::ErrorBody),(status=503,body=crate::ErrorBody)))]
pub(crate) async fn prepare(
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Result<Json<PrepareCreation>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let actor = match auth::authenticate(&state, &headers).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = auth::verify_write(&state, &headers, &actor) {
        return error.into_response();
    }
    let Ok(Json(input)) = payload else {
        return invalid_query();
    };
    let key = match operation_key(&headers) {
        Ok(key) => key,
        Err(error) => return error.into_response(),
    };
    let contexts = PgCreationContexts::new(state.database.clone());
    match contexts
        .existing(actor.user_id, input.repository_id, &key)
        .await
    {
        Ok(Some(context)) => return prepared(context),
        Ok(None) => {}
        Err(error) => return failure(error),
    }
    let provider = match state.book_source.provider(input.repository_id).await {
        Ok(provider) => provider,
        Err(error) => return specification::failure(error),
    };
    let source = match provider.main_head().await {
        Ok(source) => source,
        Err(error) => return specification::failure(BookError::Provider(error)),
    };
    if let Err(error) = PgProposals::new(state.database.clone())
        .authorize_create(actor.user_id, input.repository_id)
        .await
    {
        return failure(error);
    }
    match contexts
        .allocate(actor.user_id, input.repository_id, &source, &key)
        .await
    {
        Ok(context) => prepared(context),
        Err(error) => failure(error),
    }
}
