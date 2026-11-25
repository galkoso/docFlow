//! Manual question intents contain no actor, source, state or approval claims.
use serde::{Deserialize, Serialize};
use specspace_domain::product_question::{QuestionAction, QuestionText};
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CreateProductQuestion {
    pub request_id: Uuid,
    #[cfg_attr(feature = "openapi", schema(minimum = 1))]
    pub expected_proposal_version: i64,
    #[cfg_attr(feature = "openapi", schema(min_length = 1, max_length = 10000))]
    pub text: String,
    pub blocking: bool,
    #[cfg_attr(feature = "openapi", schema(min_length = 1, max_length = 10000))]
    pub proposed_answer: Option<String>,
}
impl CreateProductQuestion {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.request_id.is_nil()
            || self.expected_proposal_version < 1
            || QuestionText::new(&self.text).is_err()
            || self
                .proposed_answer
                .as_deref()
                .is_some_and(|a| QuestionText::new(a).is_err())
        {
            return Err("INVALID_PRODUCT_QUESTION");
        }
        Ok(())
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ChangeProductQuestion {
    pub request_id: Uuid,
    #[cfg_attr(feature = "openapi", schema(minimum = 1))]
    pub expected_version: i64,
    pub action: ProductQuestionAction,
}
impl ChangeProductQuestion {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.request_id.is_nil() || self.expected_version < 1 || self.action.domain().is_err() {
            return Err("INVALID_PRODUCT_QUESTION");
        }
        Ok(())
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum ProductQuestionAction {
    Propose {
        #[cfg_attr(feature = "openapi", schema(min_length = 1, max_length = 10000))]
        answer: String,
    },
    Decide {
        #[cfg_attr(feature = "openapi", schema(min_length = 1, max_length = 10000))]
        answer: String,
    },
    Reopen {},
    SetBlocking {
        blocking: bool,
    },
}
impl ProductQuestionAction {
    pub fn domain(
        &self,
    ) -> Result<QuestionAction, specspace_domain::product_question::QuestionError> {
        Ok(match self {
            Self::Propose { answer } => QuestionAction::Propose(QuestionText::new(answer)?),
            Self::Decide { answer } => QuestionAction::Decide(QuestionText::new(answer)?),
            Self::Reopen {} => QuestionAction::Reopen,
            Self::SetBlocking { blocking } => QuestionAction::SetBlocking(*blocking),
        })
    }
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Propose { .. } => "PROPOSE",
            Self::Decide { .. } => "DECIDE",
            Self::Reopen {} => "REOPEN",
            Self::SetBlocking { .. } => "SET_BLOCKING",
        }
    }
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum ProductQuestionPhase {
    Open,
    Proposed,
    Decided,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum ProductQuestionSource {
    Human,
    Imported,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum ProductQuestionKind {
    Open,
    Propose,
    Decide,
    Reopen,
    SetBlocking,
}
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ProductQuestionActor {
    pub id: Uuid,
    pub name: String,
}
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ProductQuestionDecision {
    pub actor: ProductQuestionActor,
    pub recorded_at: String,
}
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ProductQuestionRevision {
    pub id: Uuid,
    pub version: i64,
    pub actor: ProductQuestionActor,
    pub kind: ProductQuestionKind,
    pub state: ProductQuestionPhase,
    pub blocking: bool,
    pub answer: Option<String>,
    pub decision: Option<ProductQuestionDecision>,
    pub recorded_at: String,
}
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ProductQuestionOrigin {
    pub repository_id: Uuid,
    pub branch: String,
    pub head_sha: String,
    pub target_sha: String,
    pub proposal_version: i64,
}
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ProductQuestionPermissions {
    pub can_propose: bool,
    pub can_decide: bool,
    pub can_reopen: bool,
    pub can_toggle_blocking: bool,
}
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ProductQuestion {
    pub id: Uuid,
    pub proposal_id: Uuid,
    pub creator: ProductQuestionActor,
    pub text: String,
    pub source: ProductQuestionSource,
    /// A provenance locator, not proof of a verified import or a Product decision.
    pub source_reference: Option<Uuid>,
    pub origin: ProductQuestionOrigin,
    pub outdated_context: bool,
    pub created_at: String,
    pub current: ProductQuestionRevision,
    pub permissions: ProductQuestionPermissions,
}
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ProductQuestions {
    pub actor_id: Uuid,
    pub proposal_id: Uuid,
    pub proposal_version: i64,
    pub can_create: bool,
    pub unresolved_blockers: i64,
    pub read_at: String,
    pub questions: Vec<ProductQuestion>,
    pub next_after: Option<Uuid>,
}
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ProductQuestionHistory {
    pub actor_id: Uuid,
    pub proposal_id: Uuid,
    pub question: ProductQuestion,
    pub read_at: String,
    pub revisions: Vec<ProductQuestionRevision>,
    pub next_after: Option<i64>,
}
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ProductQuestionRecorded {
    pub id: Uuid,
    pub request_id: Uuid,
    pub actor_id: Uuid,
    pub proposal_id: Uuid,
    pub question_id: Uuid,
    pub version: i64,
    pub kind: ProductQuestionKind,
    pub recorded_at: String,
    /// Observation of the original receipt; current question state is read separately.
    pub replayed: bool,
}
