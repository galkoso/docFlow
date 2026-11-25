use super::*;

#[derive(FromRow)]
pub(super) struct RevisionRow {
    revision_id: Uuid,
    pub version: i64,
    actor_id: Uuid,
    actor_name: String,
    kind: String,
    state: String,
    blocking: bool,
    answer: Option<String>,
    decided_by: Option<Uuid>,
    decider_name: Option<String>,
    pub decided_at: Option<Timestamp>,
    recorded_at: Timestamp,
}
impl RevisionRow {
    pub fn state(&self, creator: Uuid) -> Result<QuestionState, ProductQuestionError> {
        let answer = self
            .answer
            .as_deref()
            .map(QuestionText::new)
            .transpose()
            .map_err(|_| ProductQuestionError::Unavailable)?;
        let phase = match self.state.as_str() {
            "OPEN" => QuestionPhase::Open,
            "PROPOSED" => QuestionPhase::Proposed,
            "DECIDED" => QuestionPhase::Decided,
            _ => return Err(ProductQuestionError::Unavailable),
        };
        let valid = matches!(
            (phase, answer.is_some(), self.decided_by, self.decided_at),
            (QuestionPhase::Open, false, None, None)
                | (QuestionPhase::Proposed, true, None, None)
                | (QuestionPhase::Decided, true, Some(_), Some(_))
        );
        if !valid
            || self.version < 1
            || self.decided_by.is_some_and(|id| id.is_nil())
            || self.actor_id.is_nil()
            || self.revision_id.is_nil()
        {
            return Err(ProductQuestionError::Unavailable);
        }
        Ok(QuestionState {
            creator,
            phase,
            blocking: self.blocking,
            answer,
            decided_by: self.decided_by,
        })
    }
    fn dto(self, creator: Uuid) -> Result<ProductQuestionRevision, ProductQuestionError> {
        let state = self.state(creator)?;
        let decision = if let Some(actor) = self.decided_by {
            Some(ProductQuestionDecision {
                actor: ProductQuestionActor {
                    id: actor,
                    name: self.decider_name.ok_or(ProductQuestionError::Unavailable)?,
                },
                recorded_at: self
                    .decided_at
                    .ok_or(ProductQuestionError::Unavailable)?
                    .to_rfc3339(),
            })
        } else {
            None
        };
        Ok(ProductQuestionRevision {
            id: self.revision_id,
            version: self.version,
            actor: ProductQuestionActor {
                id: self.actor_id,
                name: self.actor_name,
            },
            kind: kind(&self.kind)?,
            state: match state.phase {
                QuestionPhase::Open => ProductQuestionPhase::Open,
                QuestionPhase::Proposed => ProductQuestionPhase::Proposed,
                QuestionPhase::Decided => ProductQuestionPhase::Decided,
            },
            blocking: self.blocking,
            answer: self.answer,
            decision,
            recorded_at: self.recorded_at.to_rfc3339(),
        })
    }
}
#[derive(FromRow)]
pub(super) struct QuestionRow {
    id: Uuid,
    proposal_id: Uuid,
    pub creator_id: Uuid,
    creator_name: String,
    pub text: String,
    source: String,
    source_reference: Option<Uuid>,
    repository_id: Uuid,
    branch: String,
    head_sha: String,
    target_sha: String,
    proposal_version: i64,
    created_at: Timestamp,
    #[sqlx(flatten)]
    pub revision: RevisionRow,
}
impl QuestionRow {
    fn dto(self, actor: Uuid, scope: &Scope) -> Result<ProductQuestion, ProductQuestionError> {
        QuestionText::new(&self.text).map_err(|_| ProductQuestionError::Unavailable)?;
        let current = self.revision.state(self.creator_id)?;
        let permitted = |a| scope.policy(actor, |c| current.transition(c, a)).is_ok();
        let sample = || QuestionText::new("permission probe").expect("bounded constant");
        let permissions = ProductQuestionPermissions {
            can_propose: permitted(QuestionAction::Propose(sample())),
            can_decide: permitted(QuestionAction::Decide(sample())),
            can_reopen: permitted(QuestionAction::Reopen),
            can_toggle_blocking: permitted(QuestionAction::SetBlocking(!current.blocking)),
        };
        let source = match (self.source.as_str(), self.source_reference) {
            ("HUMAN", None) => ProductQuestionSource::Human,
            ("IMPORTED", Some(_)) => ProductQuestionSource::Imported,
            _ => return Err(ProductQuestionError::Unavailable),
        };
        let outdated_context = self.repository_id != scope.repository_id
            || self.branch != scope.branch
            || Some(&self.head_sha) != scope.head_sha.as_ref()
            || Some(&self.target_sha) != scope.target_sha.as_ref();
        Ok(ProductQuestion {
            id: self.id,
            proposal_id: self.proposal_id,
            creator: ProductQuestionActor {
                id: self.creator_id,
                name: self.creator_name,
            },
            text: self.text,
            source,
            source_reference: self.source_reference,
            origin: ProductQuestionOrigin {
                repository_id: self.repository_id,
                branch: self.branch,
                head_sha: self.head_sha,
                target_sha: self.target_sha,
                proposal_version: self.proposal_version,
            },
            outdated_context,
            created_at: self.created_at.to_rfc3339(),
            current: self.revision.dto(self.creator_id)?,
            permissions,
        })
    }
}
const REVISION_COLUMNS: &str = "r.id AS revision_id,r.version,r.actor_id,left(a.display_name,200) AS actor_name,r.kind,r.state,r.blocking,r.answer,r.decided_by,left(d.display_name,200) AS decider_name,r.decided_at,r.created_at AS recorded_at";
fn headers() -> String {
    format!("SELECT q.id,q.proposal_id,q.creator_id,left(c.display_name,200) AS creator_name,r.question_text AS text,q.source,q.source_reference,q.repository_id,q.branch,q.head_sha,q.target_sha,q.proposal_version,q.created_at,{REVISION_COLUMNS}
      FROM product_questions q JOIN users c ON c.id=q.creator_id
      JOIN LATERAL (SELECT * FROM product_question_revisions WHERE organization_id=q.organization_id AND proposal_id=q.proposal_id AND question_id=q.id ORDER BY version DESC LIMIT 1) r ON true
      JOIN users a ON a.id=r.actor_id LEFT JOIN users d ON d.id=r.decided_by WHERE q.organization_id=$1 AND q.proposal_id=$2")
}
pub(super) async fn one(
    conn: &mut PgConnection,
    org: Uuid,
    proposal: Uuid,
    question: Uuid,
) -> Result<QuestionRow, ProductQuestionError> {
    let row = sqlx::query_as(&format!("{} AND q.id=$3", headers()))
        .bind(org)
        .bind(proposal)
        .bind(question)
        .fetch_optional(&mut *conn)
        .await?;
    if let Some(row) = row {
        return Ok(row);
    }
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM product_questions WHERE organization_id=$1 AND proposal_id=$2 AND id=$3)")
        .bind(org).bind(proposal).bind(question).fetch_one(conn).await?;
    Err(if exists {
        ProductQuestionError::Unavailable
    } else {
        ProductQuestionError::NotFound
    })
}
async fn clock(conn: &mut PgConnection) -> Result<String, ProductQuestionError> {
    Ok(
        sqlx::query_scalar::<_, Timestamp>("SELECT clock_timestamp()")
            .fetch_one(conn)
            .await?
            .to_rfc3339(),
    )
}
impl PgProductQuestions {
    pub async fn list(
        &self,
        actor: Uuid,
        proposal: Uuid,
        after: Option<Uuid>,
    ) -> Result<ProductQuestions, ProductQuestionError> {
        if [actor, proposal].iter().any(Uuid::is_nil) || after.is_some_and(|id| id.is_nil()) {
            return Err(ProductQuestionError::Invalid);
        }
        let mut tx = self.transaction(true).await?;
        let scope = scope(&mut tx, actor, proposal, false).await?;
        let unresolved_blockers =
            crate::review_readiness::questions(&mut tx, scope.organization_id, proposal)
                .await?
                .verified_unresolved()?;
        let before = if let Some(after) = after {
            sqlx::query_scalar::<_,i64>("SELECT sequence FROM product_questions WHERE organization_id=$1 AND proposal_id=$2 AND id=$3").bind(scope.organization_id).bind(proposal).bind(after).fetch_optional(&mut *tx).await?.ok_or(ProductQuestionError::Invalid)?
        } else {
            i64::MAX
        };
        let mut rows = sqlx::query_as::<_, QuestionRow>(&format!(
            "{} AND q.sequence<$3 ORDER BY q.sequence DESC LIMIT 21",
            headers()
        ))
        .bind(scope.organization_id)
        .bind(proposal)
        .bind(before)
        .fetch_all(&mut *tx)
        .await?;
        let more = rows.len() > 20;
        rows.truncate(20);
        let next_after = if more {
            rows.last().map(|q| q.id)
        } else {
            None
        };
        let questions = rows
            .into_iter()
            .map(|r| r.dto(actor, &scope))
            .collect::<Result<_, _>>()?;
        let read_at = clock(&mut tx).await?;
        tx.commit().await?;
        Ok(ProductQuestions {
            actor_id: actor,
            proposal_id: proposal,
            proposal_version: scope.version,
            can_create: scope.can_create(actor),
            unresolved_blockers,
            read_at,
            questions,
            next_after,
        })
    }
    pub async fn history(
        &self,
        actor: Uuid,
        proposal: Uuid,
        question: Uuid,
        after: Option<i64>,
    ) -> Result<ProductQuestionHistory, ProductQuestionError> {
        if [actor, proposal, question].iter().any(Uuid::is_nil) || after.is_some_and(|v| v < 1) {
            return Err(ProductQuestionError::Invalid);
        }
        let mut tx = self.transaction(true).await?;
        let scope = scope(&mut tx, actor, proposal, false).await?;
        let q = one(&mut tx, scope.organization_id, proposal, question).await?;
        let creator = q.creator_id;
        if after.is_some_and(|v| v > q.revision.version) {
            return Err(ProductQuestionError::Invalid);
        }
        let mut rows = sqlx::query_as::<_, RevisionRow>(&format!("SELECT {REVISION_COLUMNS} FROM product_question_revisions r JOIN users a ON a.id=r.actor_id LEFT JOIN users d ON d.id=r.decided_by WHERE r.organization_id=$1 AND r.proposal_id=$2 AND r.question_id=$3 AND r.version>$4 ORDER BY r.version LIMIT 21"))
            .bind(scope.organization_id).bind(proposal).bind(question).bind(after.unwrap_or(0)).fetch_all(&mut *tx).await?;
        let more = rows.len() > 20;
        rows.truncate(20);
        let next_after = if more {
            rows.last().map(|r| r.version)
        } else {
            None
        };
        let revisions = rows
            .into_iter()
            .map(|r| r.dto(creator))
            .collect::<Result<_, _>>()?;
        let question = q.dto(actor, &scope)?;
        let read_at = clock(&mut tx).await?;
        tx.commit().await?;
        Ok(ProductQuestionHistory {
            actor_id: actor,
            proposal_id: proposal,
            question,
            read_at,
            revisions,
            next_after,
        })
    }
}
