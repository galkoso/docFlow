use utoipa::{
    Modify, OpenApi,
    openapi::security::{ApiKey, ApiKeyValue, SecurityScheme},
};

#[derive(OpenApi)]
#[openapi(
    info(title = "SpecSpace API", version = "0.1.0"),
    paths(crate::live, crate::ready, crate::auth::login, crate::auth::availability, crate::auth::callback, crate::auth::logout, crate::auth::me,
        crate::specification::repositories, crate::specification::specification, crate::specification::document, crate::specification::find),
    components(schemas(crate::Health, crate::ErrorBody, crate::ApiError, crate::auth::Profile, crate::auth::Membership, crate::auth::ProfileCapabilities, crate::auth::AuthAvailability)),
    modifiers(&SessionSecurity)
)]
pub struct ApiDoc;

struct SessionSecurity;

impl Modify for SessionSecurity {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        openapi.info.license = None;
        if let Some(components) = &mut openapi.components {
            components.add_security_scheme(
                "sessionCookie",
                SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::new(
                    "__Host-specspace-session",
                ))),
            );
        }
    }
}
