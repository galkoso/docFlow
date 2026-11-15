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
