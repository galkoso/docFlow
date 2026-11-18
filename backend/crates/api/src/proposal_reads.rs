//! Workflow metadata is read from PostgreSQL; provider observations are explicit
//! and never change the proposal's recorded SHA or silently repair a branch.
use crate::{
    AppState, auth,
    proposals::{failure, invalid_query, problem},
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use specspace_application::{
    book::BookError,
    proposal::{InitialMode, ProposalError, ProposalLink, UiImpact},
};
use specspace_domain::{
    BranchName,
    policy::{self, Action, Membership, ResourceAccess, Role},
};
use specspace_git_adapter::GitError;
use sqlx::{
    FromRow, Postgres, QueryBuilder,
    types::chrono::{DateTime, Utc},
};
use uuid::Uuid;

const STATES: [&str; 8] = [
    "PROVISIONING",
    "DRAFT",
    "IN_REVIEW",
    "CHANGES_REQUESTED",
    "APPROVED",
    "MERGING",
    "MERGED",
    "CLOSED",
];
const SUMMARY: &str = r#"SELECT p.id AS proposal_id,p.number,p.repository_id,p.title,p.branch,p.state,p.owner_id,owner.display_name AS owner_name,p.team_id,t.name AS team_name,coalesce(p.base_sha,CASE WHEN p.state='PROVISIONING' THEN co.expected_sha END) AS base_sha,p.head_sha,coalesce(p.target_sha,CASE WHEN p.state='PROVISIONING' THEN co.expected_sha END) AS target_sha,p.version,r.default_branch AS target_branch,to_char(p.updated_at AT TIME ZONE 'UTC','YYYY-MM-DD"T"HH24:MI:SS.US"Z"') AS updated_at,CASE WHEN p.head_sha=p.base_sha THEN 0::bigint END AS changed_documents,CASE WHEN p.head_sha=p.base_sha THEN 0::bigint END AS changed_sections,cj.id AS creation_job_id,co.error_code AS creation_error_code"#;
const JOINS: &str = " FROM proposals p JOIN repositories r ON r.id=p.repository_id AND r.organization_id=p.organization_id JOIN users owner ON owner.id=p.owner_id JOIN teams t ON t.id=p.team_id AND t.organization_id=p.organization_id JOIN memberships viewer ON viewer.organization_id=p.organization_id JOIN users actor ON actor.id=viewer.user_id LEFT JOIN git_operations co ON co.proposal_id=p.id AND co.organization_id=p.organization_id AND co.kind='CREATE' LEFT JOIN jobs cj ON cj.operation_id=co.id AND cj.organization_id=p.organization_id AND cj.kind='CREATE_PROPOSAL' ";

#[derive(Serialize, FromRow, utoipa::ToSchema)]
pub struct ProposalSummary {
    pub proposal_id: Uuid,
    pub number: i64,
    pub repository_id: Uuid,
    pub title: String,
    pub branch: String,
    pub state: String,
    pub owner_id: Uuid,
    pub owner_name: String,
    pub team_id: Uuid,
    pub team_name: String,
    pub base_sha: Option<String>,
    pub head_sha: Option<String>,
    pub target_sha: Option<String>,
    pub version: i64,
    pub target_branch: String,
    pub updated_at: String,
    /// Unknown until an actual diff has been computed; equal base/head is exactly zero.
    pub changed_documents: Option<i64>,
    pub changed_sections: Option<i64>,
    pub creation_job_id: Option<Uuid>,
    pub creation_error_code: Option<String>,
}
#[derive(Serialize, utoipa::ToSchema)]
pub struct ProposalPage {
    pub proposals: Vec<ProposalSummary>,
    pub next_cursor: Option<String>,
}
#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in=Query)]
pub(crate) struct ListQuery {
    repository_id: Option<Uuid>,
    state: Option<String>,
    team_id: Option<Uuid>,
    owner_id: Option<Uuid>,
    #[param(max_length = 200)]
    q: Option<String>,
    cursor: Option<String>,
    #[param(minimum = 1, maximum = 200, default = 50)]
    limit: Option<usize>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    actor: Uuid,
    filter: String,
    updated_at: String,
    proposal_id: Uuid,
}
fn filter_hash(query: &ListQuery) -> Result<String, ()> {
    serde_json::to_vec(&(
        query.repository_id,
        &query.state,
        query.team_id,
        query.owner_id,
        &query.q,
        query.limit.unwrap_or(50),
    ))
    .map(|bytes| format!("{:x}", Sha256::digest(bytes)))
    .map_err(|_| ())
}
fn invalid_cursor() -> Response {
    problem(
        StatusCode::BAD_REQUEST,
        "INVALID_CURSOR",
        "מיקום הרשימה אינו תואם לסינון. יש לטעון את הרשימה מחדש.",
        false,
        None,
    )
}

#[utoipa::path(get,path="/api/v1/proposals",security(("sessionCookie"=[])),params(ListQuery),
    responses((status=200,body=ProposalPage),(status=400,body=crate::ErrorBody),(status=401,body=crate::ErrorBody),(status=404,body=crate::ErrorBody),(status=503,body=crate::ErrorBody)))]
pub(crate) async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    query: Result<Query<ListQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let actor = match auth::authenticate(&state, &headers).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    let Ok(Query(query)) = query else {
        return invalid_query();
    };
    let limit = query.limit.unwrap_or(50);
    if !(1..=200).contains(&limit)
        || query
            .q
            .as_ref()
            .is_some_and(|q| q.chars().count() > 200 || q.contains('\0'))
        || query
            .state
            .as_deref()
            .is_some_and(|state| !STATES.contains(&state))
    {
        return invalid_query();
    }
    if let Some(repository) = query.repository_id
        && let Err(error) = state.books.authorize(actor.user_id, repository).await
    {
        return crate::specification::failure(error);
    }
    let filter = match filter_hash(&query) {
        Ok(hash) => hash,
        Err(_) => return failure(ProposalError::Unavailable),
    };
    let position = match query.cursor.as_deref() {
        Some(encoded) if encoded.len() <= 2048 => {
            let cursor = URL_SAFE_NO_PAD
                .decode(encoded)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Cursor>(&bytes).ok());
            let Some(cursor) =
                cursor.filter(|cursor| cursor.actor == actor.user_id && cursor.filter == filter)
            else {
                return invalid_cursor();
            };
            let Ok(at) = DateTime::parse_from_rfc3339(&cursor.updated_at) else {
                return invalid_cursor();
            };
            Some((at.with_timezone(&Utc), cursor.proposal_id))
        }
        Some(_) => return invalid_cursor(),
        None => None,
    };
    let mut sql = QueryBuilder::<Postgres>::new(SUMMARY);
    sql.push(JOINS)
        .push(" WHERE viewer.user_id=")
        .push_bind(actor.user_id)
        .push(" AND actor.status='ACTIVE'");
    if let Some(repository) = query.repository_id {
        sql.push(" AND p.repository_id=").push_bind(repository);
    }
    if let Some(status) = &query.state {
        sql.push(" AND p.state=").push_bind(status);
    }
    if let Some(team) = query.team_id {
        sql.push(" AND p.team_id=").push_bind(team);
    }
    if let Some(owner) = query.owner_id {
        sql.push(" AND p.owner_id=").push_bind(owner);
    }
    if let Some(q) = &query.q {
        sql.push(" AND (strpos(lower(p.title),lower(")
            .push_bind(q)
            .push("))>0 OR strpos(lower(p.branch),lower(")
            .push_bind(q)
            .push("))>0)");
    }
    if let Some((at, id)) = position {
        sql.push(" AND (p.updated_at,p.id)<(")
            .push_bind(at)
            .push(",")
            .push_bind(id)
            .push(")");
    }
    sql.push(" ORDER BY p.updated_at DESC,p.id DESC LIMIT ")
        .push_bind((limit + 1) as i64);
    let mut rows = match sql
        .build_query_as::<ProposalSummary>()
        .fetch_all(&state.database)
        .await
    {
        Ok(rows) => rows,
        Err(_) => return failure(ProposalError::Unavailable),
    };
    let more = rows.len() > limit;
    rows.truncate(limit);
    let next_cursor = if more {
        rows.last()
            .and_then(|row| {
                serde_json::to_vec(&Cursor {
                    actor: actor.user_id,
                    filter,
                    updated_at: row.updated_at.clone(),
                    proposal_id: row.proposal_id,
                })
                .ok()
            })
            .map(|bytes| URL_SAFE_NO_PAD.encode(bytes))
    } else {
        None
    };
    for row in &mut rows {
        row.creation_error_code = clean_code(row.creation_error_code.take());
    }
    Json(ProposalPage {
        proposals: rows,
        next_cursor,
    })
    .into_response()
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct ProposalCapabilities {
    pub can_edit: bool,
    pub can_close: bool,
    pub can_reopen: bool,
}
#[derive(Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SyncState {
    Provisioning,
    InSync,
    HeadMoved,
    ProviderMissing,
    ProviderUnavailable,
    NotConfigured,
}
#[derive(Serialize, utoipa::ToSchema)]
pub struct ProposalSync {
    pub state: SyncState,
    pub actual_head_sha: Option<String>,
    pub current_main_sha: Option<String>,
    pub base_outdated: Option<bool>,
    pub main_error_code: Option<String>,
    pub retry_after_seconds: Option<u64>,
}
#[derive(Serialize, utoipa::ToSchema)]
pub struct ProposalDetail {
    #[serde(flatten)]
    pub proposal: ProposalSummary,
    pub description: String,
    pub author_id: Uuid,
    pub initial_mode: InitialMode,
    pub ui_impact: UiImpact,
    pub ui_impact_reason: Option<String>,
    pub links: Vec<ProposalLink>,
    pub capabilities: ProposalCapabilities,
    pub synchronization: ProposalSync,
}
#[derive(FromRow)]
struct DetailRow {
    #[sqlx(flatten)]
    summary: ProposalSummary,
    description: String,
    author_id: Uuid,
    initial_mode: String,
    ui_impact: String,
    ui_impact_reason: Option<String>,
    links: sqlx::types::Json<Vec<ProposalLink>>,
    organization_id: Uuid,
    role: String,
    assigned_editor: bool,
}
async fn detail_row(
    state: &AppState,
    actor: Uuid,
    proposal: Uuid,
) -> Result<DetailRow, ProposalError> {
    let sql = format!(
        "{SUMMARY},p.description,p.author_id,p.initial_mode,p.ui_impact,p.ui_impact_reason,p.links,p.organization_id,viewer.role,(p.owner_id=$1 OR EXISTS(SELECT 1 FROM proposal_collaborators c WHERE c.organization_id=p.organization_id AND c.proposal_id=p.id AND c.user_id=$1 AND c.access='EDIT')) AS assigned_editor{JOINS} WHERE viewer.user_id=$1 AND actor.status='ACTIVE' AND p.id=$2"
    );
    sqlx::query_as(&sql)
        .bind(actor)
        .bind(proposal)
        .fetch_optional(&state.database)
        .await
        .map_err(|_| ProposalError::Unavailable)?
        .ok_or(ProposalError::NotFound)
}
#[utoipa::path(get,path="/api/v1/proposals/{proposal}",security(("sessionCookie"=[])),params(("proposal"=Uuid,Path),("X-Expected-Actor"=Option<Uuid>,Header,description="When present, binds this read to the captured actor")),
    responses((status=200,body=ProposalDetail),(status=400,body=crate::ErrorBody),(status=401,body=crate::ErrorBody),(status=404,body=crate::ErrorBody),(status=409,body=crate::ErrorBody),(status=503,body=crate::ErrorBody)))]
pub(crate) async fn detail(
    State(state): State<AppState>,
    headers: HeaderMap,
    path: Result<Path<Uuid>, axum::extract::rejection::PathRejection>,
) -> Response {
    let actor = match auth::authenticate(&state, &headers).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    if headers.contains_key("X-Expected-Actor")
        && let Err(error) = auth::verify_actor(&headers, &actor)
    {
        return error.into_response();
    }
    let Ok(Path(proposal)) = path else {
        return invalid_query();
    };
    let mut row = match detail_row(&state, actor.user_id, proposal).await {
        Ok(row) => row,
        Err(error) => return failure(error),
    };
    let provisioning = row.summary.state == "PROVISIONING";
    let mut synchronization = observe(&state, &row.summary).await;
    // Provider calls may wait or race with revocation, ownership or state changes.
    // Reload current scope and capabilities before returning private metadata.
    if !provisioning {
        row = match detail_row(&state, actor.user_id, proposal).await {
            Ok(row) => row,
            Err(error) => return failure(error),
        };
    }
    synchronization.base_outdated = synchronization
        .current_main_sha
        .as_ref()
        .zip(row.summary.target_sha.as_ref())
        .map(|(main, target)| main != target);
    if let Some(actual) = &synchronization.actual_head_sha
        && matches!(
            synchronization.state,
            SyncState::InSync | SyncState::HeadMoved
        )
    {
        synchronization.state = if row.summary.head_sha.as_ref() == Some(actual) {
            SyncState::InSync
        } else {
            SyncState::HeadMoved
        };
    }
    let role = match row.role.as_str() {
        "READER" => Role::Reader,
        "CONTRIBUTOR" => Role::Contributor,
        "REVIEWER" => Role::Reviewer,
        "MAINTAINER" => Role::Maintainer,
        "ADMIN" => Role::Admin,
        _ => return failure(ProposalError::Unavailable),
    };
    let editable = matches!(
        row.summary.state.as_str(),
        "DRAFT" | "IN_REVIEW" | "CHANGES_REQUESTED" | "APPROVED"
    );
    let member = Membership {
        organization_id: row.organization_id,
        user_id: actor.user_id,
        active: true,
        role,
        review_domains: vec![],
    };
    let resource = ResourceAccess {
        organization_id: row.organization_id,
        repository_access: true,
        assigned_editor: row.assigned_editor,
        export_allowed: false,
    };
    let capabilities = ProposalCapabilities {
        can_edit: editable && policy::authorized(&member, &resource, Action::Edit),
        can_close: editable && matches!(role, Role::Maintainer | Role::Admin),
        can_reopen: row.summary.state == "CLOSED"
            && matches!(role, Role::Maintainer | Role::Admin)
            && matches!(synchronization.state, SyncState::InSync),
    };
    let initial_mode = match row.initial_mode.as_str() {
        "manual" => InitialMode::Manual,
        "ai" => InitialMode::Ai,
        _ => return failure(ProposalError::Unavailable),
    };
    let ui_impact = match row.ui_impact.as_str() {
        "YES" => UiImpact::Yes,
        "NO" => UiImpact::No,
        "UNDECIDED" => UiImpact::Undecided,
        _ => return failure(ProposalError::Unavailable),
    };
    row.summary.creation_error_code = clean_code(row.summary.creation_error_code);
    Json(ProposalDetail {
        proposal: row.summary,
        description: row.description,
        author_id: row.author_id,
        initial_mode,
        ui_impact,
        ui_impact_reason: row.ui_impact_reason,
        links: row.links.0,
        capabilities,
        synchronization,
    })
    .into_response()
}
async fn observe(state: &AppState, summary: &ProposalSummary) -> ProposalSync {
    let mut sync = ProposalSync {
        state: SyncState::Provisioning,
        actual_head_sha: None,
        current_main_sha: None,
        base_outdated: None,
        main_error_code: None,
        retry_after_seconds: None,
    };
    if summary.state == "PROVISIONING" {
        return sync;
    }
    let branch = match BranchName::parse(&summary.branch) {
        Ok(branch) => branch,
        Err(_) => {
            sync.state = SyncState::ProviderUnavailable;
            return sync;
        }
    };
    let provider = match state.book_source.provider(summary.repository_id).await {
        Ok(provider) => provider,
        Err(BookError::RepositoryNotConfigured | BookError::InvalidRepositoryConfiguration) => {
            sync.state = SyncState::NotConfigured;
            sync.main_error_code = Some("REPOSITORY_NOT_CONFIGURED".into());
            return sync;
        }
        Err(_) => {
            sync.state = SyncState::ProviderUnavailable;
            sync.main_error_code = Some("PROVIDER_UNAVAILABLE".into());
            return sync;
        }
    };
    let (head, main) = tokio::join!(provider.branch_head(&branch), provider.main_head());
    match head {
        Ok(head) => {
            sync.actual_head_sha = Some(head.as_str().into());
            sync.state = if summary.head_sha.as_deref() == Some(head.as_str()) {
                SyncState::InSync
            } else {
                SyncState::HeadMoved
            }
        }
        Err(GitError::BranchMissing) => sync.state = SyncState::ProviderMissing,
        Err(GitError::RateLimited {
            retry_after_seconds,
        }) => {
            sync.state = SyncState::ProviderUnavailable;
            sync.retry_after_seconds = retry_after_seconds
        }
        Err(_) => sync.state = SyncState::ProviderUnavailable,
    }
    match main {
        Ok(main) => sync.current_main_sha = Some(main.as_str().into()),
        Err(GitError::RateLimited {
            retry_after_seconds,
        }) => {
            sync.main_error_code = Some("PROVIDER_RATE_LIMIT".into());
            sync.retry_after_seconds = sync
                .retry_after_seconds
                .into_iter()
                .chain(retry_after_seconds)
                .max()
        }
        Err(_) => sync.main_error_code = Some("PROVIDER_UNAVAILABLE".into()),
    }
    sync
}
fn clean_code(code: Option<String>) -> Option<String> {
    code.filter(|code| {
        !code.is_empty()
            && code.len() <= 100
            && code
                .bytes()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_')
    })
}

#[derive(Serialize, FromRow, utoipa::ToSchema)]
pub struct RepositoryChoice {
    pub repository_id: Uuid,
    pub default_branch: String,
    pub can_create: bool,
}
#[derive(Serialize, FromRow, utoipa::ToSchema)]
pub struct TeamChoice {
    pub team_id: Uuid,
    pub name: String,
    pub repository_ids: Vec<Uuid>,
}
#[derive(Serialize, FromRow, utoipa::ToSchema)]
pub struct OwnerChoice {
    pub owner_id: Uuid,
    pub display_name: String,
    pub repository_ids: Vec<Uuid>,
}
#[derive(Serialize, utoipa::ToSchema)]
pub struct ProposalChoices {
    pub repositories: Vec<RepositoryChoice>,
    pub teams: Vec<TeamChoice>,
    pub owners: Vec<OwnerChoice>,
}
#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in=Query)]
pub(crate) struct OptionsQuery {
    repository_id: Option<Uuid>,
}
#[utoipa::path(get,path="/api/v1/proposals/options",security(("sessionCookie"=[])),params(OptionsQuery),
    responses((status=200,body=ProposalChoices),(status=400,body=crate::ErrorBody),(status=401,body=crate::ErrorBody),(status=404,body=crate::ErrorBody),(status=422,body=crate::ErrorBody),(status=503,body=crate::ErrorBody)))]
pub(crate) async fn options(
    State(state): State<AppState>,
    headers: HeaderMap,
    query: Result<Query<OptionsQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let actor = match auth::authenticate(&state, &headers).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    let Ok(Query(query)) = query else {
        return invalid_query();
    };
    if let Some(repository) = query.repository_id
        && let Err(error) = state.books.authorize(actor.user_id, repository).await
    {
        return crate::specification::failure(error);
    }
    let repositories=sqlx::query_as::<_,RepositoryChoice>("SELECT r.id AS repository_id,r.default_branch,m.role<>'READER' AS can_create FROM repositories r JOIN memberships m ON m.organization_id=r.organization_id JOIN users u ON u.id=m.user_id WHERE m.user_id=$1 AND u.status='ACTIVE' AND ($2::uuid IS NULL OR r.id=$2) ORDER BY r.id LIMIT 201")
        .bind(actor.user_id).bind(query.repository_id).fetch_all(&state.database).await;
    let repositories = match repositories {
        Ok(rows) => rows,
        Err(_) => return failure(ProposalError::Unavailable),
    };
    if repositories.len() > 200 {
        return choices_limit();
    }
    let ids: Vec<_> = repositories.iter().map(|row| row.repository_id).collect();
    let teams=sqlx::query_as::<_,TeamChoice>("SELECT t.id AS team_id,t.name,array_agg(DISTINCT r.id ORDER BY r.id) AS repository_ids FROM teams t JOIN repositories r ON r.organization_id=t.organization_id JOIN memberships m ON m.organization_id=r.organization_id JOIN users u ON u.id=m.user_id WHERE m.user_id=$1 AND u.status='ACTIVE' AND r.id=ANY($2) GROUP BY t.id,t.name ORDER BY t.name,t.id LIMIT 201")
        .bind(actor.user_id).bind(&ids).fetch_all(&state.database);
    let owners=sqlx::query_as::<_,OwnerChoice>("SELECT owner.id AS owner_id,owner.display_name,array_agg(DISTINCT r.id ORDER BY r.id) AS repository_ids FROM users owner JOIN memberships om ON om.user_id=owner.id JOIN repositories r ON r.organization_id=om.organization_id JOIN memberships m ON m.organization_id=r.organization_id JOIN users u ON u.id=m.user_id WHERE m.user_id=$1 AND u.status='ACTIVE' AND owner.status='ACTIVE' AND om.role<>'READER' AND r.id=ANY($2) GROUP BY owner.id,owner.display_name ORDER BY owner.display_name,owner.id LIMIT 201")
        .bind(actor.user_id).bind(&ids).fetch_all(&state.database);
    let (teams, owners) = tokio::join!(teams, owners);
    match (teams, owners) {
        (Ok(teams), Ok(owners)) if teams.len() <= 200 && owners.len() <= 200 => {
            Json(ProposalChoices {
                repositories,
                teams,
                owners,
            })
            .into_response()
        }
        (Ok(_), Ok(_)) => choices_limit(),
        _ => failure(ProposalError::Unavailable),
    }
}
fn choices_limit() -> Response {
    problem(
        StatusCode::UNPROCESSABLE_ENTITY,
        "CHOICES_LIMIT",
        "יש לצמצם את הבחירה למאגר אחד או לעדכן את רשימות הצוותים והבעלים.",
        false,
        Some(serde_json::json!({"max_choices":200})),
    )
}
