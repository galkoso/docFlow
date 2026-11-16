use async_trait::async_trait;
use specspace_application::comparison_reader::{
    ComparisonAuthorization, ComparisonReadError, ReadContext, RepositoryBinding,
};
use specspace_domain::BranchName;
use specspace_git_adapter::CommitSha;
use sqlx::{FromRow, PgConnection, PgPool};
use uuid::Uuid;
pub struct PgComparisonAuthorization {
    pool: PgPool,
}
impl PgComparisonAuthorization {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}
#[derive(FromRow)]
struct Row {
    organization_id: Uuid,
    repository_id: Uuid,
    branch: String,
    head_sha: Option<String>,
    base_sha: Option<String>,
    version: i64,
    state: String,
    remote_id: String,
    installation_id: String,
    default_branch: String,
}
const CONTEXT_SQL: &str = "SELECT p.organization_id,p.repository_id,p.branch,p.head_sha,p.base_sha,p.version,p.state,r.remote_id,r.installation_id,r.default_branch FROM proposals p JOIN repositories r ON r.id=p.repository_id AND r.organization_id=p.organization_id AND r.provider='GITHUB' JOIN memberships m ON m.organization_id=p.organization_id JOIN users u ON u.id=m.user_id WHERE p.id=$1 AND u.id=$2 AND u.status='ACTIVE'";
#[async_trait]
impl ComparisonAuthorization for PgComparisonAuthorization {
    async fn resolve(
        &self,
        actor: Uuid,
        proposal: Uuid,
    ) -> Result<ReadContext, ComparisonReadError> {
        let mut connection = self.pool.acquire().await.map_err(|_| {
            ComparisonReadError::Source(
                specspace_application::book::BookError::AuthorizationUnavailable,
            )
        })?;
        context_in(&mut connection, actor, proposal, false).await
    }
}
pub(crate) async fn context_in(
    connection: &mut PgConnection,
    actor: Uuid,
    proposal: Uuid,
    locked: bool,
) -> Result<ReadContext, ComparisonReadError> {
    let query = if locked {
        format!("{CONTEXT_SQL} FOR SHARE OF p,r,m,u")
    } else {
        CONTEXT_SQL.into()
    };
    let row = sqlx::query_as::<_, Row>(&query)
        .bind(proposal)
        .bind(actor)
        .fetch_optional(connection)
        .await
        .map_err(|_| {
            ComparisonReadError::Source(
                specspace_application::book::BookError::AuthorizationUnavailable,
            )
        })?
        .ok_or(ComparisonReadError::NotFound)?;
    if row.state == "PROVISIONING" {
        return Err(ComparisonReadError::ProposalNotReady);
    }
    if row.default_branch != "main" {
        return Err(ComparisonReadError::InvalidConfiguration);
    }
    let number = |text: &str| {
        text.parse::<u64>()
            .ok()
            .filter(|n| *n > 0)
            .ok_or(ComparisonReadError::InvalidConfiguration)
    };
    let sha = |text: Option<String>| {
        CommitSha::parse(text.ok_or(ComparisonReadError::ProposalNotReady)?)
            .map_err(|_| ComparisonReadError::InvalidConfiguration)
    };
    Ok(ReadContext {
        actor,
        organization: row.organization_id,
        proposal,
        connection: RepositoryBinding {
            repository: row.repository_id,
            remote_id: number(&row.remote_id)?,
            installation_id: number(&row.installation_id)?,
        },
        branch: BranchName::parse(row.branch)
            .map_err(|_| ComparisonReadError::InvalidConfiguration)?,
        recorded_head: sha(row.head_sha)?,
        recorded_base: sha(row.base_sha)?,
        version: row.version,
        state: row.state,
    })
}
