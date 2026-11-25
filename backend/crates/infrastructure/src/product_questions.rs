//! Current authority, immutable question revisions and exact receipt replay.
//! All mutation work stays in one transaction; there is no provider IO.
use serde::Serialize;
use sha2::{Digest, Sha256};
use specspace_application::product_question::*;
use specspace_domain::{
    policy::{self, Membership, ProposalState, ResourceAccess, ReviewDomain, Role},
    product_question::*,
};
use sqlx::{FromRow, PgConnection, PgPool, Postgres, Transaction};
use uuid::Uuid;
mod read;
type Timestamp = sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>;

#[derive(Debug, PartialEq, Eq)]
pub enum ProductQuestionError {
    Invalid,
    NotFound,
    Forbidden,
    ProductRequired,
    NotEditable,
    MergeInProgress,
    Conflict,
    Stale,
    IdempotencyMismatch,
    Quota,
    Unavailable,
}
impl From<sqlx::Error> for ProductQuestionError {
    fn from(e: sqlx::Error) -> Self {
        if e.as_database_error()
            .is_some_and(|e| e.message() == "product question quota exceeded")
        {
            Self::Quota
        } else {
            Self::Unavailable
        }
    }
}
impl From<QuestionError> for ProductQuestionError {
    fn from(e: QuestionError) -> Self {
        match e {
            QuestionError::Forbidden => Self::Forbidden,
            QuestionError::ProductRequired => Self::ProductRequired,
            QuestionError::NotEditable => Self::NotEditable,
            QuestionError::MergeInProgress => Self::MergeInProgress,
            QuestionError::InvalidState => Self::Conflict,
            QuestionError::InvalidText => Self::Invalid,
        }
    }
}
#[derive(FromRow)]
struct Scope {
    organization_id: Uuid,
    repository_id: Uuid,
    branch: String,
    head_sha: Option<String>,
    target_sha: Option<String>,
    version: i64,
    state: String,
    role: String,
    review_domains: Vec<String>,
}
impl Scope {
    fn member(&self, actor: Uuid) -> Result<Membership, ProductQuestionError> {
        let role = match self.role.as_str() {
            "READER" => Role::Reader,
            "CONTRIBUTOR" => Role::Contributor,
            "REVIEWER" => Role::Reviewer,
            "MAINTAINER" => Role::Maintainer,
            "ADMIN" => Role::Admin,
            _ => return Err(ProductQuestionError::Unavailable),
        };
        let review_domains = self
            .review_domains
            .iter()
            .map(|v| match v.as_str() {
                "PRODUCT" => Ok(ReviewDomain::Product),
                "DEVELOPMENT" => Ok(ReviewDomain::Development),
                "DESIGN" => Ok(ReviewDomain::Design),
                _ => Err(ProductQuestionError::Unavailable),
            })
            .collect::<Result<_, _>>()?;
        Ok(Membership {
            organization_id: self.organization_id,
            user_id: actor,
            active: true,
            role,
            review_domains,
        })
    }
    fn resource(&self) -> ResourceAccess {
        ResourceAccess {
            organization_id: self.organization_id,
            repository_access: true,
            assigned_editor: false,
            export_allowed: false,
        }
    }
    fn policy<T>(
        &self,
        actor: Uuid,
        apply: impl FnOnce(&QuestionContext<'_>) -> Result<T, QuestionError>,
    ) -> Result<T, ProductQuestionError> {
        let state = match self.state.as_str() {
            "PROVISIONING" => ProposalState::Provisioning,
            "DRAFT" => ProposalState::Draft,
            "IN_REVIEW" => ProposalState::InReview,
            "CHANGES_REQUESTED" => ProposalState::ChangesRequested,
            "APPROVED" => ProposalState::Approved,
            "MERGING" => ProposalState::Merging,
            "MERGED" => ProposalState::Merged,
            "CLOSED" => ProposalState::Closed,
            _ => return Err(ProductQuestionError::Unavailable),
        };
        Ok(apply(&QuestionContext {
            member: &self.member(actor)?,
            resource: &self.resource(),
            proposal_state: state,
        })?)
    }
    fn authorize_intent(
        &self,
        actor: Uuid,
        creator: Uuid,
        action: Option<&ProductQuestionAction>,
    ) -> Result<(), ProductQuestionError> {
        let m = self.member(actor)?;
        if !policy::authorized(&m, &self.resource(), policy::Action::Comment) {
            return Err(ProductQuestionError::Forbidden);
        }
        let product = policy::authorized(&m, &self.resource(), policy::Action::Review)
            && m.review_domains.contains(&ReviewDomain::Product);
        match action {
            Some(
                ProductQuestionAction::Decide { .. }
                | ProductQuestionAction::SetBlocking { blocking: false },
            ) if !product => Err(ProductQuestionError::ProductRequired),
            Some(
                ProductQuestionAction::Reopen {}
                | ProductQuestionAction::SetBlocking { blocking: true },
            ) if !product && actor != creator => Err(ProductQuestionError::Forbidden),
            _ => Ok(()),
        }
    }
    fn can_create(&self, actor: Uuid) -> bool {
        self.head_sha.is_some()
            && self.target_sha.is_some()
            && self
                .policy(actor, |c| QuestionState::open(c, false, None))
                .is_ok()
    }
}
async fn scope(
    conn: &mut PgConnection,
    actor: Uuid,
    proposal: Uuid,
    write: bool,
) -> Result<Scope, ProductQuestionError> {
    let lock = if write {
        "FOR UPDATE OF p FOR SHARE OF r,o,m,u"
    } else {
        "FOR SHARE OF p,r,o,m,u"
    };
    let query = format!(
        "SELECT p.organization_id,p.repository_id,p.branch,p.head_sha,p.target_sha,p.version,p.state,m.role,m.review_domains FROM proposals p JOIN repositories r ON r.id=p.repository_id AND r.organization_id=p.organization_id JOIN organizations o ON o.id=p.organization_id JOIN memberships m ON m.organization_id=p.organization_id JOIN users u ON u.id=m.user_id WHERE p.id=$1 AND u.id=$2 AND u.status='ACTIVE' {lock}"
    );
    sqlx::query_as(&query)
        .bind(proposal)
        .bind(actor)
        .fetch_optional(conn)
        .await?
        .ok_or(ProductQuestionError::NotFound)
}
fn kind(value: &str) -> Result<ProductQuestionKind, ProductQuestionError> {
    Ok(match value {
        "OPEN" => ProductQuestionKind::Open,
        "PROPOSE" => ProductQuestionKind::Propose,
        "DECIDE" => ProductQuestionKind::Decide,
        "REOPEN" => ProductQuestionKind::Reopen,
        "SET_BLOCKING" => ProductQuestionKind::SetBlocking,
        _ => return Err(ProductQuestionError::Unavailable),
    })
}
#[derive(FromRow)]
struct Recorded {
    id: Uuid,
    request_id: Uuid,
    actor_id: Uuid,
    proposal_id: Uuid,
    question_id: Uuid,
    version: i64,
    kind: String,
    created_at: Timestamp,
    request_hash: String,
}
impl Recorded {
    fn dto(self, replayed: bool) -> Result<ProductQuestionRecorded, ProductQuestionError> {
        Ok(ProductQuestionRecorded {
            id: self.id,
            request_id: self.request_id,
            actor_id: self.actor_id,
            proposal_id: self.proposal_id,
            question_id: self.question_id,
            version: self.version,
            kind: kind(&self.kind)?,
            recorded_at: self.created_at.to_rfc3339(),
            replayed,
        })
    }
}
struct Intent<'a> {
    actor: Uuid,
    proposal: Uuid,
    question: Option<Uuid>,
    request: Uuid,
    hash: &'a str,
}
async fn replay(
    conn: &mut PgConnection,
    org: Uuid,
    intent: &Intent<'_>,
) -> Result<Option<ProductQuestionRecorded>, ProductQuestionError> {
    if let Some(r) = sqlx::query_as::<_, Recorded>(
        "SELECT * FROM product_question_revisions WHERE organization_id=$1 AND request_id=$2",
    )
    .bind(org)
    .bind(intent.request)
    .fetch_optional(conn)
    .await?
    {
        if r.actor_id != intent.actor
            || r.proposal_id != intent.proposal
            || intent.question.is_some_and(|q| q != r.question_id)
            || r.request_hash != intent.hash
        {
            return Err(ProductQuestionError::IdempotencyMismatch);
        }
        return Ok(Some(r.dto(true)?));
    }
    Ok(None)
}
fn hash<T: Serialize>(
    tag: &str,
    actor: Uuid,
    proposal: Uuid,
    question: Option<Uuid>,
    body: &T,
) -> Result<String, ProductQuestionError> {
    Ok(format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&(tag, actor, proposal, question, body))
                .map_err(|_| ProductQuestionError::Invalid)?
        )
    ))
}
struct RevisionInput<'a> {
    id: Uuid,
    intent: Intent<'a>,
    question: Uuid,
    version: i64,
    kind: &'a str,
    text: &'a str,
    state: &'a QuestionState,
    decided_at: Option<Timestamp>,
}
async fn insert(
    conn: &mut PgConnection,
    s: &Scope,
    r: RevisionInput<'_>,
) -> Result<ProductQuestionRecorded, ProductQuestionError> {
    let (id, audit, event) = (r.id, Uuid::new_v4(), Uuid::new_v4());
    let phase = match r.state.phase {
        QuestionPhase::Open => "OPEN",
        QuestionPhase::Proposed => "PROPOSED",
        QuestionPhase::Decided => "DECIDED",
    };
    let saved = sqlx::query_as::<_, Recorded>("INSERT INTO product_question_revisions(id,organization_id,proposal_id,question_id,version,actor_id,request_id,request_hash,kind,question_text,blocking,state,answer,decided_by,decided_at,audit_id,event_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17) RETURNING *")
        .bind(id).bind(s.organization_id).bind(r.intent.proposal).bind(r.question).bind(r.version).bind(r.intent.actor).bind(r.intent.request).bind(r.intent.hash).bind(r.kind).bind(r.text)
        .bind(r.state.blocking).bind(phase).bind(r.state.answer.as_ref().map(QuestionText::as_str)).bind(r.state.decided_by).bind(r.decided_at).bind(audit).bind(event).fetch_one(&mut *conn).await?;
    sqlx::query("INSERT INTO audit_events(id,organization_id,actor_id,action,entity_id,after_ref,result) VALUES($1,$2,$3,'PRODUCT_QUESTION_CHANGED',$4,$5,$6)")
        .bind(audit).bind(s.organization_id).bind(r.intent.actor).bind(r.question).bind(id.to_string()).bind(r.kind).execute(&mut *conn).await?;
    sqlx::query("INSERT INTO outbox(id,organization_id,event_type,entity_id,payload) VALUES($1,$2,'PRODUCT_QUESTION_CHANGED',$3,$4)")
        .bind(event).bind(s.organization_id).bind(r.intent.proposal).bind(serde_json::json!({"question_id":r.question,"revision_id":id,"version":r.version,"kind":r.kind})).execute(conn).await?;
    saved.dto(false)
}
pub struct PgProductQuestions {
    pool: PgPool,
}
impl PgProductQuestions {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
    async fn transaction(
        &self,
        read: bool,
    ) -> Result<Transaction<'_, Postgres>, ProductQuestionError> {
        let mut tx = self.pool.begin().await?;
        if read {
            sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
                .execute(&mut *tx)
                .await?;
        }
        sqlx::query("SET LOCAL lock_timeout='5s'")
            .execute(&mut *tx)
            .await?;
        sqlx::query("SET LOCAL statement_timeout='10s'")
            .execute(&mut *tx)
            .await?;
        Ok(tx)
    }
    pub async fn create(
        &self,
        actor: Uuid,
        proposal: Uuid,
        request: CreateProductQuestion,
    ) -> Result<ProductQuestionRecorded, ProductQuestionError> {
        request
            .validate()
            .map_err(|_| ProductQuestionError::Invalid)?;
        if actor.is_nil() || proposal.is_nil() {
            return Err(ProductQuestionError::Invalid);
        }
        let hash = hash(
            "product-question-create-v1",
            actor,
            proposal,
            None,
            &request,
        )?;
        let intent = Intent {
            actor,
            proposal,
            question: None,
            request: request.request_id,
            hash: &hash,
        };
        let mut tx = self.transaction(false).await?;
        let s = scope(&mut tx, actor, proposal, true).await?;
        s.authorize_intent(actor, actor, None)?;
        if let Some(old) = replay(&mut tx, s.organization_id, &intent).await? {
            tx.commit().await?;
            return Ok(old);
        }
        if s.version != request.expected_proposal_version {
            return Err(ProductQuestionError::Stale);
        }
        let state = s.policy(actor, |c| {
            QuestionState::open(
                c,
                request.blocking,
                request
                    .proposed_answer
                    .as_deref()
                    .map(QuestionText::new)
                    .transpose()?,
            )
        })?;
        let question = Uuid::new_v4();
        let opening = Uuid::new_v4();
        sqlx::query("INSERT INTO product_questions(id,organization_id,proposal_id,creator_id,source,repository_id,branch,head_sha,target_sha,proposal_version,opening_revision) VALUES($1,$2,$3,$4,'HUMAN',$5,$6,$7,$8,$9,$10)")
            .bind(question).bind(s.organization_id).bind(proposal).bind(actor).bind(s.repository_id).bind(&s.branch).bind(s.head_sha.as_ref().ok_or(ProductQuestionError::NotEditable)?).bind(s.target_sha.as_ref().ok_or(ProductQuestionError::NotEditable)?).bind(s.version).bind(opening).execute(&mut *tx).await?;
        // Opening IDs are sealed in the header; use the same ID for the revision.
        let receipt = insert(
            &mut tx,
            &s,
            RevisionInput {
                id: opening,
                intent,
                question,
                version: 1,
                kind: "OPEN",
                text: &request.text,
                state: &state,
                decided_at: None,
            },
        )
        .await?;
        tx.commit().await?;
        Ok(receipt)
    }
    pub async fn change(
        &self,
        actor: Uuid,
        proposal: Uuid,
        question: Uuid,
        request: ChangeProductQuestion,
    ) -> Result<ProductQuestionRecorded, ProductQuestionError> {
        request
            .validate()
            .map_err(|_| ProductQuestionError::Invalid)?;
        if [actor, proposal, question].iter().any(Uuid::is_nil) {
            return Err(ProductQuestionError::Invalid);
        }
        let hash = hash(
            "product-question-change-v1",
            actor,
            proposal,
            Some(question),
            &request,
        )?;
        let intent = Intent {
            actor,
            proposal,
            question: Some(question),
            request: request.request_id,
            hash: &hash,
        };
        let mut tx = self.transaction(false).await?;
        let s = scope(&mut tx, actor, proposal, true).await?;
        let q = read::one(&mut tx, s.organization_id, proposal, question).await?;
        s.authorize_intent(actor, q.creator_id, Some(&request.action))?;
        if let Some(old) = replay(&mut tx, s.organization_id, &intent).await? {
            tx.commit().await?;
            return Ok(old);
        }
        if q.revision.version != request.expected_version {
            return Err(ProductQuestionError::Stale);
        }
        let current = q.revision.state(q.creator_id)?;
        let next = s.policy(actor, |c| current.transition(c, request.action.domain()?))?;
        let receipt = insert(
            &mut tx,
            &s,
            RevisionInput {
                id: Uuid::new_v4(),
                intent,
                question,
                version: q
                    .revision
                    .version
                    .checked_add(1)
                    .ok_or(ProductQuestionError::Unavailable)?,
                kind: request.action.kind(),
                text: &q.text,
                state: &next,
                decided_at: if matches!(request.action, ProductQuestionAction::SetBlocking { .. }) {
                    q.revision.decided_at
                } else {
                    None
                },
            },
        )
        .await?;
        tx.commit().await?;
        Ok(receipt)
    }
}
