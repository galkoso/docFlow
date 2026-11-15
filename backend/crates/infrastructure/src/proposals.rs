//! Proposal reservation is atomic in PostgreSQL. Git provisioning runs later,
//! outside this transaction, against the recorded source and operation journal.
use serde::Serialize;
use specspace_application::proposal::{
    FieldError, IdempotencyKey, ProposalError, ValidatedProposal,
};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

const ACCESS: &str = "SELECT r.organization_id,r.remote_id,r.installation_id,m.role FROM repositories r JOIN memberships m ON m.organization_id=r.organization_id JOIN users u ON u.id=m.user_id WHERE r.id=$1 AND m.user_id=$2 AND u.status='ACTIVE'";
const EXISTING: &str = "SELECT o.request_hash,p.id AS proposal_id,p.number,o.id AS operation_id,j.id AS job_id FROM git_operations o JOIN proposals p ON p.id=o.proposal_id AND p.organization_id=o.organization_id JOIN jobs j ON j.operation_id=o.id AND j.organization_id=o.organization_id AND j.kind='CREATE_PROPOSAL' WHERE o.organization_id=$1 AND o.actor_id=$2 AND o.kind='CREATE' AND o.idempotency_key=$3";

#[derive(FromRow)]
struct Scope {
    organization_id: Uuid,
    role: String,
    remote_id: String,
    installation_id: String,
}
impl Scope {
    fn require_create(&self) -> Result<Uuid, ProposalError> {
        if self.role == "READER" {
            Err(ProposalError::Forbidden)
        } else {
            Ok(self.organization_id)
        }
    }
}
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CreationTicket {
    pub proposal_id: Uuid,
    pub number: i64,
    pub operation_id: Uuid,
    pub job_id: Uuid,
}
#[derive(FromRow)]
struct Existing {
    request_hash: String,
    proposal_id: Uuid,
    number: i64,
    operation_id: Uuid,
    job_id: Uuid,
}
impl Existing {
    fn ticket(self, intent: &ValidatedProposal) -> Result<CreationTicket, ProposalError> {
        if self.request_hash != intent.fingerprint() {
            return Err(ProposalError::IdempotencyMismatch);
        }
        Ok(CreationTicket {
            proposal_id: self.proposal_id,
            number: self.number,
            operation_id: self.operation_id,
            job_id: self.job_id,
        })
    }
}

pub struct PgProposals {
    pool: PgPool,
}
impl PgProposals {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
    pub async fn authorize_create(
        &self,
        actor: Uuid,
        repository: Uuid,
    ) -> Result<Uuid, ProposalError> {
        sqlx::query_as::<_, Scope>(ACCESS)
            .bind(repository)
            .bind(actor)
            .fetch_optional(&self.pool)
            .await
            .map_err(unavailable)?
            .ok_or(ProposalError::NotFound)?
            .require_create()
    }
    /// Check replay before consulting mutable Git refs. A retry with the same
    /// intent returns its original ticket even if main has since advanced.
    pub async fn existing(
        &self,
        actor: Uuid,
        intent: &ValidatedProposal,
        key: &IdempotencyKey,
    ) -> Result<Option<CreationTicket>, ProposalError> {
        let organization = self
            .authorize_create(actor, intent.request().repository_id)
            .await?;
        let existing = sqlx::query_as::<_, Existing>(EXISTING)
            .bind(organization)
            .bind(actor)
            .bind(key.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(unavailable)?;
        self.authorize_create(actor, intent.request().repository_id)
            .await?;
        existing.map(|existing| existing.ticket(intent)).transpose()
    }

    pub async fn check_context(
        &self,
        actor: Uuid,
        intent: &ValidatedProposal,
    ) -> Result<(), ProposalError> {
        let Some(id) = intent.request().creation_context_id else {
            return Ok(());
        };
        self.authorize_create(actor, intent.request().repository_id)
            .await?;
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        let scope = sqlx::query_as::<_, Scope>(ACCESS)
            .bind(intent.request().repository_id)
            .bind(actor)
            .fetch_optional(&mut *tx)
            .await
            .map_err(unavailable)?
            .ok_or(ProposalError::NotFound)?;
        scope.require_create()?;
        crate::creation_context::lock_number(
            &mut tx,
            id,
            actor,
            intent.request().repository_id,
            intent.source(),
            &scope.remote_id,
            &scope.installation_id,
        )
        .await?;
        tx.commit().await.map_err(unavailable)?;
        Ok(())
    }

    /// The caller proves the displayed source SHA is still current before this
    /// reservation. The worker must check it again before creating the Git ref.
    pub async fn reserve(
        &self,
        actor: Uuid,
        intent: &ValidatedProposal,
        key: &IdempotencyKey,
    ) -> Result<CreationTicket, ProposalError> {
        let request = intent.request();
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        // Hold current membership/user eligibility only for this short DB work.
        // No provider call may run while these locks are held.
        let access = format!("{ACCESS} FOR SHARE OF r,m,u");
        let scope = sqlx::query_as::<_, Scope>(&access)
            .bind(request.repository_id)
            .bind(actor)
            .fetch_optional(&mut *tx)
            .await
            .map_err(unavailable)?
            .ok_or(ProposalError::NotFound)?;
        let organization = scope.require_create()?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!("create:{organization}:{actor}:{}", key.as_str()))
            .execute(&mut *tx)
            .await
            .map_err(unavailable)?;
        if let Some(existing) = sqlx::query_as::<_, Existing>(EXISTING)
            .bind(organization)
            .bind(actor)
            .bind(key.as_str())
            .fetch_optional(&mut *tx)
            .await
            .map_err(unavailable)?
        {
            let ticket = existing.ticket(intent)?;
            tx.commit().await.map_err(unavailable)?;
            return Ok(ticket);
        }
        let team = sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM teams WHERE id=$1 AND organization_id=$2 FOR SHARE",
        )
        .bind(request.team_id)
        .bind(organization)
        .fetch_optional(&mut *tx)
        .await
        .map_err(unavailable)?;
        if team.is_none() {
            return Err(invalid("team_id", "TEAM_NOT_AVAILABLE"));
        }
        let owner = request.owner_id.ok_or(ProposalError::Unavailable)?;
        let eligible = sqlx::query_scalar::<_, Uuid>("SELECT u.id FROM users u JOIN memberships m ON m.user_id=u.id WHERE u.id=$1 AND m.organization_id=$2 AND u.status='ACTIVE' AND m.role IN ('CONTRIBUTOR','REVIEWER','MAINTAINER','ADMIN') FOR SHARE OF u,m")
            .bind(owner).bind(organization).fetch_optional(&mut *tx).await.map_err(unavailable)?;
        if eligible.is_none() {
            return Err(invalid("owner_id", "OWNER_NOT_AVAILABLE"));
        }
        let reserved_number = if let Some(id) = request.creation_context_id {
            Some(
                crate::creation_context::lock_number(
                    &mut tx,
                    id,
                    actor,
                    request.repository_id,
                    intent.source(),
                    &scope.remote_id,
                    &scope.installation_id,
                )
                .await?,
            )
        } else {
            None
        };
        let proposal_id = Uuid::new_v4();
        let number: i64 = sqlx::query_scalar("INSERT INTO proposals(id,organization_id,repository_id,title,branch,owner_id,author_id,team_id,description,ui_impact,ui_impact_reason,initial_mode,links,number,creation_context_id) OVERRIDING SYSTEM VALUE VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,coalesce($14::bigint,nextval('proposals_number_seq')),$15) RETURNING number")
            .bind(proposal_id).bind(organization).bind(request.repository_id).bind(&request.title).bind(intent.branch().as_str())
            .bind(owner).bind(actor).bind(request.team_id).bind(&request.description).bind(request.ui_impact.as_str()).bind(&request.ui_impact_reason)
            .bind(request.initial_mode.as_str()).bind(serde_json::to_value(&request.links).map_err(|_| ProposalError::Unavailable)?).bind(reserved_number).bind(request.creation_context_id)
            .fetch_one(&mut *tx).await.map_err(reservation_error)?;
        let operation_id = Uuid::new_v4();
        sqlx::query("INSERT INTO git_operations(id,organization_id,proposal_id,actor_id,kind,idempotency_key,request_hash,expected_sha) VALUES($1,$2,$3,$4,'CREATE',$5,$6,$7)")
            .bind(operation_id).bind(organization).bind(proposal_id).bind(actor).bind(key.as_str()).bind(intent.fingerprint()).bind(intent.source().as_str())
            .execute(&mut *tx).await.map_err(unavailable)?;
        let job_id = Uuid::new_v4();
        sqlx::query("INSERT INTO jobs(id,organization_id,operation_id,kind,payload) VALUES($1,$2,$3,'CREATE_PROPOSAL',$4)")
            .bind(job_id).bind(organization).bind(operation_id)
            .bind(serde_json::json!({"proposal_id":proposal_id,"repository_id":request.repository_id,"expected_source_sha":intent.source().as_str(),"remote_id":scope.remote_id,"installation_id":scope.installation_id}))
            .execute(&mut *tx).await.map_err(unavailable)?;
        sqlx::query("INSERT INTO audit_events(id,organization_id,actor_id,action,entity_id,after_ref,operation_id,result) VALUES($1,$2,$3,'PROPOSAL_PROVISIONING',$4,$5,$6,'PENDING')")
            .bind(Uuid::new_v4()).bind(organization).bind(actor).bind(proposal_id).bind(intent.source().as_str()).bind(operation_id)
            .execute(&mut *tx).await.map_err(unavailable)?;
        sqlx::query("INSERT INTO outbox(id,organization_id,event_type,entity_id,payload) VALUES($1,$2,'PROPOSAL_PROVISIONING',$3,$4)")
            .bind(Uuid::new_v4()).bind(organization).bind(proposal_id).bind(serde_json::json!({"job_id":job_id,"operation_id":operation_id}))
            .execute(&mut *tx).await.map_err(unavailable)?;
        tx.commit().await.map_err(unavailable)?;
        Ok(CreationTicket {
            proposal_id,
            number,
            operation_id,
            job_id,
        })
    }
}
fn unavailable(_: sqlx::Error) -> ProposalError {
    ProposalError::Unavailable
}
fn invalid(field: &'static str, code: &'static str) -> ProposalError {
    ProposalError::Validation(vec![FieldError { field, code }])
}
fn reservation_error(error: sqlx::Error) -> ProposalError {
    if error.as_database_error().is_some_and(|error| {
        error.is_unique_violation()
            && error.constraint() == Some("proposals_repository_id_branch_key_key")
    }) {
        ProposalError::BranchTaken
    } else if error.as_database_error().is_some_and(|error| {
        error.is_unique_violation()
            && error.constraint() == Some("proposals_creation_context_id_key")
    }) {
        ProposalError::ContextUsed
    } else {
        ProposalError::Unavailable
    }
}
