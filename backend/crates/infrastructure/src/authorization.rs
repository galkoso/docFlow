use async_trait::async_trait;
use specspace_application::book::{BookError, ReadAuthorization};
use specspace_application::proposal_reader::{ProposalReadAuthorization, ProposalReadScope};
use specspace_domain::BranchName;
use specspace_git_adapter::CommitSha;
use sqlx::PgPool;
use uuid::Uuid;

pub struct PgReadAuthorization {
    pool: PgPool,
}

pub struct PgProposalReadAuthorization {
    pool: PgPool,
}
impl PgProposalReadAuthorization {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}
#[async_trait]
impl ProposalReadAuthorization for PgProposalReadAuthorization {
    async fn resolve(&self, actor: Uuid, proposal: Uuid) -> Result<ProposalReadScope, BookError> {
        let row: Option<(Uuid, String, Option<String>)> = sqlx::query_as(
            "SELECT p.repository_id,p.branch,p.head_sha FROM proposals p JOIN repositories r ON r.id=p.repository_id AND r.organization_id=p.organization_id JOIN memberships m ON m.organization_id=p.organization_id JOIN users u ON u.id=m.user_id WHERE p.id=$1 AND u.id=$2 AND u.status='ACTIVE'"
        ).bind(proposal).bind(actor).fetch_optional(&self.pool).await.map_err(|_| BookError::AuthorizationUnavailable)?;
        let (repository, branch, head) = row.ok_or(BookError::NotFound)?;
        Ok(ProposalReadScope {
            repository,
            branch: BranchName::parse(branch)
                .map_err(|_| BookError::InvalidRepositoryConfiguration)?,
            head: CommitSha::parse(head.ok_or(BookError::ProposalNotReady)?)
                .map_err(|_| BookError::InvalidRepositoryConfiguration)?,
        })
    }
}

impl PgReadAuthorization {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl ReadAuthorization for PgReadAuthorization {
    async fn can_read(&self, actor: Uuid, repository: Uuid) -> Result<bool, BookError> {
        // v1 has one organization repository. All its active members may read it.
        // No role, organization or repository scope is accepted from the browser.
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (
              SELECT 1 FROM users u
              JOIN memberships m ON m.user_id = u.id
              JOIN repositories r ON r.organization_id = m.organization_id
              WHERE u.id = $1 AND r.id = $2 AND u.status = 'ACTIVE'
            )",
        )
        .bind(actor)
        .bind(repository)
        .fetch_one(&self.pool)
        .await
        .map_err(|_| BookError::AuthorizationUnavailable)
    }
}
