//! Short-lived number/source previews, scoped to the current human and repository.
//! Git reads happen before allocation. Consuming a context is atomic with CREATE.
use crate::proposals::PgProposals;
use specspace_application::proposal::{IdempotencyKey, ProposalError};
use specspace_git_adapter::CommitSha;
use sqlx::{FromRow, PgConnection, PgPool};
use uuid::Uuid;

#[derive(FromRow)]
pub struct CreationContext {
    pub context_id: Uuid,
    pub repository_id: Uuid,
    pub number: i64,
    pub source_sha: String,
    pub expires_at: String,
    valid: bool,
    consumed: bool,
}
impl CreationContext {
    fn available(self) -> Result<Self, ProposalError> {
        if self.consumed {
            Err(ProposalError::ContextUsed)
        } else if !self.valid {
            Err(ProposalError::ContextExpired)
        } else {
            Ok(self)
        }
    }
}
const READ: &str = r#"SELECT c.id AS context_id,c.repository_id,c.number,c.source_sha,to_char(c.expires_at AT TIME ZONE 'UTC','YYYY-MM-DD"T"HH24:MI:SS.US"Z"') AS expires_at,c.expires_at>clock_timestamp() AS valid,EXISTS(SELECT 1 FROM proposals p WHERE p.creation_context_id=c.id) AS consumed FROM proposal_creation_contexts c WHERE c.actor_id=$1 AND c.organization_id=$2 AND c.idempotency_key=$3"#;
pub struct PgCreationContexts {
    pool: PgPool,
}
impl PgCreationContexts {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
    pub async fn existing(
        &self,
        actor: Uuid,
        repository: Uuid,
        key: &IdempotencyKey,
    ) -> Result<Option<CreationContext>, ProposalError> {
        let organization = PgProposals::new(self.pool.clone())
            .authorize_create(actor, repository)
            .await?;
        let row = sqlx::query_as::<_, CreationContext>(READ)
            .bind(actor)
            .bind(organization)
            .bind(key.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(unavailable)?;
        PgProposals::new(self.pool.clone())
            .authorize_create(actor, repository)
            .await?;
        row.map(|row| {
            if row.repository_id != repository {
                Err(ProposalError::IdempotencyMismatch)
            } else {
                row.available()
            }
        })
        .transpose()
    }
    pub async fn allocate(
        &self,
        actor: Uuid,
        repository: Uuid,
        source: &CommitSha,
        key: &IdempotencyKey,
    ) -> Result<CreationContext, ProposalError> {
        let store = PgProposals::new(self.pool.clone());
        store.authorize_create(actor, repository).await?;
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        let binding:Option<(Uuid,String,String)>=sqlx::query_as("SELECT r.organization_id,r.remote_id,r.installation_id FROM repositories r JOIN memberships m ON m.organization_id=r.organization_id JOIN users u ON u.id=m.user_id WHERE r.id=$1 AND m.user_id=$2 AND u.status='ACTIVE' AND m.role<>'READER' AND r.provider='GITHUB' AND r.default_branch='main' FOR SHARE OF r,m,u")
            .bind(repository).bind(actor).fetch_optional(&mut *tx).await.map_err(unavailable)?;
        let (organization, remote, installation) = binding.ok_or(ProposalError::Forbidden)?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!("context:{organization}:{actor}:{}", key.as_str()))
            .execute(&mut *tx)
            .await
            .map_err(unavailable)?;
        let existing = sqlx::query_as::<_, CreationContext>(READ)
            .bind(actor)
            .bind(organization)
            .bind(key.as_str())
            .fetch_optional(&mut *tx)
            .await
            .map_err(unavailable)?;
        if let Some(row) = existing {
            if row.repository_id != repository {
                return Err(ProposalError::IdempotencyMismatch);
            }
            let row = row.available()?;
            tx.commit().await.map_err(unavailable)?;
            return Ok(row);
        }
        sqlx::query("INSERT INTO proposal_creation_contexts(id,organization_id,repository_id,actor_id,idempotency_key,source_sha,remote_id,installation_id,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,clock_timestamp()+interval '30 minutes')")
            .bind(Uuid::new_v4()).bind(organization).bind(repository).bind(actor).bind(key.as_str()).bind(source.as_str()).bind(remote).bind(installation).execute(&mut *tx).await.map_err(unavailable)?;
        let row = sqlx::query_as::<_, CreationContext>(READ)
            .bind(actor)
            .bind(organization)
            .bind(key.as_str())
            .fetch_one(&mut *tx)
            .await
            .map_err(unavailable)?;
        tx.commit().await.map_err(unavailable)?;
        Ok(row)
    }
}

pub(crate) async fn lock_number(
    connection: &mut PgConnection,
    id: Uuid,
    actor: Uuid,
    repository: Uuid,
    source: &CommitSha,
    remote: &str,
    installation: &str,
) -> Result<i64, ProposalError> {
    // Lock first, then inspect consumption in a fresh READ COMMITTED statement;
    // a waiter must see a proposal inserted by the previous context holder.
    let found:Option<Uuid>=sqlx::query_scalar("SELECT id FROM proposal_creation_contexts WHERE id=$1 AND actor_id=$2 AND repository_id=$3 FOR UPDATE")
        .bind(id).bind(actor).bind(repository).fetch_optional(&mut *connection).await.map_err(unavailable)?;
    if found.is_none() {
        return Err(ProposalError::NotFound);
    }
    let (number,valid,recorded,original_remote,original_installation,consumed):(i64,bool,String,String,String,bool)=sqlx::query_as("SELECT number,expires_at>clock_timestamp(),source_sha,remote_id,installation_id,EXISTS(SELECT 1 FROM proposals p WHERE p.creation_context_id=c.id) FROM proposal_creation_contexts c WHERE id=$1")
        .bind(id).fetch_one(connection).await.map_err(unavailable)?;
    if consumed {
        return Err(ProposalError::ContextUsed);
    }
    if !valid {
        return Err(ProposalError::ContextExpired);
    }
    if recorded != source.as_str()
        || original_remote != remote
        || original_installation != installation
    {
        return Err(ProposalError::ContextMismatch);
    }
    Ok(number)
}
fn unavailable(_: sqlx::Error) -> ProposalError {
    ProposalError::Unavailable
}
