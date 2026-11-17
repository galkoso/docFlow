//! Immutable review settings and actor-bound replay receipts. No provider calls,
//! PR creation, content publication or successful review state is implied.
use specspace_application::{proposal::UiImpact, review_configuration::*};
use sqlx::{FromRow, PgPool, Postgres, Transaction};
use std::collections::HashMap;
use uuid::Uuid;
#[derive(Debug, PartialEq, Eq)]
pub enum ConfigurationError {
    NotFound,
    Forbidden,
    Stale,
    Busy,
    StateChanged,
    KeyConflict,
    IneligibleReviewer,
    Invalid(&'static str),
    Unavailable,
}
impl From<sqlx::Error> for ConfigurationError {
    fn from(_: sqlx::Error) -> Self {
        Self::Unavailable
    }
}
pub struct PgReviewConfiguration {
    pool: PgPool,
}
#[derive(FromRow)]
struct Current {
    proposal_id: Uuid,
    organization_id: Uuid,
    author_id: Uuid,
    owner_id: Uuid,
    role: String,
    state: String,
    head_sha: Option<String>,
    version: i64,
    review_configuration_version: i64,
    policy_version: i64,
    ui_impact: String,
    ui_impact_reason: Option<String>,
    assignments: serde_json::Value,
}
#[derive(FromRow)]
struct User {
    id: Uuid,
    display_name: String,
    status: String,
}
#[derive(FromRow)]
struct Member {
    user_id: Uuid,
    role: String,
    review_domains: Vec<String>,
}
#[derive(FromRow)]
struct Receipt {
    request_hash: String,
    applied_proposal_version: i64,
    applied_configuration_version: i64,
    changed: bool,
}
impl PgReviewConfiguration {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
    async fn transaction(&self) -> Result<Transaction<'_, Postgres>, ConfigurationError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SET LOCAL lock_timeout='5s'")
            .execute(&mut *tx)
            .await?;
        sqlx::query("SET LOCAL statement_timeout='10s'")
            .execute(&mut *tx)
            .await?;
        Ok(tx)
    }
    pub async fn read(
        &self,
        actor: Uuid,
        proposal: Uuid,
    ) -> Result<ReviewConfiguration, ConfigurationError> {
        let mut tx = self.transaction().await?;
        let result = read_in(&mut tx, actor, proposal).await?;
        tx.commit().await?;
        Ok(result)
    }
    pub async fn update(
        &self,
        actor: Uuid,
        proposal: Uuid,
        mut request: ReviewConfigurationRequest,
    ) -> Result<ConfigurationUpdated, ConfigurationError> {
        request.normalize().map_err(ConfigurationError::Invalid)?;
        let mut tx = self.transaction().await?;
        let row = current(&mut tx, actor, proposal, true).await?;
        if !editor(&mut tx, &row, actor).await? {
            return Err(ConfigurationError::Forbidden);
        }
        let fingerprint = request.fingerprint(row.organization_id, proposal, actor);
        let receipt=sqlx::query_as::<_,Receipt>("SELECT request_hash,applied_proposal_version,applied_configuration_version,changed FROM review_configuration_operations WHERE organization_id=$1 AND proposal_id=$2 AND actor_id=$3 AND request_id=$4")
            .bind(row.organization_id).bind(proposal).bind(actor).bind(request.request_id).fetch_optional(&mut *tx).await?;
        if let Some(receipt) = receipt {
            if receipt.request_hash != fingerprint {
                return Err(ConfigurationError::KeyConflict);
            }
            let result = ConfigurationUpdated {
                proposal_id: proposal,
                request_id: request.request_id,
                applied_proposal_version: receipt.applied_proposal_version,
                applied_configuration_version: receipt.applied_configuration_version,
                changed: receipt.changed,
                replayed: true,
                current: read_in(&mut tx, actor, proposal).await?,
            };
            tx.commit().await?;
            return Ok(result);
        }
        if !open(&row.state) {
            return Err(ConfigurationError::StateChanged);
        }
        if busy(&mut tx, row.organization_id, proposal).await? {
            return Err(ConfigurationError::Busy);
        }
        if row.head_sha.as_deref() != Some(&request.expected_head_sha)
            || row.version != request.expected_proposal_version
            || row.review_configuration_version != request.expected_configuration_version
        {
            return Err(ConfigurationError::Stale);
        }
        let reviewers = statuses(
            &mut tx,
            row.organization_id,
            row.author_id,
            &request.settings.assignments,
        )
        .await?;
        if reviewers
            .iter()
            .any(|reviewer| !reviewer.active || !reviewer.qualified)
        {
            return Err(ConfigurationError::IneligibleReviewer);
        }
        let changed = settings(&row)? != request.settings;
        let (version, configuration) = if changed {
            (
                row.version
                    .checked_add(1)
                    .ok_or(ConfigurationError::Unavailable)?,
                row.review_configuration_version
                    .checked_add(1)
                    .ok_or(ConfigurationError::Unavailable)?,
            )
        } else {
            (row.version, row.review_configuration_version)
        };
        if changed {
            let assignments = serde_json::to_value(&request.settings.assignments)
                .map_err(|_| ConfigurationError::Unavailable)?;
            sqlx::query("INSERT INTO proposal_review_configurations(organization_id,proposal_id,version,ui_impact,ui_impact_reason,assignments,created_by) VALUES($1,$2,$3,$4,$5,$6,$7)")
                .bind(row.organization_id).bind(proposal).bind(configuration).bind(request.settings.ui_impact.as_str()).bind(&request.settings.ui_impact_reason).bind(assignments).bind(actor).execute(&mut *tx).await?;
            for reviewer in &request.settings.assignments {
                sqlx::query("INSERT INTO proposal_review_assignments(organization_id,proposal_id,configuration_version,reviewer_id,domain,required) VALUES($1,$2,$3,$4,$5,$6)")
                    .bind(row.organization_id).bind(proposal).bind(configuration).bind(reviewer.reviewer_id).bind(reviewer.domain.as_str()).bind(reviewer.required).execute(&mut *tx).await?;
            }
            sqlx::query("UPDATE proposals SET review_configuration_version=$2,version=$3,ui_impact=$4,ui_impact_reason=$5,state=CASE WHEN state='APPROVED' THEN 'IN_REVIEW' ELSE state END,updated_at=clock_timestamp() WHERE id=$1 AND organization_id=$6")
                .bind(proposal).bind(configuration).bind(version).bind(request.settings.ui_impact.as_str()).bind(&request.settings.ui_impact_reason).bind(row.organization_id).execute(&mut *tx).await?;
            sqlx::query("INSERT INTO audit_events(id,organization_id,actor_id,action,entity_id,before_ref,after_ref,result) VALUES($1,$2,$3,'REVIEW_CONFIGURATION_CHANGED',$4,$5,$6,'SAVED')")
                .bind(Uuid::new_v4()).bind(row.organization_id).bind(actor).bind(proposal).bind(row.review_configuration_version.to_string()).bind(configuration.to_string()).execute(&mut *tx).await?;
            sqlx::query("INSERT INTO outbox(id,organization_id,event_type,entity_id,payload) VALUES($1,$2,'PROPOSAL_REVIEW_CONFIGURATION_CHANGED',$3,$4)")
                .bind(Uuid::new_v4()).bind(row.organization_id).bind(proposal).bind(serde_json::json!({"configuration_version":configuration,"proposal_version":version,"invalidate_review":true})).execute(&mut *tx).await?;
        }
        sqlx::query("INSERT INTO review_configuration_operations(organization_id,proposal_id,actor_id,request_id,request_hash,applied_proposal_version,applied_configuration_version,changed) VALUES($1,$2,$3,$4,$5,$6,$7,$8)")
            .bind(row.organization_id).bind(proposal).bind(actor).bind(request.request_id).bind(fingerprint).bind(version).bind(configuration).bind(changed).execute(&mut *tx).await?;
        let result = ConfigurationUpdated {
            proposal_id: proposal,
            request_id: request.request_id,
            applied_proposal_version: version,
            applied_configuration_version: configuration,
            changed,
            replayed: false,
            current: read_in(&mut tx, actor, proposal).await?,
        };
        tx.commit().await?;
        Ok(result)
    }
}
fn open(state: &str) -> bool {
    matches!(
        state,
        "DRAFT" | "IN_REVIEW" | "CHANGES_REQUESTED" | "APPROVED"
    )
}
async fn current(
    tx: &mut Transaction<'_, Postgres>,
    actor: Uuid,
    proposal: Uuid,
    write: bool,
) -> Result<Current, ConfigurationError> {
    let lock = if write {
        "FOR UPDATE OF p FOR SHARE OF r,m,u,o"
    } else {
        "FOR SHARE OF p,r,m,u,o"
    };
    let mut row = sqlx::query_as::<_,Current>(&format!("SELECT p.id AS proposal_id,p.organization_id,p.author_id,p.owner_id,m.role,p.state,p.head_sha,p.version,p.review_configuration_version,o.policy_version,p.ui_impact,p.ui_impact_reason,'[]'::jsonb AS assignments FROM proposals p JOIN repositories r ON r.id=p.repository_id AND r.organization_id=p.organization_id JOIN organizations o ON o.id=p.organization_id JOIN memberships m ON m.organization_id=p.organization_id JOIN users u ON u.id=m.user_id WHERE p.id=$1 AND u.id=$2 AND u.status='ACTIVE' {lock}"))
        .bind(proposal).bind(actor).fetch_optional(&mut **tx).await?.ok_or(ConfigurationError::NotFound)?;
    // Lock the proposal before reading its immutable revision. A joined header
    // lookup can use a pre-wait statement snapshot and miss the revision just
    // published by the transaction that held this proposal lock.
    row.assignments = sqlx::query_scalar("SELECT assignments FROM proposal_review_configurations WHERE organization_id=$1 AND proposal_id=$2 AND version=$3")
        .bind(row.organization_id).bind(proposal).bind(row.review_configuration_version).fetch_one(&mut **tx).await?;
    Ok(row)
}
async fn editor(
    tx: &mut Transaction<'_, Postgres>,
    row: &Current,
    actor: Uuid,
) -> Result<bool, ConfigurationError> {
    if row.role == "READER" {
        return Ok(false);
    }
    if row.role == "ADMIN" || row.owner_id == actor {
        return Ok(true);
    }
    let access:Option<String>=sqlx::query_scalar("SELECT access FROM proposal_collaborators WHERE organization_id=$1 AND proposal_id=$2 AND user_id=$3 FOR SHARE")
        .bind(row.organization_id).bind(row.proposal_id).bind(actor).fetch_optional(&mut **tx).await?;
    Ok(access.as_deref() == Some("EDIT"))
}
async fn busy(
    tx: &mut Transaction<'_, Postgres>,
    organization: Uuid,
    proposal: Uuid,
) -> Result<bool, ConfigurationError> {
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM git_operations WHERE organization_id=$1 AND proposal_id=$2 AND kind IN ('COMMIT','IMPORT_APPLY','REFRESH_BASE','MERGE','REVERT','SUBMIT') AND state IN ('PENDING','RUNNING','UNKNOWN'))")
        .bind(organization).bind(proposal).fetch_one(&mut **tx).await?)
}
fn settings(row: &Current) -> Result<ReviewSettings, ConfigurationError> {
    let impact = match row.ui_impact.as_str() {
        "YES" => UiImpact::Yes,
        "NO" => UiImpact::No,
        "UNDECIDED" => UiImpact::Undecided,
        _ => return Err(ConfigurationError::Unavailable),
    };
    Ok(ReviewSettings {
        ui_impact: impact,
        ui_impact_reason: row.ui_impact_reason.clone(),
        assignments: serde_json::from_value(row.assignments.clone())
            .map_err(|_| ConfigurationError::Unavailable)?,
    })
}
async fn read_in(
    tx: &mut Transaction<'_, Postgres>,
    actor: Uuid,
    proposal: Uuid,
) -> Result<ReviewConfiguration, ConfigurationError> {
    let row = current(tx, actor, proposal, false).await?;
    let settings = settings(&row)?;
    let reviewers = statuses(
        tx,
        row.organization_id,
        row.author_id,
        &settings.assignments,
    )
    .await?;
    let can_edit = open(&row.state)
        && editor(tx, &row, actor).await?
        && !busy(tx, row.organization_id, proposal).await?;
    Ok(ReviewConfiguration {
        proposal_id: proposal,
        proposal_version: row.version,
        configuration_version: row.review_configuration_version,
        policy_version: row.policy_version,
        head_sha: row.head_sha,
        state: row.state,
        can_edit,
        settings,
        reviewers,
    })
}
async fn statuses(
    tx: &mut Transaction<'_, Postgres>,
    organization: Uuid,
    author: Uuid,
    selected: &[ReviewerSelection],
) -> Result<Vec<ReviewerStatus>, ConfigurationError> {
    let ids: Vec<_> = selected
        .iter()
        .map(|reviewer| reviewer.reviewer_id)
        .collect();
    let users=sqlx::query_as::<_,User>("SELECT id,left(display_name,200) AS display_name,status FROM users WHERE id=ANY($1) ORDER BY id FOR SHARE").bind(&ids).fetch_all(&mut **tx).await?;
    let members=sqlx::query_as::<_,Member>("SELECT user_id,role,review_domains FROM memberships WHERE organization_id=$1 AND user_id=ANY($2) ORDER BY user_id FOR SHARE")
        .bind(organization).bind(&ids).fetch_all(&mut **tx).await?;
    let users: HashMap<_, _> = users.into_iter().map(|user| (user.id, user)).collect();
    let members: HashMap<_, _> = members
        .into_iter()
        .map(|member| (member.user_id, member))
        .collect();
    Ok(selected
        .iter()
        .map(|selection| {
            let user = users.get(&selection.reviewer_id);
            let member = members.get(&selection.reviewer_id);
            let active = user.is_some_and(|user| user.status == "ACTIVE") && member.is_some();
            let qualified = active
                && selection.reviewer_id != author
                && member.is_some_and(|member| {
                    matches!(member.role.as_str(), "REVIEWER" | "MAINTAINER" | "ADMIN")
                        && member
                            .review_domains
                            .iter()
                            .any(|domain| domain == selection.domain.as_str())
                });
            ReviewerStatus {
                reviewer_id: selection.reviewer_id,
                domain: selection.domain,
                required: selection.required,
                display_name: user
                    .map(|user| user.display_name.clone())
                    .unwrap_or_else(|| "משתמש לא זמין".into()),
                active,
                qualified,
            }
        })
        .collect())
}
