use utoipa::{
    Modify, OpenApi,
    openapi::security::{ApiKey, ApiKeyValue, SecurityScheme},
};

#[derive(OpenApi)]
#[openapi(
    info(title = "SpecSpace API", version = "0.1.0"),
    paths(crate::review_readiness::stored, crate::unit_approvals::page, crate::unit_approvals::approve, crate::review_visits::coverage, crate::review_visits::source, crate::review_visits::acknowledge, crate::discussions::open, crate::discussions::mutate, crate::discussions::threads, crate::discussions::comments, crate::official_reviews::history, crate::official_reviews::record, crate::comparisons::media::before, crate::comparisons::media::read, crate::comparisons::shared::raw, crate::comparisons::shared::page, crate::comparisons::shared::unit, crate::comparisons::shared::image, crate::reviews::submit, crate::reviews::history, crate::reviews::pr_status, crate::reviews::recover_pr, crate::reviews::snapshot, crate::reviews::files, crate::reviews::documents, crate::reviews::units, crate::reviews::media, crate::review_configuration::read, crate::review_configuration::update, crate::review_configuration::candidates, crate::live, crate::ready, crate::auth::login, crate::auth::availability, crate::auth::callback, crate::auth::logout, crate::auth::me,
        crate::specification::repositories, crate::specification::specification, crate::specification::document, crate::specification::find, crate::specification::asset,
        crate::specification::highlights, crate::proposals::create, crate::proposals::job_status, crate::proposal_reads::list, crate::proposal_reads::detail, crate::proposal_reads::options, crate::creation_context::prepare,
        crate::proposal_content::specification, crate::proposal_content::document, crate::proposal_content::find, crate::proposal_content::highlights, crate::proposal_content::asset,
        crate::comparisons::read, crate::comparisons::image::read, crate::comparisons::raw::read, crate::comparisons::readable::page, crate::comparisons::readable::unit, crate::saves::submit, crate::drafts::read, crate::drafts::save, crate::drafts::discard, crate::editor::model, crate::editor::preview,
        crate::editor::source_mode::read, crate::editor::source_mode::preview),
    components(schemas(specspace_application::unit_approval::UnitApprovalPageRequest, specspace_application::review_visits::ReviewCoverageRequest, specspace_application::review_visits::ReviewSourceRequest,crate::comparisons::media::ReviewBeforeMediaEvidence, crate::comparisons::media::ReviewMediaEvidence, crate::comparisons::image::ImageEvidence, crate::Health, crate::ErrorBody, crate::ApiError, crate::auth::Profile, crate::auth::Membership, crate::auth::ProfileCapabilities,
        crate::auth::AuthAvailability, specspace_document_engine::find::MatchWindow)),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_operations_do_not_collide_with_file_categories() {
        let document = serde_json::to_value(ApiDoc::openapi()).unwrap();
        let schemas = &document["components"]["schemas"];
        assert_eq!(
            schemas["RawSegment"]["properties"]["kind"]["$ref"],
            "#/components/schemas/RawDiffKind"
        );
        assert_eq!(
            schemas["RawDiffKind"]["enum"],
            serde_json::json!(["equal", "delete", "insert"])
        );
        assert_eq!(
            schemas["Kind"]["enum"],
            serde_json::json!(["document", "asset", "catalog", "other"])
        );
    }

    #[test]
    fn operation_ids_are_unique_for_generated_clients() {
        let document = serde_json::to_value(ApiDoc::openapi()).unwrap();
        let mut ids = std::collections::HashSet::new();
        for path in document["paths"].as_object().unwrap().values() {
            for operation in path.as_object().unwrap().values() {
                if let Some(id) = operation["operationId"].as_str() {
                    assert!(ids.insert(id), "duplicate OpenAPI operationId {id}");
                }
            }
        }
    }
}
