use async_trait::async_trait;
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use http_body_util::BodyExt;
use specspace_api::{AppState, router};
use specspace_application::{book::BookError, reader::BookProviderFactory};
use specspace_domain::{BranchName, RepositoryPath};
use specspace_git_adapter::{
    CommitMetadata, CommitRequest, CommitSha, FileEntry, GitError, GitProvider,
};
use specspace_infrastructure::{index_worker::IndexWorker, sessions::SessionStore};
use sqlx::PgPool;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, AtomicUsize, Ordering},
    },
};
use tower::ServiceExt;
use uuid::Uuid;

const OLD: &str = "1111111111111111111111111111111111111111";
const NEW: &str = "2222222222222222222222222222222222222222";
const ASSET: &str = "5e3d382db4dd83d59aa5742793ad6b7903409e865c83bcbc54835049f043bc15.png";

struct Provider {
    files: HashMap<String, String>,
    version: AtomicU8,
    reads: AtomicUsize,
    asset_reads: AtomicUsize,
    asset_corrupt: AtomicU8,
    revoke_after_asset: Mutex<Option<(PgPool, Uuid)>>,
    revoke_editor_after_text: Mutex<Option<(PgPool, Uuid)>>,
}

#[async_trait]
impl GitProvider for Provider {
    async fn read_bytes(
        &self,
        commit: &CommitSha,
        path: &RepositoryPath,
        max: usize,
    ) -> Result<Vec<u8>, GitError> {
        self.asset_reads.fetch_add(1, Ordering::SeqCst);
        assert_eq!(commit.as_str(), OLD);
        assert_eq!(max, 5 * 1024 * 1024);
        if path.as_str() != format!("assets/{ASSET}") {
            return Err(GitError::FileMissing);
        }
        let bytes = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../fixtures/repository/assets")
                .join(ASSET),
        )
        .unwrap();
        let revoke = self.revoke_after_asset.lock().unwrap().take();
        if let Some((pool, user)) = revoke {
            sqlx::query("UPDATE users SET status = 'DISABLED' WHERE id = $1")
                .bind(user)
                .execute(&pool)
                .await
                .unwrap();
        }
        match self.asset_corrupt.load(Ordering::SeqCst) {
            1 => Ok(b"corrupt image".to_vec()),
            2 => Err(GitError::ResponseTooLarge),
            _ => Ok(bytes),
        }
    }
    async fn main_head(&self) -> Result<CommitSha, GitError> {
        CommitSha::parse(if self.version.load(Ordering::SeqCst) == 0 {
            OLD
        } else {
            NEW
        })
    }
    async fn branch_head(&self, _: &BranchName) -> Result<CommitSha, GitError> {
        Err(GitError::ProviderRejected)
    }
    async fn create_branch(&self, _: &BranchName, _: &CommitSha) -> Result<CommitSha, GitError> {
        Err(GitError::ProviderRejected)
    }
    async fn commit(&self, _: CommitRequest<'_>) -> Result<CommitSha, GitError> {
        Err(GitError::ProviderRejected)
    }
    async fn read_text(
        &self,
        commit: &CommitSha,
        path: &RepositoryPath,
    ) -> Result<String, GitError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        let revoke = self.revoke_editor_after_text.lock().unwrap().take();
        if let Some((pool, actor)) = revoke {
            sqlx::query("UPDATE memberships SET role='READER' WHERE user_id=$1")
                .bind(actor)
                .execute(&pool)
                .await
                .unwrap();
        }
        let source = self
            .files
            .get(path.as_str())
            .ok_or(GitError::InvalidResponse)?;
        Ok(
            if commit.as_str() == NEW && path.as_str() == "docs/permissions.md" {
                source.replace("מחיקה", "צפייה")
            } else {
                source.clone()
            },
        )
    }
    async fn list_files(&self, _: &CommitSha) -> Result<Vec<FileEntry>, GitError> {
        Ok(self
            .files
            .iter()
            .map(|(path, text)| FileEntry {
                path: RepositoryPath::parse(path).unwrap(),
                blob_sha: CommitSha::parse(OLD).unwrap(),
                size: text.len() as u64,
            })
            .collect())
    }
    async fn commit_receipt(
        &self,
        _: &specspace_git_adapter::CommitSha,
    ) -> Result<specspace_git_adapter::CommitReceipt, specspace_git_adapter::GitError> {
        Err(specspace_git_adapter::GitError::ProviderRejected)
    }
    async fn commit_metadata(&self, commit: &CommitSha) -> Result<CommitMetadata, GitError> {
        Ok(CommitMetadata {
            sha: commit.clone(),
            committed_at: if commit.as_str() == OLD {
                "2026-11-14T10:00:00Z"
            } else {
                "2026-11-15T10:00:00Z"
            }
            .parse()
            .unwrap(),
        })
    }
    async fn is_ancestor(
        &self,
        ancestor: &CommitSha,
        descendant: &CommitSha,
    ) -> Result<bool, GitError> {
        Ok(ancestor == descendant || (ancestor.as_str() == OLD && descendant.as_str() == NEW))
    }
}

struct Source(Arc<Provider>);

#[async_trait]
impl BookProviderFactory for Source {
    async fn provider(&self, _: Uuid) -> Result<Arc<dyn GitProvider>, BookError> {
        Ok(self.0.clone())
    }
}

async fn setup(pool: &PgPool) -> (Uuid, Uuid, Uuid, String) {
    let actor = Uuid::new_v4();
    let organization = Uuid::new_v4();
    let repository = Uuid::new_v4();
    let foreign = Uuid::new_v4();
    let foreign_repo = Uuid::new_v4();
    sqlx::query("INSERT INTO organizations (id, name) VALUES ($1, 'Synthetic book'), ($2, 'Other organization')").bind(organization).bind(foreign).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO users (id, issuer, external_subject, display_name) VALUES ($1, 'fixture', $1::text, 'Synthetic reader')").bind(actor).execute(pool).await.unwrap();
    sqlx::query(
        "INSERT INTO memberships (organization_id, user_id, role) VALUES ($1, $2, 'READER')",
    )
    .bind(organization)
    .bind(actor)
    .execute(pool)
    .await
    .unwrap();
    for (id, org) in [(repository, organization), (foreign_repo, foreign)] {
        sqlx::query("INSERT INTO repositories (id, organization_id, provider, remote_id, installation_id) VALUES ($1, $2, 'GITHUB', '42', '7')").bind(id).bind(org).execute(pool).await.unwrap();
    }
    let credentials = SessionStore::new(pool.clone())
        .mint(actor, 3600)
        .await
        .unwrap();
    (
        actor,
        repository,
        foreign_repo,
        credentials.session_cookie_value().into(),
    )
}

fn fixture(pool: &PgPool) -> (Router, Arc<Provider>, IndexWorker) {
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../fixtures/repository");
    let files = [
        ".specspace/catalog.json",
        "docs/overview.md",
        "docs/permissions.md",
        "docs/media.md",
        "docs/recordings.md",
        "docs/player.md",
        "docs/operations.md",
    ]
    .into_iter()
    .map(|path| {
        (
            path.into(),
            std::fs::read_to_string(root.join(path)).unwrap(),
        )
    })
    .collect();
    fixture_with_files(pool, files)
}

fn fixture_with_files(
    pool: &PgPool,
    files: HashMap<String, String>,
) -> (Router, Arc<Provider>, IndexWorker) {
    let provider = Arc::new(Provider {
        files,
        version: AtomicU8::new(0),
        reads: AtomicUsize::new(0),
        asset_reads: AtomicUsize::new(0),
        asset_corrupt: AtomicU8::new(0),
        revoke_after_asset: Mutex::new(None),
        revoke_editor_after_text: Mutex::new(None),
    });
    let mut state = AppState::new(pool.clone());
    state.cookies =
        specspace_api::auth::CookiePolicy::new("https://specspace.example", false).unwrap();
    state.configure_book_source(Arc::new(Source(provider.clone())));
    let worker = state
        .index_worker(std::time::Duration::from_secs(30))
        .unwrap();
    (router(state), provider, worker)
}

async fn response(app: &Router, token: &str, uri: &str) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .header("Cookie", format!("__Host-specspace-session={token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn get(app: &Router, token: &str, uri: &str) -> (StatusCode, serde_json::Value) {
    let response = response(app, token, uri).await;
    let status = response.status();
    let value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    (status, value)
}

fn search(repository: Uuid, commit: &str, cursor: Option<&str>) -> String {
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query
        .append_pair("q", "מחיקה")
        .append_pair("commit", commit)
        .append_pair("limit", "2");
    if let Some(cursor) = cursor {
        query.append_pair("cursor", cursor);
    }
    format!("/api/v1/repositories/{repository}/find?{}", query.finish())
}

fn highlight(repository: Uuid, commit: &str, document: &str, windows: serde_json::Value) -> String {
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query
        .append_pair("q", "מחיקה")
        .append_pair("commit", commit)
        .append_pair(
            "windows",
            &URL_SAFE_NO_PAD.encode(serde_json::to_vec(&windows).unwrap()),
        );
    format!(
        "/api/v1/repositories/{repository}/documents/{document}/highlights?{}",
        query.finish()
    )
}

async fn proposal(pool: &PgPool, actor: Uuid, repository: Uuid, head: Option<&str>) -> Uuid {
    let id = Uuid::new_v4();
    let team = Uuid::new_v4();
    sqlx::query("INSERT INTO teams(id,organization_id,name) SELECT $1,organization_id,$1::text FROM repositories WHERE id=$2")
        .bind(team).bind(repository).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO proposals(id,organization_id,repository_id,title,branch,owner_id,author_id,team_id,description,ui_impact,state,base_sha,head_sha,target_sha) SELECT $1,organization_id,id,'Synthetic proposal',$3,$4,$4,$5,'Synthetic read-only proposal','UNDECIDED',CASE WHEN $6::text IS NULL THEN 'PROVISIONING' ELSE 'DRAFT' END,$7,$6,$7 FROM repositories WHERE id=$2")
        .bind(id).bind(repository).bind(format!("change/{id}")).bind(actor).bind(team).bind(head).bind(OLD).execute(pool).await.unwrap();
    id
}

fn proposal_uri(uri: String, repository: Uuid, proposal: Uuid) -> String {
    uri.replace(
        &format!("repositories/{repository}"),
        &format!("proposals/{proposal}"),
    )
}

async fn editor_model(
    app: &Router,
    token: &str,
    proposal: Uuid,
    document: &str,
    commit: &str,
) -> (StatusCode, serde_json::Value) {
    get(
        app,
        token,
        &format!("/api/v1/proposals/{proposal}/documents/{document}/editor?commit={commit}"),
    )
    .await
}
fn keep_edit(model: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({"expected_sha256":model["sha256"],"blocks":model["blocks"].as_array().unwrap().iter().map(|block|serde_json::json!({"action":"keep","key":block["key"]})).collect::<Vec<_>>(),"deleted_keys":[],"deleted_section_ids":[]})
}
async fn editor_preview(
    pool: &PgPool,
    app: &Router,
    actor: Uuid,
    proposal: Uuid,
    document: &str,
    body: &serde_json::Value,
    csrf: bool,
) -> (StatusCode, serde_json::Value) {
    editor_post(
        pool,
        app,
        actor,
        &format!("/api/v1/proposals/{proposal}/documents/{document}/editor/preview"),
        body,
        csrf,
    )
    .await
}
async fn editor_post(
    pool: &PgPool,
    app: &Router,
    actor: Uuid,
    uri: &str,
    body: &serde_json::Value,
    csrf: bool,
) -> (StatusCode, serde_json::Value) {
    let credentials = SessionStore::new(pool.clone())
        .mint(actor, 3600)
        .await
        .unwrap();
    let mut request = Request::builder()
        .method("POST")
        .uri(uri)
        .header(
            "Cookie",
            format!(
                "__Host-specspace-session={}",
                credentials.session_cookie_value()
            ),
        )
        .header("Content-Type", "application/json");
    if csrf {
        request = request
            .header("Origin", "https://specspace.example")
            .header("X-XSRF-TOKEN", credentials.csrf_cookie_value());
    }
    let response = app
        .clone()
        .oneshot(
            request
                .body(Body::from(serde_json::to_vec(body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    assert_eq!(response.headers()["cache-control"], "no-store");
    (
        status,
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap(),
    )
}

#[sqlx::test(migrations = "../../migrations")]
async fn editor_model_and_preview_keep_exact_source_without_claiming_a_git_save(pool: PgPool) {
    let (actor, repository, _, token) = setup(&pool).await;
    let id = proposal(&pool, actor, repository, Some(OLD)).await;
    let (app, provider, _) = fixture(&pool);
    let (status, readonly) = editor_model(&app, &token, id, "DOC-OVERVIEW", OLD).await;
    assert_eq!(status, StatusCode::OK, "{readonly}");
    assert_eq!(
        readonly["can_edit"], false,
        "Reader role remains readonly even if it owns a fixture proposal"
    );
    sqlx::query("UPDATE memberships SET role='CONTRIBUTOR' WHERE user_id=$1")
        .bind(actor)
        .execute(&pool)
        .await
        .unwrap();
    let (_, model) = editor_model(&app, &token, id, "DOC-OVERVIEW", OLD).await;
    assert_eq!(model["can_edit"], true);
    assert_eq!(model["snapshot_sha"], OLD);
    assert_eq!(model["recorded_head_sha"], OLD);
    assert_eq!(model["branch"], format!("change/{id}"));
    assert_eq!(model["target_branch"], "main");
    let original = &provider.files["docs/overview.md"];
    let base = &model["model"];
    let keep = keep_edit(base);
    let no_op = serde_json::json!({"commit":OLD,"expected_proposal_version":1,"edit":keep});
    let (status, unchanged) =
        editor_preview(&pool, &app, actor, id, "DOC-OVERVIEW", &no_op, true).await;
    assert_eq!(status, StatusCode::OK, "{unchanged}");
    assert_eq!(unchanged["changed"], false);
    assert_eq!(unchanged["markdown"], *original);
    assert_eq!(unchanged["sha256"], base["sha256"]);
    assert!(
        unchanged.get("snapshot_sha").is_none(),
        "uncommitted preview must not claim a new Git snapshot"
    );
    let blocks = base["blocks"].as_array().unwrap();
    let index = blocks
        .iter()
        .position(|block| block["kind"] == "heading")
        .unwrap();
    let replacement = "## כותרת חדשה **מודגשת**\n";
    let mut edit = keep_edit(base);
    edit["blocks"][index] =
        serde_json::json!({"action":"replace","key":blocks[index]["key"],"markdown":replacement});
    let (status, preview) = editor_preview(
        &pool,
        &app,
        actor,
        id,
        "DOC-OVERVIEW",
        &serde_json::json!({"commit":OLD,"expected_proposal_version":1,"edit":edit}),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    let mut expected = original.clone();
    expected.replace_range(
        blocks[index]["source_start_byte"].as_u64().unwrap() as usize
            ..blocks[index]["source_end_byte"].as_u64().unwrap() as usize,
        replacement,
    );
    assert_eq!(preview["markdown"], expected);
    assert_eq!(preview["changed"], true);
    assert_eq!(preview["base_snapshot_sha"], OLD);
    assert_eq!(preview["removed_section_ids"], serde_json::json!([]));
    assert!(
        preview["render_events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["text"] == "כותרת חדשה ")
    );
    let saved:(String,i64,i64)=sqlx::query_as("SELECT p.head_sha,p.version,(SELECT count(*) FROM git_operations) FROM proposals p WHERE id=$1").bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(saved, (OLD.into(), 1, 0));
}

#[sqlx::test(migrations = "../../migrations")]
async fn editor_preview_checks_scope_csrf_roles_versions_and_historical_readonly_state(
    pool: PgPool,
) {
    let (actor, repository, _, token) = setup(&pool).await;
    let id = proposal(&pool, actor, repository, Some(NEW)).await;
    let (app, provider, _) = fixture(&pool);
    sqlx::query("UPDATE memberships SET role='CONTRIBUTOR' WHERE user_id=$1")
        .bind(actor)
        .execute(&pool)
        .await
        .unwrap();
    let (_, historical) = editor_model(&app, &token, id, "DOC-PERMISSIONS", OLD).await;
    assert_eq!(historical["snapshot_sha"], OLD);
    assert_eq!(historical["recorded_head_sha"], NEW);
    assert_eq!(historical["can_edit"], false);
    let (_, current) = editor_model(&app, &token, id, "DOC-PERMISSIONS", NEW).await;
    let body = serde_json::json!({"commit":NEW,"expected_proposal_version":1,"edit":keep_edit(&current["model"])});
    let reads = provider.reads.load(Ordering::SeqCst);
    assert_eq!(
        editor_preview(&pool, &app, actor, id, "DOC-PERMISSIONS", &body, false)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let mut stale = body.clone();
    stale["expected_proposal_version"] = serde_json::json!(2);
    assert_eq!(
        editor_preview(&pool, &app, actor, id, "DOC-PERMISSIONS", &stale, true)
            .await
            .1["error"]["code"],
        "EDITOR_VERSION_CHANGED"
    );
    stale["expected_proposal_version"] = serde_json::json!(1);
    stale["commit"] = serde_json::json!(OLD);
    assert_eq!(
        editor_preview(&pool, &app, actor, id, "DOC-PERMISSIONS", &stale, true)
            .await
            .1["error"]["code"],
        "EDITOR_HEAD_CHANGED"
    );
    assert_eq!(
        editor_preview(
            &pool,
            &app,
            actor,
            Uuid::new_v4(),
            "DOC-PERMISSIONS",
            &body,
            true
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    sqlx::query("UPDATE memberships SET role='READER' WHERE user_id=$1")
        .bind(actor)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        editor_preview(&pool, &app, actor, id, "DOC-PERMISSIONS", &body, true)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        provider.reads.load(Ordering::SeqCst),
        reads,
        "denied or stale previews must not consult provider content"
    );
    sqlx::query("UPDATE memberships SET role='CONTRIBUTOR' WHERE user_id=$1")
        .bind(actor)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE proposals SET state='CLOSED' WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        editor_model(&app, &token, id, "DOC-PERMISSIONS", NEW)
            .await
            .1["can_edit"],
        false
    );
    assert_eq!(
        editor_preview(&pool, &app, actor, id, "DOC-PERMISSIONS", &body, true)
            .await
            .1["error"]["code"],
        "PROPOSAL_NOT_EDITABLE"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn editor_preview_protects_unsupported_source_and_rechecks_permission_after_provider_await(
    pool: PgPool,
) {
    let (actor, repository, _, token) = setup(&pool).await;
    sqlx::query("UPDATE memberships SET role='CONTRIBUTOR' WHERE user_id=$1")
        .bind(actor)
        .execute(&pool)
        .await
        .unwrap();
    let id = proposal(&pool, actor, repository, Some(OLD)).await;
    let (app, provider, _) = fixture(&pool);
    let model =
        specspace_document_engine::editing::parse(&provider.files["docs/overview.md"]).unwrap();
    let body =
        serde_json::json!({"commit":OLD,"expected_proposal_version":1,"edit":model.unchanged()});
    *provider.revoke_editor_after_text.lock().unwrap() = Some((pool.clone(), actor));
    let (status, denied) =
        editor_preview(&pool, &app, actor, id, "DOC-OVERVIEW", &body, true).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{denied}");
    assert!(denied.get("markdown").is_none());
    sqlx::query("UPDATE memberships SET role='CONTRIBUTOR' WHERE user_id=$1")
        .bind(actor)
        .execute(&pool)
        .await
        .unwrap();
    let mut files = provider.files.clone();
    files
        .get_mut("docs/overview.md")
        .unwrap()
        .push_str("\n<script>private()</script>\n");
    let (app, _, _) = fixture_with_files(&pool, files);
    let (status, model) = editor_model(&app, &token, id, "DOC-OVERVIEW", OLD).await;
    assert_eq!(status, StatusCode::OK);
    let base = &model["model"];
    let blocks = base["blocks"].as_array().unwrap();
    let index = blocks
        .iter()
        .position(|block| !block["protected_reason"].is_null())
        .unwrap();
    let mut edit = keep_edit(base);
    edit["blocks"][index] = serde_json::json!({"action":"replace","key":blocks[index]["key"],"markdown":"lost script source\n"});
    let (status, error) = editor_preview(
        &pool,
        &app,
        actor,
        id,
        "DOC-OVERVIEW",
        &serde_json::json!({"commit":OLD,"expected_proposal_version":1,"edit":edit}),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error["error"]["code"], "PROTECTED_EDITOR_BLOCK");
    let (_, preview) = editor_preview(
        &pool,
        &app,
        actor,
        id,
        "DOC-OVERVIEW",
        &serde_json::json!({"commit":OLD,"expected_proposal_version":1,"edit":keep_edit(base)}),
        true,
    )
    .await;
    assert!(
        preview["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| finding["code"] == "UNSUPPORTED_HTML")
    );
    assert!(
        preview["render_events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["kind"] == "unsupported")
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn editor_preview_requires_current_edit_assignment_and_rejects_foreign_documents(
    pool: PgPool,
) {
    let (owner, repository, _, token) = setup(&pool).await;
    sqlx::query("UPDATE memberships SET role='CONTRIBUTOR' WHERE user_id=$1")
        .bind(owner)
        .execute(&pool)
        .await
        .unwrap();
    let id = proposal(&pool, owner, repository, Some(OLD)).await;
    let (peer, foreign_repo, _, peer_token) = setup(&pool).await;
    let foreign = proposal(&pool, peer, foreign_repo, Some(OLD)).await;
    sqlx::query("INSERT INTO memberships(organization_id,user_id,role) SELECT organization_id,$2,'CONTRIBUTOR' FROM repositories WHERE id=$1")
        .bind(repository).bind(peer).execute(&pool).await.unwrap();
    let (app, provider, _) = fixture(&pool);
    let (_, model) = editor_model(&app, &token, id, "DOC-OVERVIEW", OLD).await;
    let body = serde_json::json!({"commit":OLD,"expected_proposal_version":1,"edit":keep_edit(&model["model"])});
    assert_eq!(
        editor_model(&app, &peer_token, id, "DOC-OVERVIEW", OLD)
            .await
            .1["can_edit"],
        false
    );
    assert_eq!(
        editor_preview(&pool, &app, peer, id, "DOC-OVERVIEW", &body, true)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    sqlx::query("INSERT INTO proposal_collaborators(organization_id,proposal_id,user_id,access) SELECT organization_id,id,$2,'EDIT' FROM proposals WHERE id=$1")
        .bind(id).bind(peer).execute(&pool).await.unwrap();
    assert_eq!(
        editor_preview(&pool, &app, peer, id, "DOC-OVERVIEW", &body, true)
            .await
            .0,
        StatusCode::OK
    );
    sqlx::query(
        "UPDATE proposal_collaborators SET access='READ' WHERE proposal_id=$1 AND user_id=$2",
    )
    .bind(id)
    .bind(peer)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        editor_preview(&pool, &app, peer, id, "DOC-OVERVIEW", &body, true)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let reads = provider.reads.load(Ordering::SeqCst);
    assert_eq!(
        editor_model(&app, &token, foreign, "DOC-OVERVIEW", OLD)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        editor_preview(&pool, &app, owner, foreign, "DOC-OVERVIEW", &body, true)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(provider.reads.load(Ordering::SeqCst), reads);
    let mut wrong_document = body.clone();
    wrong_document["edit"]["expected_sha256"] = serde_json::json!("a".repeat(64));
    let (status, error) = editor_preview(
        &pool,
        &app,
        owner,
        id,
        "DOC-OVERVIEW",
        &wrong_document,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "EDITOR_BASE_MISMATCH");
}

#[sqlx::test(migrations = "../../migrations")]
async fn editor_rejects_catalog_and_front_matter_identity_mismatch_before_editing(pool: PgPool) {
    let (actor, repository, _, token) = setup(&pool).await;
    let id = proposal(&pool, actor, repository, Some(OLD)).await;
    let (_, provider, _) = fixture(&pool);
    let mut files = provider.files.clone();
    let original = files.get_mut("docs/overview.md").unwrap();
    *original = original.replacen("document_id: DOC-OVERVIEW", "document_id: DOC-OTHER", 1);
    let (app, _, _) = fixture_with_files(&pool, files);
    let (status, error) = editor_model(&app, &token, id, "DOC-OVERVIEW", OLD).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error["error"]["code"], "EDITOR_DOCUMENT_ID_MISMATCH");
}

async fn editor_source(
    app: &Router,
    token: &str,
    proposal: Uuid,
    commit: &str,
) -> (StatusCode, serde_json::Value) {
    get(
        app,
        token,
        &format!(
            "/api/v1/proposals/{proposal}/documents/DOC-OVERVIEW/editor/source?commit={commit}"
        ),
    )
    .await
}
fn source_request(commit: &str, source: &serde_json::Value, markdown: &str) -> serde_json::Value {
    serde_json::json!({"commit":commit,"expected_proposal_version":1,"expected_document_sha256":source["sha256"],"markdown":markdown})
}
async fn source_preview(
    pool: &PgPool,
    app: &Router,
    actor: Uuid,
    proposal: Uuid,
    body: &serde_json::Value,
    csrf: bool,
) -> (StatusCode, serde_json::Value) {
    editor_post(
        pool,
        app,
        actor,
        &format!("/api/v1/proposals/{proposal}/documents/DOC-OVERVIEW/editor/source-preview"),
        body,
        csrf,
    )
    .await
}

#[sqlx::test(migrations = "../../migrations")]
async fn source_mode_loads_broken_bytes_and_previews_repairs_and_unfinished_recovery_without_git_writes(
    pool: PgPool,
) {
    let (actor, repository, _, token) = setup(&pool).await;
    sqlx::query("UPDATE memberships SET role='CONTRIBUTOR' WHERE user_id=$1")
        .bind(actor)
        .execute(&pool)
        .await
        .unwrap();
    let id = proposal(&pool, actor, repository, Some(OLD)).await;
    let (_, provider, _) = fixture(&pool);
    let valid = provider.files["docs/overview.md"].clone();
    let broken = "unfinished source ** שָׁלוֹם\r\n";
    let mut files = provider.files.clone();
    files.insert("docs/overview.md".into(), broken.into());
    let (app, _, _) = fixture_with_files(&pool, files);
    let (status, source) = editor_source(&app, &token, id, OLD).await;
    assert_eq!(status, StatusCode::OK, "{source}");
    assert_eq!(source["markdown"], broken);
    assert_eq!(source["parse_error"], "FRONT_MATTER_REQUIRED");
    assert!(source["model"].is_null());
    assert_eq!(source["can_edit"], true);
    let (status, repaired) = source_preview(
        &pool,
        &app,
        actor,
        id,
        &source_request(OLD, &source, &valid),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{repaired}");
    assert!(repaired["parse_error"].is_null());
    assert_eq!(repaired["base_parse_error"], "FRONT_MATTER_REQUIRED");
    assert_eq!(repaired["model"]["document_id"], "DOC-OVERVIEW");
    assert!(
        repaired["preview"]["removed_section_ids"].is_null(),
        "invalid original source makes removed-ID comparison unknown"
    );
    assert_eq!(
        repaired["preview"]["base_document_sha256"],
        source["sha256"]
    );
    assert_eq!(repaired["preview"]["markdown"], valid);
    let unfinished = "---\ndocument_id: unfinished";
    let (status, draft) = source_preview(
        &pool,
        &app,
        actor,
        id,
        &source_request(OLD, &source, unfinished),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(draft["preview"]["markdown"], unfinished);
    assert!(!draft["parse_error"].is_null());
    assert!(draft["model"].is_null());
    assert_eq!(draft["preview"]["findings_total"], 1);
    let mismatched = valid.replacen("DOC-OVERVIEW", "DOC-OTHER", 1);
    let (_, identity) = source_preview(
        &pool,
        &app,
        actor,
        id,
        &source_request(OLD, &source, &mismatched),
        true,
    )
    .await;
    assert_eq!(identity["model_error"], "EDITOR_DOCUMENT_ID_MISMATCH");
    assert!(identity["model"].is_null());
    let unchanged:(String,i64,i64,i64)=sqlx::query_as("SELECT p.head_sha,p.version,(SELECT count(*) FROM git_operations),(SELECT count(*) FROM draft_buffers) FROM proposals p WHERE id=$1")
        .bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(unchanged, (OLD.into(), 1, 0, 0));
}

#[sqlx::test(migrations = "../../migrations")]
async fn source_recovery_comparison_is_readonly_for_historical_or_closed_bases_and_rechecks_scope(
    pool: PgPool,
) {
    let (actor, repository, _, token) = setup(&pool).await;
    sqlx::query("UPDATE memberships SET role='CONTRIBUTOR' WHERE user_id=$1")
        .bind(actor)
        .execute(&pool)
        .await
        .unwrap();
    let id = proposal(&pool, actor, repository, Some(NEW)).await;
    let (app, provider, _) = fixture(&pool);
    let (_, old) = editor_source(&app, &token, id, OLD).await;
    assert_eq!(old["can_edit"], false);
    assert_eq!(old["recorded_head_sha"], NEW);
    let body = source_request(
        OLD,
        &old,
        &format!(
            "{}\nLocal recovered paragraph.\n",
            old["markdown"].as_str().unwrap()
        ),
    );
    let (status, compared) = source_preview(&pool, &app, actor, id, &body, true).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(compared["can_edit"], false);
    assert_eq!(compared["preview"]["base_snapshot_sha"], OLD);
    sqlx::query("UPDATE proposals SET state='CLOSED' WHERE id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let (_, closed) = editor_source(&app, &token, id, NEW).await;
    assert_eq!(closed["can_edit"], false);
    let body = source_request(NEW, &closed, closed["markdown"].as_str().unwrap());
    assert_eq!(
        source_preview(&pool, &app, actor, id, &body, true).await.1["can_edit"],
        false
    );
    assert_eq!(
        source_preview(&pool, &app, actor, id, &body, false).await.0,
        StatusCode::FORBIDDEN
    );
    let mut stale = body.clone();
    stale["expected_proposal_version"] = serde_json::json!(2);
    assert_eq!(
        source_preview(&pool, &app, actor, id, &stale, true).await.1["error"]["code"],
        "EDITOR_VERSION_CHANGED"
    );
    stale = body.clone();
    stale["expected_document_sha256"] = serde_json::json!("a".repeat(64));
    assert_eq!(
        source_preview(&pool, &app, actor, id, &stale, true).await.1["error"]["code"],
        "EDITOR_BASE_MISMATCH"
    );
    let (foreign_actor, foreign_repository, _, _) = setup(&pool).await;
    let foreign = proposal(&pool, foreign_actor, foreign_repository, Some(OLD)).await;
    let reads = provider.reads.load(Ordering::SeqCst);
    assert_eq!(
        editor_source(&app, &token, foreign, OLD).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        source_preview(&pool, &app, actor, foreign, &body, true)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(provider.reads.load(Ordering::SeqCst), reads);
    sqlx::query("UPDATE memberships SET role='READER' WHERE user_id=$1")
        .bind(actor)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        source_preview(&pool, &app, actor, id, &body, true).await.0,
        StatusCode::FORBIDDEN
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn source_mode_bounds_diagnostics_and_refuses_catalog_paths_outside_docs(pool: PgPool) {
    let (actor, repository, _, token) = setup(&pool).await;
    sqlx::query("UPDATE memberships SET role='CONTRIBUTOR' WHERE user_id=$1")
        .bind(actor)
        .execute(&pool)
        .await
        .unwrap();
    let id = proposal(&pool, actor, repository, Some(OLD)).await;
    let (_, provider, _) = fixture(&pool);
    let mut files = provider.files.clone();
    files
        .get_mut("docs/overview.md")
        .unwrap()
        .push_str(&format!(
            "\n{}\n",
            "text <span>unsupported</span> ".repeat(1100)
        ));
    let (app, _, _) = fixture_with_files(&pool, files);
    let (_, source) = editor_source(&app, &token, id, OLD).await;
    assert!(source["findings_total"].as_u64().unwrap() > 1000);
    assert_eq!(source["findings"].as_array().unwrap().len(), 1000);
    assert_eq!(source["findings_truncated"], true);
    let (_, preview) = source_preview(
        &pool,
        &app,
        actor,
        id,
        &source_request(OLD, &source, source["markdown"].as_str().unwrap()),
        true,
    )
    .await;
    assert!(preview["preview"]["findings_total"].as_u64().unwrap() > 1000);
    assert_eq!(
        preview["preview"]["findings"].as_array().unwrap().len(),
        1000
    );
    let mut files = provider.files.clone();
    let mut catalog: serde_json::Value =
        serde_json::from_str(&files[".specspace/catalog.json"]).unwrap();
    catalog["documents"][0]["path"] = serde_json::json!(".specspace/private.txt");
    files.insert(
        ".specspace/catalog.json".into(),
        serde_json::to_string(&catalog).unwrap(),
    );
    files.insert(
        ".specspace/private.txt".into(),
        "outside-docs-secret-fixture".into(),
    );
    let (app, provider, _) = fixture_with_files(&pool, files);
    let book = get(
        &app,
        &token,
        &format!("/api/v1/proposals/{id}/specification"),
    )
    .await;
    assert_eq!(book.0, StatusCode::OK);
    let reads = provider.reads.load(Ordering::SeqCst);
    let (status, error) = editor_source(&app, &token, id, OLD).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!error.to_string().contains("outside-docs-secret-fixture"));
    assert_eq!(provider.reads.load(Ordering::SeqCst), reads);
}

#[sqlx::test(migrations = "../../migrations")]
async fn source_repair_rechecks_permissions_after_fetching_an_unparseable_file(pool: PgPool) {
    let (actor, repository, _, token) = setup(&pool).await;
    sqlx::query("UPDATE memberships SET role='CONTRIBUTOR' WHERE user_id=$1")
        .bind(actor)
        .execute(&pool)
        .await
        .unwrap();
    let id = proposal(&pool, actor, repository, Some(OLD)).await;
    let (_, provider, _) = fixture(&pool);
    let mut files = provider.files.clone();
    files.insert("docs/overview.md".into(), "unfinished source".into());
    let (app, provider, _) = fixture_with_files(&pool, files);
    let (_, source) = editor_source(&app, &token, id, OLD).await;
    *provider.revoke_editor_after_text.lock().unwrap() = Some((pool.clone(), actor));
    let (status, error) = source_preview(
        &pool,
        &app,
        actor,
        id,
        &source_request(OLD, &source, "still unfinished"),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(error.get("preview").is_none());
}

#[sqlx::test(migrations = "../../migrations")]
async fn proposal_book_keeps_private_versions_search_and_cache_out_of_main(pool: PgPool) {
    let (actor, repository, _, token) = setup(&pool).await;
    let private = proposal(&pool, actor, repository, Some(NEW)).await;
    let other = proposal(&pool, actor, repository, Some(NEW)).await;
    let (app, provider, _) = fixture(&pool);
    let (status, book) = get(
        &app,
        &token,
        &format!("/api/v1/proposals/{private}/specification"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(book["branch"], format!("change/{private}"));
    assert_eq!(book["repository_id"], repository.to_string());
    assert_eq!(book["snapshot_sha"], NEW);
    assert_eq!(book["version_selected"], false);
    assert_eq!(book["documents"].as_array().unwrap().len(), 6);
    let doc_uri = format!("/api/v1/proposals/{private}/documents/DOC-PERMISSIONS?commit={NEW}");
    let reads = provider.reads.load(Ordering::SeqCst);
    let (status, document) = get(&app, &token, &doc_uri).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(document["snapshot_sha"], NEW);
    assert!(document["markdown"].as_str().unwrap().contains("צפייה"));
    assert!(!document["markdown"].as_str().unwrap().contains("מחיקה"));
    assert_eq!(
        provider.reads.load(Ordering::SeqCst),
        reads,
        "authorized proposal cache reused"
    );
    let (status, found) = get(
        &app,
        &token,
        &proposal_uri(search(repository, NEW, None), repository, private),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(found["total_matches"].as_u64().unwrap() < 4);
    let cursor = found["next_cursor"].as_str().unwrap();
    let (status, _) = get(
        &app,
        &token,
        &proposal_uri(search(repository, NEW, Some(cursor)), repository, other),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, history) = get(
        &app,
        &token,
        &format!("/api/v1/proposals/{private}/specification?commit={OLD}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(history["snapshot_sha"], OLD);
    assert_eq!(history["version_selected"], true);
    let (_, old_search) = get(
        &app,
        &token,
        &proposal_uri(search(repository, OLD, None), repository, private),
    )
    .await;
    assert_eq!(old_search["total_matches"], 4);
    let unknown = "3".repeat(40);
    assert_eq!(
        get(
            &app,
            &token,
            &format!("/api/v1/proposals/{private}/specification?commit={unknown}")
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    for uri in [
        format!("/api/v1/repositories/{repository}/specification?commit={NEW}"),
        search(repository, NEW, None),
    ] {
        assert_eq!(
            get(&app, &token, &uri).await.0,
            StatusCode::NOT_FOUND,
            "draft SHA cannot become approved through cache/index"
        );
    }
    let counts: (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM search_index_snapshots),(SELECT count(*) FROM jobs)",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        counts,
        (0, 0),
        "proposal reads must never enqueue approved indexing"
    );
    let uri = proposal_uri(
        highlight(repository, OLD, "DOC-PERMISSIONS", serde_json::json!([])),
        repository,
        private,
    );
    assert_eq!(get(&app, &token, &uri).await.0, StatusCode::OK);
}

#[sqlx::test(migrations = "../../migrations")]
async fn proposal_reader_rechecks_scope_for_cached_history_pending_closed_and_revoked_access(
    pool: PgPool,
) {
    let (owner, repository, _, _) = setup(&pool).await;
    let (viewer, _, _, token) = setup(&pool).await;
    sqlx::query("INSERT INTO memberships(organization_id,user_id,role) SELECT organization_id,$2,'READER' FROM repositories WHERE id=$1")
        .bind(repository).bind(viewer).execute(&pool).await.unwrap();
    let pending = proposal(&pool, owner, repository, None).await;
    let ready = proposal(&pool, owner, repository, Some(NEW)).await;
    let (app, provider, _) = fixture(&pool);
    let uri = format!("/api/v1/proposals/{ready}/specification?commit={NEW}");
    assert_eq!(get(&app, &token, &uri).await.0, StatusCode::OK);
    let reads = provider.reads.load(Ordering::SeqCst);
    let (status, pending_response) = get(
        &app,
        &token,
        &format!("/api/v1/proposals/{pending}/specification"),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(pending_response["error"]["code"], "PROPOSAL_NOT_READY");
    assert_eq!(provider.reads.load(Ordering::SeqCst), reads);
    sqlx::query("UPDATE proposals SET state='CLOSED' WHERE id=$1")
        .bind(ready)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        get(&app, &token, &uri).await.0,
        StatusCode::OK,
        "closed branch keeps readable history"
    );
    sqlx::query("UPDATE proposals SET head_sha=$2 WHERE id=$1")
        .bind(ready)
        .bind(OLD)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        get(&app, &token, &uri).await.0,
        StatusCode::NOT_FOUND,
        "a cached SHA must prove ancestry again after head changes"
    );
    let uri = format!("/api/v1/proposals/{ready}/specification?commit={OLD}");
    assert_eq!(get(&app, &token, &uri).await.0, StatusCode::OK);
    sqlx::query("DELETE FROM memberships WHERE user_id=$1 AND organization_id=(SELECT organization_id FROM repositories WHERE id=$2)")
        .bind(viewer).bind(repository).execute(&pool).await.unwrap();
    let reads = provider.reads.load(Ordering::SeqCst);
    assert_eq!(get(&app, &token, &uri).await.0, StatusCode::NOT_FOUND);
    assert_eq!(provider.reads.load(Ordering::SeqCst), reads);
    assert_eq!(
        get(
            &app,
            &token,
            &format!("/api/v1/proposals/{}/specification", Uuid::new_v4())
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn proposal_images_are_pinned_private_derivatives_and_recheck_revocation(pool: PgPool) {
    let (actor, repository, _, token) = setup(&pool).await;
    let id = proposal(&pool, actor, repository, Some(OLD)).await;
    let (app, provider, _) = fixture(&pool);
    let uri = format!("/api/v1/proposals/{id}/assets/{ASSET}?commit={OLD}");
    let image = response(&app, &token, &uri).await;
    assert_eq!(image.status(), StatusCode::OK);
    assert_eq!(image.headers()["content-type"], "image/png");
    assert_eq!(image.headers()["cache-control"], "no-store");
    assert!(
        image
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .starts_with(b"\x89PNG")
    );
    *provider.revoke_after_asset.lock().unwrap() = Some((pool.clone(), actor));
    assert_eq!(
        response(&app, &token, &uri).await.status(),
        StatusCode::NOT_FOUND,
        "revoked during provider await"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn indexed_find_reports_progress_then_survives_restart_and_keeps_failures_explicit(
    pool: PgPool,
) {
    let (_, repository, foreign, token) = setup(&pool).await;
    let (app, provider, worker) = fixture(&pool);
    let uri = search(repository, OLD, None);
    let pending = response(&app, &token, &uri).await;
    assert_eq!(pending.status(), StatusCode::CONFLICT);
    assert_eq!(pending.headers()["retry-after"], "2");
    let pending: serde_json::Value =
        serde_json::from_slice(&pending.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(pending["error"]["code"], "SEARCH_SNAPSHOT_NOT_READY");
    assert_eq!(pending["error"]["retryable"], true);
    assert_eq!(pending["error"]["details"]["snapshot_sha"], OLD);
    assert!(pending["total_matches"].is_null());
    let (_, again) = get(&app, &token, &uri).await;
    assert_eq!(
        pending["error"]["details"]["job_id"],
        again["error"]["details"]["job_id"]
    );
    let reads = provider.reads.load(Ordering::SeqCst);
    assert_eq!(
        get(&app, &token, &search(foreign, OLD, None)).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(provider.reads.load(Ordering::SeqCst), reads);
    assert!(matches!(
        worker.run_once().await.unwrap(),
        specspace_infrastructure::index_worker::WorkResult::Completed(_)
    ));
    // A fresh process needs no Git round-trip for a fully published search.
    let restarted = router(AppState::new(pool.clone()));
    let (status, ready) = get(&restarted, &token, &uri).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ready["total_matches"], 4);
    assert_eq!(ready["snapshot_sha"], OLD);
    provider.version.store(1, Ordering::SeqCst);
    let uri = search(repository, NEW, None);
    assert_eq!(get(&app, &token, &uri).await.0, StatusCode::CONFLICT);
    sqlx::query("UPDATE jobs SET state='FAILED' WHERE payload->>'snapshot_sha'=$1")
        .bind(NEW)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE search_index_snapshots SET state='FAILED',error_code='SOURCE_NOT_AVAILABLE' WHERE repository_id=$1 AND snapshot_sha=$2").bind(repository).bind(NEW).execute(&pool).await.unwrap();
    let (status, failed) = get(&app, &token, &uri).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(failed["error"]["code"], "SEARCH_SNAPSHOT_FAILED");
    assert_eq!(failed["error"]["retryable"], false);
    assert!(failed["total_matches"].is_null());
    assert_eq!(
        get(&restarted, &token, &search(repository, OLD, None))
            .await
            .0,
        StatusCode::OK
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn visible_highlights_use_paged_identity_and_never_mix_versions_or_access_scopes(
    pool: PgPool,
) {
    let (actor, repository, foreign, token) = setup(&pool).await;
    let (app, provider, _worker) = fixture(&pool);
    assert_eq!(
        get(&app, &token, &search(repository, OLD, None)).await.0,
        StatusCode::CONFLICT
    );
    _worker.run_once().await.unwrap();
    let (_, page) = get(&app, &token, &search(repository, OLD, None)).await;
    let hit = page["matches"]
        .as_array()
        .unwrap()
        .iter()
        .find(|hit| hit["document_id"] == "DOC-PERMISSIONS")
        .unwrap();
    let windows = serde_json::json!([{ "block_id": hit["block_id"], "start_utf16": hit["start_utf16"].as_u64().unwrap() + 1,
        "end_utf16": hit["end_utf16"].as_u64().unwrap() - 1 }]);
    provider.version.store(1, Ordering::SeqCst);
    let uri = highlight(repository, OLD, "DOC-PERMISSIONS", windows.clone());
    let (status, visible) = get(&app, &token, &uri).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(visible["snapshot_sha"], OLD);
    assert_eq!(visible["matches"].as_array().unwrap().len(), 1);
    assert_eq!(visible["matches"][0]["match_id"], hit["match_id"]);
    assert_eq!(visible["matches"][0]["start_utf16"], hit["start_utf16"]);
    let (_, changed) = get(
        &app,
        &token,
        &highlight(repository, NEW, "DOC-PERMISSIONS", windows),
    )
    .await;
    assert_eq!(changed["matches"].as_array().unwrap().len(), 0);
    let malformed = serde_json::json!([{"block_id":"missing", "start_utf16":0, "end_utf16":1}]);
    let before = provider.reads.load(Ordering::SeqCst);
    assert_eq!(
        get(
            &app,
            &token,
            &highlight(foreign, OLD, "DOC-PERMISSIONS", malformed.clone())
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(provider.reads.load(Ordering::SeqCst), before);
    assert_eq!(get(&app, "", &uri).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(
        get(
            &app,
            &token,
            &highlight(repository, OLD, "DOC-PERMISSIONS", malformed)
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        get(
            &app,
            &token,
            &highlight(repository, OLD, "missing-document", serde_json::json!([]))
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let excessive = serde_json::json!(vec![
        serde_json::json!({"block_id":"block-0", "start_utf16":0, "end_utf16":1});
        65
    ]);
    let (status, error) = get(
        &app,
        &token,
        &highlight(repository, OLD, "DOC-PERMISSIONS", excessive),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"]["details"]["max_windows"], 64);
    sqlx::query("UPDATE users SET status = 'DISABLED' WHERE id = $1")
        .bind(actor)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(get(&app, &token, &uri).await.0, StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = "../../migrations")]
async fn book_document_and_search_pages_remain_pinned_when_main_advances(pool: PgPool) {
    let (_, repository, _, token) = setup(&pool).await;
    let (app, provider, _worker) = fixture(&pool);
    let (status, catalog) = get(
        &app,
        &token,
        &format!("/api/v1/repositories/{repository}/specification"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(catalog["snapshot_sha"], OLD);
    assert_eq!(catalog["documents"].as_array().unwrap().len(), 6);
    assert_eq!(catalog["documents"][0]["document_id"], "DOC-OVERVIEW");
    _worker.run_once().await.unwrap();
    let (_, first) = get(&app, &token, &search(repository, OLD, None)).await;
    assert_eq!(first["total_matches"], 4);
    assert_eq!(first["matches"].as_array().unwrap().len(), 2);
    let cursor = first["next_cursor"].as_str().unwrap();
    provider.version.store(1, Ordering::SeqCst);
    let (_, latest) = get(
        &app,
        &token,
        &format!("/api/v1/repositories/{repository}/specification"),
    )
    .await;
    assert_eq!(latest["snapshot_sha"], NEW);
    let (_, second) = get(&app, &token, &search(repository, OLD, Some(cursor))).await;
    assert_eq!(second["snapshot_sha"], OLD);
    assert_eq!(second["total_matches"], 4);
    assert_eq!(second["matches"][1]["document_id"], "DOC-OPERATIONS");
    assert_eq!(second["start_at"], 2);
    let (_, last) = get(
        &app,
        &token,
        &format!("{}&start_at=3", search(repository, OLD, None)),
    )
    .await;
    assert_eq!(last["start_at"], 3);
    assert_eq!(last["total_matches"], 4);
    assert_eq!(last["matches"].as_array().unwrap().len(), 1);
    assert_eq!(
        last["matches"][0]["match_id"],
        second["matches"][1]["match_id"]
    );
    assert!(last["next_cursor"].is_null());
    assert_eq!(
        get(
            &app,
            &token,
            &format!("{}&start_at=0", search(repository, OLD, Some(cursor)))
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    _worker.run_once().await.unwrap();
    let (_, changed) = get(&app, &token, &search(repository, NEW, None)).await;
    assert_eq!(changed["total_matches"], 3);
    assert_eq!(
        get(&app, &token, &search(repository, NEW, Some(cursor)))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let (_, old_doc) = get(
        &app,
        &token,
        &format!("/api/v1/repositories/{repository}/documents/DOC-PERMISSIONS?commit={OLD}"),
    )
    .await;
    assert!(old_doc["markdown"].as_str().unwrap().contains("מחיקה"));
    assert!(old_doc["render_error"].is_null());
    assert!(
        old_doc["render_events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["kind"] == "open" && event["tag"] == "table")
    );
    assert!(
        old_doc["render_events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|event| event["kind"] == "text")
            .all(|event| event["block_id"].is_string() && event["offset_utf16"].is_number())
    );
    let (_, new_doc) = get(
        &app,
        &token,
        &format!("/api/v1/repositories/{repository}/documents/DOC-PERMISSIONS?commit={NEW}"),
    )
    .await;
    assert!(new_doc["markdown"].as_str().unwrap().contains("צפייה"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn foreign_repositories_and_revoked_sessions_never_receive_cached_content(pool: PgPool) {
    let (actor, repository, foreign, token) = setup(&pool).await;
    let (app, provider, _worker) = fixture(&pool);
    let (_, repositories) = get(&app, &token, "/api/v1/repositories").await;
    assert_eq!(repositories["repositories"].as_array().unwrap().len(), 1);
    assert_eq!(repositories["repositories"][0]["can_create"], false);
    assert_eq!(
        get(
            &app,
            &token,
            &format!("/api/v1/repositories/{foreign}/specification")
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(provider.reads.load(Ordering::SeqCst), 0);
    assert_eq!(
        get(
            &app,
            &token,
            &format!("/api/v1/repositories/{repository}/specification")
        )
        .await
        .0,
        StatusCode::OK
    );
    sqlx::query("UPDATE users SET status = 'DISABLED' WHERE id = $1")
        .bind(actor)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        get(&app, &token, &search(repository, OLD, None)).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        get(
            &app,
            &token,
            &format!("/api/v1/repositories/{repository}/documents/DOC-OVERVIEW?commit={OLD}")
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn missing_app_configuration_returns_a_setup_error_without_fixture_fallback(pool: PgPool) {
    let (_, repository, _, token) = setup(&pool).await;
    let app = router(AppState::new(pool));
    let (status, body) = get(
        &app,
        &token,
        &format!("/api/v1/repositories/{repository}/specification"),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"]["code"], "REPOSITORY_NOT_CONFIGURED");
    assert_eq!(body["error"]["retryable"], false);
}

#[sqlx::test(migrations = "../../migrations")]
async fn image_proxy_is_private_pinned_validated_and_rechecks_access_after_processing(
    pool: PgPool,
) {
    let (actor, repository, foreign, token) = setup(&pool).await;
    let (app, provider, _worker) = fixture(&pool);
    let uri = format!("/api/v1/repositories/{repository}/assets/{ASSET}?commit={OLD}");
    // Invalid asset names must not expose an inaccessible repository's existence.
    assert_eq!(
        get(
            &app,
            &token,
            &format!("/api/v1/repositories/{foreign}/assets/bad.svg?commit={OLD}")
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(provider.asset_reads.load(Ordering::SeqCst), 0);
    assert_eq!(get(&app, "", &uri).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(
        get(&app, &token, &format!("{uri}bad")).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        get(
            &app,
            &token,
            &format!("/api/v1/repositories/{repository}/assets/bad.svg?commit={OLD}")
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        get(
            &app,
            &token,
            &format!(
                "/api/v1/repositories/{repository}/assets/{ASSET}?commit={}",
                "3".repeat(40)
            )
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(provider.asset_reads.load(Ordering::SeqCst), 0);
    provider.version.store(1, Ordering::SeqCst);
    let image = response(&app, &token, &uri).await;
    assert_eq!(image.status(), StatusCode::OK);
    assert_eq!(image.headers()["content-type"], "image/png");
    assert_eq!(image.headers()["cache-control"], "no-store");
    assert_eq!(image.headers()["x-content-type-options"], "nosniff");
    assert_eq!(
        image.headers()["content-disposition"],
        "inline; filename=\"specspace-display.png\""
    );
    let bytes = image.into_body().collect().await.unwrap().to_bytes();
    assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert_eq!(
        get(
            &app,
            &token,
            &format!(
                "/api/v1/repositories/{repository}/assets/{}.png?commit={OLD}",
                "0".repeat(64)
            )
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    provider.asset_corrupt.store(1, Ordering::SeqCst);
    let (status, error) = get(&app, &token, &uri).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error["error"]["code"], "ASSET_HASH_MISMATCH");
    provider.asset_corrupt.store(2, Ordering::SeqCst);
    let (status, error) = get(&app, &token, &uri).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        error["error"]["details"]["max_asset_bytes"],
        5 * 1024 * 1024
    );
    provider.asset_corrupt.store(0, Ordering::SeqCst);
    *provider.revoke_after_asset.lock().unwrap() = Some((pool, actor));
    assert_eq!(get(&app, &token, &uri).await.0, StatusCode::NOT_FOUND);
    assert_eq!(get(&app, &token, &uri).await.0, StatusCode::UNAUTHORIZED);
}

/// Real local TCP/session/SQL gate. Provider latency, corporate-network latency
/// and browser rendering are separate acceptance gates, not inferred here.
#[sqlx::test(migrations = "../../migrations")]
#[ignore = "release-mode 20MiB authenticated HTTP performance measurement"]
async fn authenticated_http_six_readers_20mib_benchmark(pool: PgPool) {
    use std::time::{Duration, Instant};
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .without_time()
        .with_ansi(false)
        .try_init();
    let (_, repository, _, token) = setup(&pool).await;
    let organization: Uuid =
        sqlx::query_scalar("SELECT organization_id FROM repositories WHERE id=$1")
            .bind(repository)
            .fetch_one(&pool)
            .await
            .unwrap();
    let mut tokens = vec![token];
    for _ in 0..5 {
        let actor = Uuid::new_v4();
        sqlx::query("INSERT INTO users(id,issuer,external_subject,display_name) VALUES($1,'fixture',$1::text,'Synthetic HTTP performance user')")
            .bind(actor).execute(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO memberships(organization_id,user_id,role) VALUES($1,$2,'CONTRIBUTOR')",
        )
        .bind(organization)
        .bind(actor)
        .execute(&pool)
        .await
        .unwrap();
        tokens.push(
            SessionStore::new(pool.clone())
                .mint(actor, 3600)
                .await
                .unwrap()
                .session_cookie_value()
                .into(),
        );
    }
    let count = 1000;
    let target = 20 * 1024 * 1024;
    let mut files = HashMap::new();
    let mut catalog = vec![];
    for index in 0..count {
        let document = format!("DOC-{index:04}");
        let path = format!("docs/{index:04}.md");
        let mut source = format!(
            "---\ndocument_id: {document}\ntitle: Synthetic\nowner_team: TEAM\nschema_version: 1\n---\n<!-- ss:section:SEC-{index:04} -->\n# פרק\n\n"
        );
        // Repeated filler alone would deduplicate most of the searchable book.
        source.push_str(&format!("ייחודי{index:04} "));
        let suffix = format!("{}מזהה{index:04}", "מחיקה SearchNeedle [x]*\n\n".repeat(8));
        let size = target / count + usize::from(index < target % count);
        let phrase = "טקסט הקשר עברי רגיל לבדיקת ביצועים. ";
        source.push_str(&phrase.repeat((size - source.len() - suffix.len()) / phrase.len()));
        source.push_str(&suffix);
        source.extend(std::iter::repeat_n('x', size - source.len()));
        assert_eq!(source.len(), size);
        files.insert(path.clone(), source);
        catalog.push(serde_json::json!({ "document_id": document, "path": path }));
    }
    let source_bytes: usize = files.values().map(String::len).sum();
    assert_eq!(source_bytes, target);
    files.insert(
        ".specspace/catalog.json".into(),
        serde_json::json!({ "schema_version": 1, "documents": catalog }).to_string(),
    );
    // Use the production connection limit, independently of sqlx::test's pool.
    let http_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(10)
        .connect_with(pool.connect_options().as_ref().clone())
        .await
        .unwrap();
    let (app, provider, worker) = fixture_with_files(&http_pool, files);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
            .unwrap();
    });
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();
    let specification = format!("{base}/api/v1/repositories/{repository}/specification");
    let catalog = http_json(&client, &tokens[0], &specification).await;
    assert_eq!(catalog["documents"].as_array().unwrap().len(), count);
    assert!(catalog["findings"].as_array().unwrap().is_empty());
    let started = Instant::now();
    assert!(matches!(
        worker.run_once().await.unwrap(),
        specspace_infrastructure::index_worker::WorkResult::Completed(_)
    ));
    let worker_seconds = started.elapsed().as_secs_f64();
    let unique_payload_bytes: i64 = sqlx::query_scalar("SELECT coalesce(sum(octet_length(original_text)),0)::bigint FROM search_index_texts WHERE organization_id=$1")
        .bind(organization).fetch_one(&http_pool).await.unwrap();
    assert!(
        unique_payload_bytes > 19 * 1024 * 1024,
        "benchmark must scan distinct book payloads"
    );
    let fetches = provider.reads.load(Ordering::SeqCst);
    // Start with no-result queries on a fresh process cache. Warming only
    // matching payloads must not make this workload repeatedly scan all TOAST.
    let mut empty_readers = tokio::task::JoinSet::new();
    for token in &tokens {
        let token = token.clone();
        let client = client.clone();
        let uri = http_search(&base, repository, "absent-needle", 0);
        empty_readers.spawn(async move {
            let mut durations = vec![];
            for _ in 0..20 {
                let started = Instant::now();
                let page = http_json(&client, &token, &uri).await;
                durations.push(started.elapsed().as_secs_f64());
                assert_eq!(page["total_matches"], 0);
            }
            durations
        });
    }
    let mut empty_durations = vec![];
    while let Some(reader) = empty_readers.join_next().await {
        empty_durations.extend(reader.unwrap());
    }
    let empty_p95 = p95(&mut empty_durations);
    let cold = Instant::now();
    let page = http_json(
        &client,
        &tokens[0],
        &http_search(&base, repository, "מחיקה", 0),
    )
    .await;
    let cold_seconds = cold.elapsed().as_secs_f64();
    assert_eq!(page["total_matches"], 8000);
    let mut readers = tokio::task::JoinSet::new();
    for (reader, (token, query)) in tokens
        .into_iter()
        .zip([
            "מחיקה",
            "מְחיקה",
            "מ\u{200f}חיקה",
            "מחיק",
            "SEARCHNEEDLE",
            "[x]*",
        ])
        .enumerate()
    {
        let client = client.clone();
        let base = base.clone();
        let specification = specification.clone();
        readers.spawn(async move {
            let mut search = vec![];
            let mut chapters = vec![];
            let mut books = vec![];
            for round in 0..20 {
                let started = Instant::now();
                let page = http_json(
                    &client,
                    &token,
                    &http_search(&base, repository, query, reader * 1200 + round * 4),
                )
                .await;
                search.push(started.elapsed().as_secs_f64());
                assert_eq!(page["snapshot_sha"], OLD);
                assert_eq!(page["total_matches"], 8000);
                assert_eq!(page["matches"].as_array().unwrap().len(), 100);
                let document = page["matches"][0]["document_id"].as_str().unwrap();
                let started = Instant::now();
                let chapter = http_json(
                    &client,
                    &token,
                    &format!(
                        "{base}/api/v1/repositories/{repository}/documents/{document}?commit={OLD}"
                    ),
                )
                .await;
                chapters.push(started.elapsed().as_secs_f64());
                assert_eq!(chapter["snapshot_sha"], OLD);
                assert_eq!(chapter["document_id"], document);
                assert!(chapter["error_code"].is_null());
                assert!(chapter["render_error"].is_null());
                let started = Instant::now();
                let book = http_json(&client, &token, &specification).await;
                books.push(started.elapsed().as_secs_f64());
                assert_eq!(book["snapshot_sha"], OLD);
            }
            (search, chapters, books)
        });
    }
    let (mut search, mut chapters, mut books) = (vec![], vec![], vec![]);
    while let Some(reader) = readers.join_next().await {
        let (a, b, c) = reader.unwrap();
        search.extend(a);
        chapters.extend(b);
        books.extend(c);
    }
    let samples = search.len();
    let search_p95 = p95(&mut search);
    let chapter_p95 = p95(&mut chapters);
    let book_p95 = p95(&mut books);
    let _ = stop.send(());
    server.await.unwrap();
    assert_eq!(
        provider.reads.load(Ordering::SeqCst),
        fetches,
        "warm HTTP requests must not fetch Git content"
    );
    http_pool.close().await;
    println!(
        "source_bytes={source_bytes} unique_payload_bytes={unique_payload_bytes} documents={count} users=6 samples_per_route={samples} matches=8000 worker_seconds={worker_seconds:.3} fresh_no_match_http_p95_seconds={empty_p95:.3} subsequent_http_search_seconds={cold_seconds:.3} warm_http_search_p95_seconds={search_p95:.3} warm_http_chapter_p95_seconds={chapter_p95:.3} warm_http_metadata_p95_seconds={book_p95:.3}"
    );
    assert!(
        search_p95 < 1.0,
        "authenticated HTTP search p95 exceeded 1s: {search_p95:.3}"
    );
    assert!(
        empty_p95 < 1.0,
        "fresh no-match HTTP p95 exceeded 1s: {empty_p95:.3}"
    );
    assert!(
        chapter_p95 < 1.5,
        "HTTP chapter p95 exceeded 1.5s: {chapter_p95:.3}; browser navigation remains separate"
    );
    assert!(
        book_p95 < 2.0,
        "HTTP metadata p95 exceeded 2s: {book_p95:.3}; browser first content remains separate"
    );
}

fn http_search(base: &str, repository: Uuid, query: &str, start: usize) -> String {
    let mut params = url::form_urlencoded::Serializer::new(String::new());
    params
        .append_pair("q", query)
        .append_pair("commit", OLD)
        .append_pair("start_at", &start.to_string());
    format!(
        "{base}/api/v1/repositories/{repository}/find?{}",
        params.finish()
    )
}

async fn http_json(client: &reqwest::Client, token: &str, url: &str) -> serde_json::Value {
    let response = client
        .get(url)
        .header("Cookie", format!("__Host-specspace-session={token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    response.json().await.unwrap()
}

fn p95(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    values[(values.len() * 95).div_ceil(100) - 1]
}
