//! Short, read-only scope transactions. No provider call occurs while database
//! locks are held. Callers must re-resolve before crossing publication boundaries.
use async_trait::async_trait;
use specspace_application::{
    review::{ReviewAssignment, ReviewContext},
    review_configuration::ReviewerSelection,
    review_context::{ReviewAuthorization, ReviewContextError, ReviewExpectation, ReviewScope},
};
use specspace_domain::policy::{
    self, Action, Membership, ResourceAccess, ReviewDomain, Role, UiImpact,
};
use sqlx::{FromRow, PgConnection, PgPool};
use uuid::Uuid;

pub struct PgReviewAuthorization {
    pool: PgPool,
    submission: Option<crate::review_submissions::SubmissionLease>,
}
impl PgReviewAuthorization {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            submission: None,
        }
    }
    pub(crate) fn for_submission(
        pool: PgPool,
        submission: crate::review_submissions::SubmissionLease,
    ) -> Self {
        Self {
            pool,
            submission: Some(submission),
        }
    }
}
fn unavailable(_: sqlx::Error) -> ReviewContextError {
    ReviewContextError::Unavailable
}
#[derive(FromRow)]
struct Metadata {
    author_id: Uuid,
    owner_id: Uuid,
    team_id: Uuid,
    description: String,
    review_configuration_version: i64,
    policy_version: i64,
    role: String,
    review_domains: Vec<String>,
}
#[derive(FromRow)]
struct Revision {
    ui_impact: String,
    ui_impact_reason: Option<String>,
    assignments: serde_json::Value,
}
#[derive(FromRow)]
struct User {
    id: Uuid,
    status: String,
}
#[derive(FromRow)]
struct Member {
    user_id: Uuid,
    role: String,
    review_domains: Vec<String>,
}
#[async_trait]
impl ReviewAuthorization for PgReviewAuthorization {
    async fn resolve(
        &self,
        actor: Uuid,
        proposal: Uuid,
        expectation: &ReviewExpectation,
    ) -> Result<ReviewScope, ReviewContextError> {
        if actor.is_nil()
            || proposal.is_nil()
            || expectation.proposal_version < 1
            || expectation.configuration_version == 0
            || expectation.policy_version == 0
        {
            return Err(ReviewContextError::InvalidConfiguration);
        }
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        sqlx::query("SET LOCAL lock_timeout='5s'")
            .execute(&mut *tx)
            .await
            .map_err(unavailable)?;
        sqlx::query("SET LOCAL statement_timeout='10s'")
            .execute(&mut *tx)
            .await
            .map_err(unavailable)?;
        let scope = if let Some(lease) = &self.submission {
            if actor != lease.actor()
                || proposal != lease.proposal()
                || expectation != lease.expectation()
            {
                return Err(ReviewContextError::LeaseLost);
            }
            lease_context_in(&mut tx, lease, false).await?
        } else {
            context_in(&mut tx, actor, proposal, expectation).await?
        };
        tx.commit().await.map_err(unavailable)?;
        Ok(scope)
    }
}
pub(crate) async fn context_in(
    connection: &mut PgConnection,
    actor: Uuid,
    proposal: Uuid,
    expectation: &ReviewExpectation,
) -> Result<ReviewScope, ReviewContextError> {
    context_excluding(connection, actor, proposal, expectation, None).await
}
pub(crate) async fn lease_context_in(
    connection: &mut PgConnection,
    lease: &crate::review_submissions::SubmissionLease,
    write: bool,
) -> Result<ReviewScope, ReviewContextError> {
    let failure = |error: crate::review_submissions::SubmissionError| match error {
        crate::review_submissions::SubmissionError::LeaseLost => ReviewContextError::LeaseLost,
        _ => ReviewContextError::Unavailable,
    };
    crate::review_submissions::lock_proposal(connection, lease)
        .await
        .map_err(failure)?;
    crate::review_submissions::lock_lease(connection, lease, write)
        .await
        .map_err(failure)?;
    let scope = context_excluding(
        connection,
        lease.actor(),
        lease.proposal(),
        lease.expectation(),
        Some(lease.operation_id()),
    )
    .await?;
    if !lease.matches_scope(&scope) {
        return Err(ReviewContextError::Stale);
    }
    // A slow authorization read must not extend or return an expired fence.
    crate::review_submissions::lock_lease(connection, lease, write)
        .await
        .map_err(failure)?;
    Ok(scope)
}
async fn context_excluding(
    connection: &mut PgConnection,
    actor: Uuid,
    proposal: Uuid,
    expectation: &ReviewExpectation,
    own_operation: Option<Uuid>,
) -> Result<ReviewScope, ReviewContextError> {
    let comparison = crate::comparison_authorization::context_in(connection, actor, proposal, true)
        .await
        .map_err(ReviewContextError::Read)?;
    if !matches!(
        comparison.state.as_str(),
        "DRAFT" | "IN_REVIEW" | "CHANGES_REQUESTED" | "APPROVED"
    ) {
        return Err(ReviewContextError::StateChanged);
    }
    let row = sqlx::query_as::<_,Metadata>("SELECT p.author_id,p.owner_id,p.team_id,p.description,p.review_configuration_version,o.policy_version,m.role,m.review_domains FROM proposals p JOIN organizations o ON o.id=p.organization_id JOIN memberships m ON m.organization_id=p.organization_id WHERE p.id=$1 AND m.user_id=$2 FOR SHARE OF p,o,m")
        .bind(proposal).bind(actor).fetch_one(&mut *connection).await.map_err(unavailable)?;
    let actor_member = membership(
        comparison.organization,
        actor,
        true,
        &row.role,
        &row.review_domains,
    )?;
    let collaborator: Option<String> = sqlx::query_scalar("SELECT access FROM proposal_collaborators WHERE organization_id=$1 AND proposal_id=$2 AND user_id=$3 FOR SHARE")
        .bind(comparison.organization).bind(proposal).bind(actor).fetch_optional(&mut *connection).await.map_err(unavailable)?;
    let assigned = row.owner_id == actor || collaborator.as_deref() == Some("EDIT");
    if !policy::authorized(
        &actor_member,
        &ResourceAccess {
            organization_id: comparison.organization,
            repository_access: true,
            assigned_editor: assigned,
            export_allowed: false,
        },
        Action::Edit,
    ) {
        return Err(ReviewContextError::Forbidden);
    }
    if comparison.recorded_head != expectation.head
        || comparison.version != expectation.proposal_version
        || u64::try_from(row.review_configuration_version).ok()
            != Some(expectation.configuration_version)
        || u64::try_from(row.policy_version).ok() != Some(expectation.policy_version)
    {
        return Err(ReviewContextError::Stale);
    }
    let busy: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM git_operations WHERE organization_id=$1 AND proposal_id=$2 AND kind IN ('COMMIT','SUBMIT','IMPORT_APPLY','REFRESH_BASE','MERGE','REVERT') AND state IN ('PENDING','RUNNING','UNKNOWN') AND ($3::uuid IS NULL OR id<>$3))")
        .bind(comparison.organization).bind(proposal).bind(own_operation).fetch_one(&mut *connection).await.map_err(unavailable)?;
    if busy {
        return Err(ReviewContextError::Busy);
    }
    // Read the header in a new statement after locking the proposal. A header
    // joined into the initial locking read can be invisible after a writer wait.
    let revision = sqlx::query_as::<_,Revision>("SELECT ui_impact,ui_impact_reason,assignments FROM proposal_review_configurations WHERE organization_id=$1 AND proposal_id=$2 AND version=$3")
        .bind(comparison.organization).bind(proposal).bind(row.review_configuration_version).fetch_one(&mut *connection).await.map_err(unavailable)?;
    let selected: Vec<ReviewerSelection> = serde_json::from_value(revision.assignments)
        .map_err(|_| ReviewContextError::InvalidConfiguration)?;
    if selected.len() > 200 {
        return Err(ReviewContextError::InvalidConfiguration);
    }
    let mut users: Vec<_> = selected
        .iter()
        .map(|item| item.reviewer_id)
        .chain([row.owner_id])
        .collect();
    users.sort();
    users.dedup();
    let active_users = sqlx::query_as::<_, User>(
        "SELECT id,status FROM users WHERE id=ANY($1) ORDER BY id FOR SHARE",
    )
    .bind(&users)
    .fetch_all(&mut *connection)
    .await
    .map_err(unavailable)?;
    let members = sqlx::query_as::<_,Member>("SELECT user_id,role,review_domains FROM memberships WHERE organization_id=$1 AND user_id=ANY($2) ORDER BY user_id FOR SHARE")
        .bind(comparison.organization).bind(&users).fetch_all(&mut *connection).await.map_err(unavailable)?;
    let memberships: Vec<_> = members
        .iter()
        .map(|member| {
            membership(
                comparison.organization,
                member.user_id,
                active_users
                    .iter()
                    .any(|user| user.id == member.user_id && user.status == "ACTIVE"),
                &member.role,
                &member.review_domains,
            )
        })
        .collect::<Result<_, _>>()?;
    if !memberships.iter().any(|member| {
        member.user_id == row.owner_id && member.active && member.role != Role::Reader
    }) {
        return Err(ReviewContextError::OwnerNotEligible);
    }
    let reviewer_memberships = memberships
        .into_iter()
        .filter(|member| {
            selected
                .iter()
                .any(|item| item.reviewer_id == member.user_id)
        })
        .collect();
    let assignments = selected
        .into_iter()
        .map(|item| ReviewAssignment {
            reviewer: item.reviewer_id,
            domain: match item.domain {
                specspace_application::review_configuration::ReviewArea::Product => {
                    ReviewDomain::Product
                }
                specspace_application::review_configuration::ReviewArea::Development => {
                    ReviewDomain::Development
                }
                specspace_application::review_configuration::ReviewArea::Design => {
                    ReviewDomain::Design
                }
            },
            required: item.required,
        })
        .collect();
    let review = ReviewContext {
        actor: actor_member,
        assigned_editor: assigned,
        repository_access: true,
        proposal,
        repository: comparison.connection.repository,
        author: row.author_id,
        owner: row.owner_id,
        team: row.team_id,
        description: row.description,
        expected_head: expectation.head.clone(),
        expected_target: expectation.target.clone(),
        expected_version: expectation.proposal_version,
        policy_version: expectation.policy_version,
        configuration_version: expectation.configuration_version,
        ui_impact: match revision.ui_impact.as_str() {
            "YES" => UiImpact::Yes,
            "NO" => UiImpact::No,
            "UNDECIDED" => UiImpact::Undecided,
            _ => return Err(ReviewContextError::InvalidConfiguration),
        },
        ui_impact_reason: revision.ui_impact_reason,
        assignments,
        reviewer_memberships,
    };
    Ok(ReviewScope { comparison, review })
}
fn membership(
    organization_id: Uuid,
    user_id: Uuid,
    active: bool,
    role: &str,
    domains: &[String],
) -> Result<Membership, ReviewContextError> {
    let role = match role {
        "READER" => Role::Reader,
        "CONTRIBUTOR" => Role::Contributor,
        "REVIEWER" => Role::Reviewer,
        "MAINTAINER" => Role::Maintainer,
        "ADMIN" => Role::Admin,
        _ => return Err(ReviewContextError::InvalidConfiguration),
    };
    // Stable order and deduplication make semantic rechecks independent of the
    // database array order, including users qualified in multiple domains.
    let mut review_domains = vec![];
    for (name, domain) in [
        ("PRODUCT", ReviewDomain::Product),
        ("DEVELOPMENT", ReviewDomain::Development),
        ("DESIGN", ReviewDomain::Design),
    ] {
        if domains.iter().any(|value| value == name) {
            review_domains.push(domain);
        }
    }
    if domains
        .iter()
        .any(|name| !matches!(name.as_str(), "PRODUCT" | "DEVELOPMENT" | "DESIGN"))
    {
        return Err(ReviewContextError::InvalidConfiguration);
    }
    Ok(Membership {
        organization_id,
        user_id,
        active,
        role,
        review_domains,
    })
}
