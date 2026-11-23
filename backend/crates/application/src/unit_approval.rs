//! Explicit approval of one sealed mandatory review item, separate from visits,
//! official snapshot decisions and publication.
use crate::review_configuration::ReviewArea;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

fn digest(v: &str) -> bool {
    v.len() == 64
        && v.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UnitApprovalRequest {
    pub request_id: Uuid,
    pub expected_fingerprint: String,
    pub domain: ReviewArea,
    pub unit_id: String,
}
impl UnitApprovalRequest {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.request_id.is_nil() || !digest(&self.expected_fingerprint) || !digest(&self.unit_id)
        {
            return Err("INVALID_UNIT_APPROVAL");
        }
        Ok(())
    }
}
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UnitApprovalRecorded {
    pub id: Uuid,
    pub request_id: Uuid,
    pub actor_id: Uuid,
    pub proposal_id: Uuid,
    pub snapshot_id: Uuid,
    pub fingerprint: String,
    pub domain: ReviewArea,
    pub unit_id: String,
    pub head_sha: String,
    pub target_sha: String,
    pub policy_version: i64,
    pub configuration_version: i64,
    pub recorded_at: String,
    pub replayed: bool,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UnitApprovalPageRequest {
    pub expected_fingerprint: String,
    pub domain: ReviewArea,
    pub offset: i32,
    pub limit: i32,
}
impl UnitApprovalPageRequest {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !digest(&self.expected_fingerprint)
            || !(0..=40000).contains(&self.offset)
            || !(1..=100).contains(&self.limit)
        {
            return Err("INVALID_UNIT_APPROVAL_PAGE");
        }
        Ok(())
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UnitApprovalStatus {
    pub unit_id: String,
    pub approval_id: Option<Uuid>,
    pub approved_at: Option<String>,
}
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UnitApprovalPage {
    pub actor_id: Uuid,
    pub proposal_id: Uuid,
    pub snapshot_id: Uuid,
    pub fingerprint: String,
    pub head_sha: String,
    pub target_sha: String,
    pub policy_version: i64,
    pub configuration_version: i64,
    pub domain: ReviewArea,
    pub current_for_metadata: bool,
    pub available: bool,
    pub total_units: i32,
    pub approved_units: i32,
    pub unapproved_units: i32,
    pub offset: i32,
    pub next_offset: Option<i32>,
    pub units: Vec<UnitApprovalStatus>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn individual_intents_require_exact_fingerprint_unit_nonce_and_known_fields() {
        let body = serde_json::json!({"request_id":Uuid::new_v4(),"expected_fingerprint":"a".repeat(64),"domain":"PRODUCT","unit_id":"b".repeat(64)});
        let request: UnitApprovalRequest = serde_json::from_value(body.clone()).unwrap();
        assert!(request.validate().is_ok());
        for (key, value) in [
            ("request_id", Uuid::nil().to_string()),
            ("expected_fingerprint", "A".repeat(64)),
            ("unit_id", "b".repeat(63)),
        ] {
            let mut invalid = body.clone();
            invalid[key] = value.into();
            assert!(
                serde_json::from_value::<UnitApprovalRequest>(invalid)
                    .unwrap()
                    .validate()
                    .is_err()
            );
        }
        for key in ["actor_id", "viewed", "approved", "source"] {
            let mut invalid = body.clone();
            invalid[key] = true.into();
            assert!(serde_json::from_value::<UnitApprovalRequest>(invalid).is_err());
        }
    }
}
