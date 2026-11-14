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
    let provider = Arc::new(Provider {
        files,
        version: AtomicU8::new(0),
        reads: AtomicUsize::new(0),
        asset_reads: AtomicUsize::new(0),
        asset_corrupt: AtomicU8::new(0),
        revoke_after_asset: Mutex::new(None),
    });
    let mut state = AppState::new(pool.clone());
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
