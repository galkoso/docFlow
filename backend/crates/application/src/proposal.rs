//! Validated proposal creation intent. Validation and hashing happen before any
//! provider mutation; persistence rechecks current scope when reserving work.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use specspace_domain::BranchName;
use specspace_git_adapter::CommitSha;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum InitialMode {
    Manual,
    Ai,
}
impl InitialMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Ai => "ai",
        }
    }
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum UiImpact {
    Yes,
    No,
    Undecided,
}
impl UiImpact {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Yes => "YES",
            Self::No => "NO",
            Self::Undecided => "UNDECIDED",
        }
    }
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum LinkKind {
    Figma,
    Ticket,
    DevelopmentPr,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ProposalLink {
    pub kind: LinkKind,
    pub url: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CreateProposal {
    pub repository_id: Uuid,
    pub title: String,
    pub branch: String,
    pub source_branch: String,
    pub expected_source_sha: String,
    pub description: String,
    pub team_id: Uuid,
    pub owner_id: Option<Uuid>,
    pub initial_mode: InitialMode,
    pub ui_impact: UiImpact,
    pub ui_impact_reason: Option<String>,
    #[serde(default)]
    pub links: Vec<ProposalLink>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct FieldError {
    pub field: &'static str,
    pub code: &'static str,
}
#[derive(Debug, PartialEq, Eq)]
pub enum ProposalError {
    Validation(Vec<FieldError>),
    NotFound,
    Forbidden,
    BranchTaken,
    IdempotencyMismatch,
    Unavailable,
}

#[derive(Clone, Debug)]
pub struct IdempotencyKey(String);
impl IdempotencyKey {
    pub fn parse(key: impl Into<String>) -> Result<Self, ProposalError> {
        let key = key.into();
        if key.is_empty() || key.len() > 200 || !key.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(ProposalError::Validation(vec![FieldError {
                field: "idempotency_key",
                code: "INVALID_IDEMPOTENCY_KEY",
            }]));
        }
        Ok(Self(key))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub struct ValidatedProposal {
    request: CreateProposal,
    branch: BranchName,
    source: CommitSha,
    fingerprint: String,
}
impl ValidatedProposal {
    pub fn validate(mut request: CreateProposal, actor: Uuid) -> Result<Self, ProposalError> {
        let mut errors = vec![];
        let mut invalid = |field, code| errors.push(FieldError { field, code });
        if !(3..=120).contains(&request.title.chars().count())
            || request.title.trim().is_empty()
            || request.title.chars().any(char::is_control)
        {
            invalid("title", "INVALID_TITLE");
        }
        if !(10..=10000).contains(&request.description.chars().count())
            || request.description.trim().is_empty()
            || request
                .description
                .chars()
                .any(|c| c.is_control() && c != '\n' && c != '\t')
        {
            invalid("description", "INVALID_DESCRIPTION");
        }
        let branch = BranchName::parse(request.branch.clone());
        if branch.is_err() {
            invalid("branch", "INVALID_BRANCH");
        }
        if request.source_branch != "main" {
            invalid("source_branch", "INVALID_SOURCE_BRANCH");
        }
        let source = CommitSha::parse(&request.expected_source_sha);
        if source.is_err() {
            invalid("expected_source_sha", "INVALID_SOURCE_SHA");
        }
        if request.ui_impact_reason.as_ref().is_some_and(|reason| {
            reason.chars().count() > 10000
                || reason
                    .chars()
                    .any(|c| c.is_control() && c != '\n' && c != '\t')
        }) || (request.ui_impact == UiImpact::No
            && request
                .ui_impact_reason
                .as_ref()
                .is_none_or(|reason| reason.trim().is_empty()))
        {
            invalid("ui_impact_reason", "UI_IMPACT_REASON_REQUIRED_OR_INVALID");
        }
        if request.links.len() > 20
            || request.links.iter().any(|link| {
                if link.url.len() > 2000
                    || link
                        .url
                        .chars()
                        .any(|c| c.is_control() || c.is_whitespace())
                {
                    return true;
                }
                url::Url::parse(&link.url).map_or(true, |url| {
                    url.scheme() != "https"
                        || url.host_str().is_none()
                        || !url.username().is_empty()
                        || url.password().is_some()
                })
            })
        {
            invalid("links", "INVALID_PROPOSAL_LINKS");
        }
        if !errors.is_empty() {
            return Err(ProposalError::Validation(errors));
        }
        request.owner_id = Some(request.owner_id.unwrap_or(actor));
        let source = source.map_err(|_| ProposalError::Unavailable)?;
        request.expected_source_sha = source.as_str().into();
        let fingerprint = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&request).map_err(|_| ProposalError::Unavailable)?)
        );
        Ok(Self {
            request,
            branch: branch.map_err(|_| ProposalError::Unavailable)?,
            source,
            fingerprint,
        })
    }
    pub fn request(&self) -> &CreateProposal {
        &self.request
    }
    pub fn branch(&self) -> &BranchName {
        &self.branch
    }
    pub fn source(&self) -> &CommitSha {
        &self.source
    }
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> CreateProposal {
        CreateProposal {
            repository_id: Uuid::new_v4(),
            title: "הצעה חדשה".into(),
            branch: "change/25-new-proposal".into(),
            source_branch: "main".into(),
            expected_source_sha: "1".repeat(40),
            description: "תיאור מלא של השינוי המוצע".into(),
            team_id: Uuid::new_v4(),
            owner_id: None,
            initial_mode: InitialMode::Manual,
            ui_impact: UiImpact::Undecided,
            ui_impact_reason: None,
            links: vec![],
        }
    }
    #[test]
    fn accepts_hebrew_metadata_but_validates_source_refs_fields_and_optional_links() {
        let actor = Uuid::new_v4();
        let mut input = request();
        assert!(ValidatedProposal::validate(input.clone(), actor).is_ok());
        input.title = "x".into();
        input.branch = "main".into();
        input.source_branch = "change/other".into();
        input.expected_source_sha = "short".into();
        input.description = " ".repeat(20);
        input.ui_impact = UiImpact::No;
        input.links.push(ProposalLink {
            kind: LinkKind::Ticket,
            url: "https://name:secret@example.test/ticket".into(),
        });
        let Err(ProposalError::Validation(errors)) = ValidatedProposal::validate(input, actor)
        else {
            panic!("invalid input accepted")
        };
        assert_eq!(errors.len(), 7);
        assert!(errors.iter().any(|error| error.field == "ui_impact_reason"));
        for value in ["", " ", "line\nbreak", &"x".repeat(201)] {
            assert!(IdempotencyKey::parse(value).is_err());
        }
        assert!(IdempotencyKey::parse(Uuid::new_v4().to_string()).is_ok());
    }
    #[test]
    fn idempotency_hash_includes_intent_and_resolves_the_default_owner() {
        let actor = Uuid::new_v4();
        let input = request();
        let baseline = ValidatedProposal::validate(input.clone(), actor).unwrap();
        let mut explicit = input.clone();
        explicit.owner_id = Some(actor);
        assert_eq!(
            baseline.fingerprint(),
            ValidatedProposal::validate(explicit, actor)
                .unwrap()
                .fingerprint()
        );
        let mut changed = input.clone();
        changed.expected_source_sha = "2".repeat(40);
        assert_ne!(
            baseline.fingerprint(),
            ValidatedProposal::validate(changed, actor)
                .unwrap()
                .fingerprint()
        );
        let mut changed = input;
        changed.initial_mode = InitialMode::Ai;
        assert_ne!(
            baseline.fingerprint(),
            ValidatedProposal::validate(changed, actor)
                .unwrap()
                .fingerprint()
        );
    }
}
