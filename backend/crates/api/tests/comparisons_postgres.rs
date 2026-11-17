use async_trait::async_trait;
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use specspace_api::{AppState, auth::CookiePolicy, router};
use specspace_application::comparison_reader::{
    BoundProviderFactory, ComparisonReadError, RepositoryBinding,
};
use specspace_domain::{BranchName, RepositoryPath};
use specspace_git_adapter::{
    CommitMetadata, CommitReceipt, CommitRequest, CommitSha, FileEntry, FileMode, GitError,
    GitProvider, blob,
};
use specspace_infrastructure::{
    comparison_worker::{ComparisonWork, ComparisonWorker},
    sessions::SessionStore,
};
use sqlx::PgPool;
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tower::ServiceExt;
use uuid::Uuid;
const OLD: &str = "1111111111111111111111111111111111111111";
const HEAD: &str = "2222222222222222222222222222222222222222";
const MAIN: &str = "3333333333333333333333333333333333333333";
fn sha(value: &str) -> CommitSha {
    CommitSha::parse(value).unwrap()
}
struct Provider {
    files: BTreeMap<String, String>,
    reads: AtomicUsize,
    backoff: AtomicUsize,
    hook: Mutex<Option<(PgPool, Uuid)>>,
}
impl Provider {
    fn text(&self, commit: &CommitSha, path: &str) -> String {
        let text = self.files[path].clone();
        if commit.as_str() == HEAD && (path.ends_with(".md") || path == "assets/change.png") {
            text.replace("before", "edited")
        } else {
            text
        }
    }
}
#[async_trait]
impl GitProvider for Provider {
    async fn main_head(&self) -> Result<CommitSha, GitError> {
        if self
            .backoff
            .compare_exchange(1, 0, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            return Err(GitError::RateLimited {
                retry_after_seconds: Some(13),
            });
        }
        Ok(sha(MAIN))
    }
    async fn branch_head(&self, _: &BranchName) -> Result<CommitSha, GitError> {
        Ok(sha(HEAD))
    }
    async fn create_branch(&self, _: &BranchName, _: &CommitSha) -> Result<CommitSha, GitError> {
        panic!("comparison cannot mutate Git")
    }
    async fn commit(&self, _: CommitRequest<'_>) -> Result<CommitSha, GitError> {
        panic!("comparison cannot mutate Git")
    }
    async fn read_text(
        &self,
        commit: &CommitSha,
        path: &RepositoryPath,
    ) -> Result<String, GitError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        match self.backoff.load(Ordering::SeqCst) {
            2 => {
                return Err(GitError::RateLimited {
                    retry_after_seconds: Some(13),
                });
            }
            3 => return Ok("forged source".into()),
            4 => return Err(GitError::FileMissing),
            _ => {}
        }
        let hook = self.hook.lock().unwrap().take();
        if let Some((pool, actor)) = hook {
            sqlx::query("UPDATE users SET status='DISABLED' WHERE id=$1")
                .bind(actor)
                .execute(&pool)
                .await
                .unwrap();
        }
        Ok(self.text(commit, path.as_str()))
    }
    async fn read_bytes(
        &self,
        _: &CommitSha,
        _: &RepositoryPath,
        _: usize,
    ) -> Result<Vec<u8>, GitError> {
        panic!("inventory cannot fetch opaque media")
    }
    async fn list_files(&self, commit: &CommitSha) -> Result<Vec<FileEntry>, GitError> {
        Ok(self
            .files
            .keys()
            .map(|path| {
                let text = self.text(commit, path);
                FileEntry {
                    path: RepositoryPath::parse(path).unwrap(),
                    blob_sha: blob::oid(text.as_bytes(), 40).unwrap(),
                    size: text.len() as u64,
                    mode: FileMode::Regular,
                }
            })
            .collect())
    }
    async fn commit_metadata(&self, _: &CommitSha) -> Result<CommitMetadata, GitError> {
        panic!("unused metadata")
    }
    async fn commit_receipt(&self, _: &CommitSha) -> Result<CommitReceipt, GitError> {
        panic!("no mutation evidence required")
    }
    async fn merge_base(&self, _: &CommitSha, _: &CommitSha) -> Result<CommitSha, GitError> {
        Ok(sha(OLD))
    }
    async fn is_ancestor(&self, a: &CommitSha, b: &CommitSha) -> Result<bool, GitError> {
        Ok(a == b || a.as_str() == OLD && [HEAD, MAIN].contains(&b.as_str()))
    }
}
struct Factory {
    provider: Arc<Provider>,
    repository: Uuid,
}
#[async_trait]
impl BoundProviderFactory for Factory {
    async fn provider_bound(
        &self,
        binding: &RepositoryBinding,
    ) -> Result<Arc<dyn GitProvider>, ComparisonReadError> {
        assert_eq!(binding.repository, self.repository);
        assert_eq!(binding.remote_id, 42);
        assert_eq!(binding.installation_id, 7);
        Ok(self.provider.clone())
    }
}

struct User {
    id: Uuid,
    token: String,
}
struct Fixture {
    actor: User,
    owner: User,
    proposal: Uuid,
    organization: Uuid,
    app: Router,
    worker: ComparisonWorker,
    provider: Arc<Provider>,
}
async fn user(pool: &PgPool, organization: Uuid, role: &str) -> User {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO users(id,issuer,external_subject,display_name) VALUES($1,'fixture',$1::text,'Synthetic diff HTTP user')").bind(id).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO memberships(organization_id,user_id,role) VALUES($1,$2,$3)")
        .bind(organization)
        .bind(id)
        .bind(role)
        .execute(pool)
        .await
        .unwrap();
    let token = SessionStore::new(pool.clone())
        .mint(id, 3600)
        .await
        .unwrap()
        .session_cookie_value()
        .to_owned();
    User { id, token }
}
async fn fixture(pool: &PgPool, configured: bool) -> Fixture {
    fixture_source(pool, configured, "PRIVATE SOURCE SENTINEL before", false).await
}
async fn fixture_source(pool: &PgPool, configured: bool, body: &str, asset: bool) -> Fixture {
    let organization = Uuid::new_v4();
    let repository = Uuid::new_v4();
    let proposal = Uuid::new_v4();
    let team = Uuid::new_v4();
    sqlx::query("INSERT INTO organizations(id,name) VALUES($1,'Synthetic HTTP diff organization')")
        .bind(organization)
        .execute(pool)
        .await
        .unwrap();
    let actor = user(pool, organization, "READER").await;
    let owner = user(pool, organization, "ADMIN").await;
    sqlx::query("INSERT INTO teams(id,organization_id,name) VALUES($1,$2,'Synthetic team')")
        .bind(team)
        .bind(organization)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO repositories(id,organization_id,provider,remote_id,installation_id) VALUES($1,$2,'GITHUB','42','7')").bind(repository).bind(organization).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO proposals(id,organization_id,repository_id,title,description,branch,owner_id,author_id,team_id,state,ui_impact,base_sha,head_sha,target_sha) VALUES($1,$2,$3,'Synthetic HTTP comparison','Synthetic comparison description','change/compare',$4,$4,$5,'DRAFT','UNDECIDED',$6,$7,$6)")
        .bind(proposal).bind(organization).bind(repository).bind(owner.id).bind(team).bind(OLD).bind(HEAD).execute(pool).await.unwrap();
    let mut files = BTreeMap::new();
    let entries:Vec<_>=(0..3).map(|n| {
        let path=format!("docs/{n}.md");let id=format!("DOC-{n}");files.insert(path.clone(),format!("---\ndocument_id: {id}\ntitle: Synthetic\nowner_team: fixture\nschema_version: 1\n---\n{body}\n"));json!({"document_id":id,"path":path})
    }).collect();
    files.insert(
        ".specspace/catalog.json".into(),
        json!({"schema_version":1,"documents":entries}).to_string(),
    );
    if asset {
        files.insert(
            "assets/change.png".into(),
            "opaque fixture bytes before".into(),
        );
    }
    let provider = Arc::new(Provider {
        files,
        reads: AtomicUsize::new(0),
        backoff: AtomicUsize::new(0),
        hook: Mutex::new(None),
    });
    let mut state = AppState::new(pool.clone());
    state.cookies = CookiePolicy::new("http://localhost:4200", true).unwrap();
    if configured {
        state.configure_comparison_source(Arc::new(Factory {
            provider: provider.clone(),
            repository,
        }));
    }
    let worker = state.comparison_worker(Duration::from_secs(900)).unwrap();
    Fixture {
        actor,
        owner,
        proposal,
        organization,
        app: router(state),
        worker,
        provider,
    }
}
async fn get(
    app: &Router,
    user: &User,
    uri: &str,
    bound: bool,
) -> (StatusCode, Value, Option<String>, Option<String>) {
    let mut request = Request::builder()
        .uri(uri)
        .header("Cookie", format!("specspace-session={}", user.token));
    if bound {
        request = request.header("X-Expected-Actor", user.id.to_string());
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    assert_eq!(response.headers()["cache-control"], "no-store");
    let location = response
        .headers()
        .get("Location")
        .map(|v| v.to_str().unwrap().to_owned());
    let retry = response
        .headers()
        .get("Retry-After")
        .map(|v| v.to_str().unwrap().to_owned());
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap(),
        location,
        retry,
    )
}
fn prepare(f: &Fixture, request: Uuid) -> String {
    format!("/api/v1/proposals/{}/diff?request_id={request}", f.proposal)
}
#[sqlx::test(migrations = "../../migrations")]
async fn queued_http_comparison_becomes_a_pinned_paged_result_and_a_typed_generic_job(
    pool: PgPool,
) {
    let f = fixture(&pool, true).await;
    let request = prepare(&f, Uuid::new_v4());
    let (status, pending, location, retry) = get(&f.app, &f.actor, &request, true).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{pending}");
    assert_eq!(retry.as_deref(), Some("2"));
    assert_eq!(pending["phase"], "QUEUED");
    assert!(pending["summary"].is_null() && pending["page"].is_null());
    let location = location.unwrap();
    assert!(location.contains("?snapshot="));
    assert_eq!(f.provider.reads.load(Ordering::SeqCst), 0);
    let job = format!("/api/v1/jobs/{}", pending["job_id"].as_str().unwrap());
    let (status, job_pending, _, _) = get(&f.app, &f.actor, &job, true).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(job_pending["kind"], "COMPARE_PROPOSAL");
    assert_eq!(
        job_pending["comparison"]["snapshot_id"],
        pending["snapshot_id"]
    );
    assert!(job_pending["comparison"]["content_id"].is_null());
    assert_eq!(
        f.worker.run_once().await.unwrap(),
        ComparisonWork::Completed(Uuid::parse_str(pending["job_id"].as_str().unwrap()).unwrap())
    );
    let reads = f.provider.reads.load(Ordering::SeqCst);
    let (status, ready, _, retry) = get(
        &f.app,
        &f.actor,
        &format!("{location}&offset=0&limit=1"),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ready}");
    assert!(retry.is_none());
    assert_eq!(ready["state"], "READY");
    assert_eq!(ready["summary"]["merge_base_sha"], OLD);
    assert_eq!(ready["page"]["total"], 3);
    assert_eq!(ready["page"]["files"].as_array().unwrap().len(), 1);
    assert_eq!(ready["page"]["next_offset"], 1);
    assert!(ready["completed_at"].as_str().unwrap().ends_with('Z'));
    assert!(!ready.to_string().contains("PRIVATE SOURCE SENTINEL"));
    assert!(!ready.to_string().contains("installation"));
    let (_, rest, _, _) = get(
        &f.app,
        &f.actor,
        &format!("{location}&offset=1&limit=100"),
        true,
    )
    .await;
    assert_eq!(rest["page"]["files"].as_array().unwrap().len(), 2);
    assert_eq!(rest["page"]["content_id"], ready["summary"]["content_id"]);
    assert_eq!(f.provider.reads.load(Ordering::SeqCst), reads);
    let (_, job_ready, _, _) = get(&f.app, &f.actor, &job, true).await;
    assert_eq!(job_ready["phase"], "READY");
    assert_eq!(
        job_ready["comparison"]["content_id"],
        ready["summary"]["content_id"]
    );
    let (_, replay, _, _) = get(&f.app, &f.actor, &request, true).await;
    assert_eq!(replay["snapshot_id"], pending["snapshot_id"]);
}
#[sqlx::test(migrations = "../../migrations")]
async fn malformed_or_unbound_queries_never_reserve_work_and_shared_cookie_changes_cannot_retarget_it(
    pool: PgPool,
) {
    let f = fixture(&pool, true).await;
    let request = prepare(&f, Uuid::new_v4());
    assert_eq!(
        get(&f.app, &f.actor, &request, false).await.0,
        StatusCode::CONFLICT
    );
    for suffix in [
        "",
        "?request_id=bad",
        "?request_id=00000000-0000-0000-0000-000000000000",
        "?snapshot=bad",
        "?snapshot=00000000-0000-0000-0000-000000000001&head=bad",
        "?request_id=00000000-0000-4000-8000-000000000001&offset=0",
        "?request_id=00000000-0000-4000-8000-000000000001&head=bad",
        "?request_id=00000000-0000-4000-8000-000000000001&unknown=1",
        "?snapshot=00000000-0000-4000-8000-000000000001&limit=101",
        "?request_id=00000000-0000-4000-8000-000000000001&request_id=00000000-0000-4000-8000-000000000002",
    ] {
        assert_eq!(
            get(
                &f.app,
                &f.actor,
                &format!("/api/v1/proposals/{}/diff{suffix}", f.proposal),
                true
            )
            .await
            .0,
            StatusCode::BAD_REQUEST,
            "{suffix}"
        );
    }
    for duplicate in [false, true] {
        let mut builder = Request::builder()
            .uri(&request)
            .header("Cookie", format!("specspace-session={}", f.owner.token))
            .header("X-Expected-Actor", f.actor.id.to_string());
        if duplicate {
            builder = builder.header("X-Expected-Actor", f.actor.id.to_string());
        }
        let response = f
            .app
            .clone()
            .oneshot(builder.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }
    let response = f
        .app
        .clone()
        .oneshot(Request::builder().uri(request).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM comparison_snapshots")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}
#[sqlx::test(migrations = "../../migrations")]
async fn every_page_and_job_read_rechecks_private_actor_membership_and_repository_binding(
    pool: PgPool,
) {
    let f = fixture(&pool, true).await;
    let (_, ticket, location, _) = get(&f.app, &f.actor, &prepare(&f, Uuid::new_v4()), true).await;
    f.worker.run_once().await.unwrap();
    let location = location.unwrap();
    let job = format!("/api/v1/jobs/{}", ticket["job_id"].as_str().unwrap());
    assert_eq!(
        get(&f.app, &f.actor, &job, false).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        get(&f.app, &f.owner, &location, true).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        get(&f.app, &f.owner, &job, true).await.0,
        StatusCode::NOT_FOUND
    );
    sqlx::query("UPDATE repositories SET installation_id='8' WHERE id=(SELECT repository_id FROM proposals WHERE id=$1)").bind(f.proposal).execute(&pool).await.unwrap();
    assert_eq!(
        get(&f.app, &f.actor, &location, true).await.0,
        StatusCode::CONFLICT
    );
    sqlx::query("UPDATE repositories SET installation_id='7' WHERE id=(SELECT repository_id FROM proposals WHERE id=$1)").bind(f.proposal).execute(&pool).await.unwrap();
    // Keep a valid login in another organization while revoking this scope.
    let other_org = Uuid::new_v4();
    sqlx::query("INSERT INTO organizations(id,name) VALUES($1,'Other organization')")
        .bind(other_org)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO memberships(organization_id,user_id,role) VALUES($1,$2,'READER')")
        .bind(other_org)
        .bind(f.actor.id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM memberships WHERE organization_id=$1 AND user_id=$2")
        .bind(f.organization)
        .bind(f.actor.id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        get(&f.app, &f.actor, &location, true).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        get(&f.app, &f.actor, &job, true).await.0,
        StatusCode::NOT_FOUND
    );
    sqlx::query("DELETE FROM memberships WHERE user_id=$1")
        .bind(f.actor.id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        get(&f.app, &f.actor, &location, true).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        get(&f.app, &f.actor, &job, true).await.0,
        StatusCode::UNAUTHORIZED
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn historical_results_keep_their_content_and_report_current_closed_context_without_new_git_reads(
    pool: PgPool,
) {
    let f = fixture(&pool, true).await;
    let request = prepare(&f, Uuid::new_v4());
    let (_, ticket, location, _) = get(&f.app, &f.actor, &request, true).await;
    f.worker.run_once().await.unwrap();
    let reads = f.provider.reads.load(Ordering::SeqCst);
    sqlx::query(
        "UPDATE proposals SET head_sha=repeat('4',40),version=2,state='CLOSED' WHERE id=$1",
    )
    .bind(f.proposal)
    .execute(&pool)
    .await
    .unwrap();
    let (status, historical, _, _) = get(&f.app, &f.actor, &location.unwrap(), true).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(historical["snapshot_id"], ticket["snapshot_id"]);
    assert_eq!(historical["summary"]["head_sha"], HEAD);
    assert_eq!(historical["summary"]["proposal_version"], 1);
    assert_eq!(historical["current_proposal_version"], 2);
    assert_eq!(historical["current_proposal_state"], "CLOSED");
    assert_eq!(historical["context_changed"], true);
    assert_eq!(historical["historical_head"], true);
    let (_, replay, _, _) = get(&f.app, &f.actor, &request, true).await;
    assert_eq!(replay["snapshot_id"], ticket["snapshot_id"]);
    assert_eq!(f.provider.reads.load(Ordering::SeqCst), reads);
}
#[sqlx::test(migrations = "../../migrations")]
async fn changed_replay_and_admission_limits_are_explicit_without_duplicate_jobs(pool: PgPool) {
    let f = fixture(&pool, true).await;
    let request = prepare(&f, Uuid::new_v4());
    get(&f.app, &f.actor, &request, true).await;
    let (status, error, _, _) = get(&f.app, &f.actor, &format!("{request}&head={OLD}"), true).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "IDEMPOTENCY_MISMATCH");
    get(&f.app, &f.actor, &prepare(&f, Uuid::new_v4()), true).await;
    let (status, error, _, retry) = get(&f.app, &f.actor, &prepare(&f, Uuid::new_v4()), true).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(retry.as_deref(), Some("60"));
    assert_eq!(error["error"]["code"], "COMPARISON_LIMIT");
    assert_eq!(
        get(&f.app, &f.actor, &request, true).await.0,
        StatusCode::ACCEPTED
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn missing_setup_and_worker_failures_are_terminal_statuses_without_fake_ready_pages(
    pool: PgPool,
) {
    let f = fixture(&pool, false).await;
    let (_, _, location, _) = get(&f.app, &f.actor, &prepare(&f, Uuid::new_v4()), true).await;
    assert!(matches!(
        f.worker.run_once().await.unwrap(),
        ComparisonWork::Failed(_)
    ));
    let (status, failed, _, retry) = get(&f.app, &f.actor, &location.unwrap(), true).await;
    assert_eq!(status, StatusCode::OK);
    assert!(retry.is_none());
    assert_eq!(failed["state"], "FAILED");
    assert_eq!(failed["error_code"], "COMPARISON_SETUP_REQUIRED");
    assert!(failed["summary"].is_null() && failed["page"].is_null());
}

async fn ready_file(f: &Fixture) -> (Value, Value) {
    let (_, ticket, location, _) = get(&f.app, &f.actor, &prepare(f, Uuid::new_v4()), true).await;
    f.worker.run_once().await.unwrap();
    let (status, ready, _, _) = get(&f.app, &f.actor, &location.unwrap(), true).await;
    assert_eq!(status, StatusCode::OK, "{ready}");
    let file = ready["page"]["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["kind"] == "document")
        .unwrap()
        .clone();
    (ticket, file)
}
fn raw_path(f: &Fixture, ticket: &Value, file: &Value) -> String {
    format!(
        "/api/v1/proposals/{}/diff/files/{}?snapshot={}",
        f.proposal,
        file["id"].as_str().unwrap(),
        ticket["snapshot_id"].as_str().unwrap()
    )
}
#[sqlx::test(migrations = "../../migrations")]
async fn raw_http_windows_reconstruct_exact_pinned_source_after_later_head_and_closure(
    pool: PgPool,
) {
    let body = format!(
        "{}\r\n<script>before</script>\r\nlast",
        "שָׁלוֹם 👩🏽‍💻 before\t".repeat(200)
    );
    let f = fixture_source(&pool, true, &body, false).await;
    let (ticket, file) = ready_file(&f).await;
    let path = raw_path(&f, &ticket, &file);
    let reads = f.provider.reads.load(Ordering::SeqCst);
    let (mut before, mut after) = (String::new(), String::new());
    let mut cursor = json!({"chunk":0,"offset":0});
    let mut pages = 0;
    loop {
        let url = format!(
            "{path}&chunk={}&offset={}&max_bytes=1025",
            cursor["chunk"], cursor["offset"]
        );
        let (status, window, _, retry) = get(&f.app, &f.actor, &url, true).await;
        assert_eq!(status, StatusCode::OK, "{window}");
        assert!(retry.is_none());
        assert_eq!(window["snapshot_id"], ticket["snapshot_id"]);
        assert_eq!(window["file"], file);
        assert_eq!(window["head_sha"], HEAD);
        assert_eq!(window["merge_base_sha"], OLD);
        assert_eq!(window["raw"]["old_sha256"], file["before"]["sha256"]);
        assert_eq!(window["raw"]["new_sha256"], file["after"]["sha256"]);
        assert_eq!(window["raw"]["cursor"], cursor);
        let mut size = 0;
        for segment in window["raw"]["segments"].as_array().unwrap() {
            assert_eq!(segment["old_start_byte"], before.len());
            assert_eq!(segment["new_start_byte"], after.len());
            let a = segment["old_text"].as_str().unwrap();
            let b = segment["new_text"].as_str().unwrap();
            size += a.len() + b.len();
            before.push_str(a);
            after.push_str(b);
        }
        assert!(size <= 1025);
        assert_eq!(window["raw"]["returned_text_bytes"], size);
        pages += 1;
        if pages == 1 {
            // Keep this accepted snapshot while the proposal becomes historical.
            sqlx::query(
                "UPDATE proposals SET head_sha=$2,version=version+1,state='CLOSED' WHERE id=$1",
            )
            .bind(f.proposal)
            .bind("4".repeat(40))
            .execute(&pool)
            .await
            .unwrap();
        }
        if window["raw"]["next"].is_null() {
            break;
        }
        assert!(size > 0 && pages < 100);
        cursor = window["raw"]["next"].clone();
    }
    assert!(pages > 1);
    assert_eq!(
        before,
        f.provider
            .text(&sha(OLD), file["before"]["path"].as_str().unwrap())
    );
    assert_eq!(
        after,
        f.provider
            .text(&sha(HEAD), file["after"]["path"].as_str().unwrap())
    );
    // Each window fetches only its verified pair; body caching remains subsequent work.
    assert_eq!(f.provider.reads.load(Ordering::SeqCst), reads + pages * 2);
    let first = get(&f.app, &f.actor, &format!("{path}&max_bytes=1025"), true)
        .await
        .1;
    let replay = get(&f.app, &f.actor, &format!("{path}&max_bytes=1025"), true)
        .await
        .1;
    assert_eq!(first, replay);
}
#[sqlx::test(migrations = "../../migrations")]
async fn raw_file_queries_require_private_actor_scope_and_bounded_unambiguous_coordinates(
    pool: PgPool,
) {
    let f = fixture(&pool, true).await;
    let (ticket, file) = ready_file(&f).await;
    let path = raw_path(&f, &ticket, &file);
    let reads = f.provider.reads.load(Ordering::SeqCst);
    for query in [
        "&max_bytes=0",
        "&max_bytes=65537",
        "&chunk=1025",
        "&offset=2097153",
        "&offset=-1",
        "&chunk=0&chunk=1",
        "&head=other",
        "&snapshot=duplicate",
    ] {
        assert_eq!(
            get(&f.app, &f.actor, &format!("{path}{query}"), true)
                .await
                .0,
            StatusCode::BAD_REQUEST,
            "{query}"
        );
    }
    assert_eq!(
        get(&f.app, &f.actor, path.split('?').next().unwrap(), true)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        get(&f.app, &f.actor, &path, false).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        get(&f.app, &f.owner, &path, true).await.0,
        StatusCode::NOT_FOUND
    );
    let unknown = path.replace(file["id"].as_str().unwrap(), &"0".repeat(64));
    assert_eq!(
        get(&f.app, &f.actor, &unknown, true).await.0,
        StatusCode::NOT_FOUND
    );
    let foreign = path.replace(
        ticket["snapshot_id"].as_str().unwrap(),
        &Uuid::new_v4().to_string(),
    );
    assert_eq!(
        get(&f.app, &f.actor, &foreign, true).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(f.provider.reads.load(Ordering::SeqCst), reads);
    // A syntactically bounded but nonexistent alignment coordinate fails explicitly.
    let invalid = get(&f.app, &f.actor, &format!("{path}&chunk=1024"), true).await;
    assert_eq!(invalid.0, StatusCode::BAD_REQUEST);
    assert_eq!(invalid.1["error"]["code"], "INVALID_COMPARISON_WINDOW");
}
#[sqlx::test(migrations = "../../migrations")]
async fn raw_files_never_serialize_unverified_or_revoked_source_and_preserve_backoff(pool: PgPool) {
    let f = fixture(&pool, true).await;
    let (ticket, file) = ready_file(&f).await;
    let path = raw_path(&f, &ticket, &file);
    for (mode, code, delay) in [
        (2, "COMPARISON_SOURCE_BACKOFF", Some("13")),
        (3, "COMPARISON_EVIDENCE_INVALID", None),
        (4, "COMPARISON_SOURCE_MISSING", None),
    ] {
        f.provider.backoff.store(mode, Ordering::SeqCst);
        let (status, response, _, retry) = get(&f.app, &f.actor, &path, true).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response["error"]["code"], code);
        assert_eq!(retry.as_deref(), delay);
        assert!(!response.to_string().contains("PRIVATE SOURCE SENTINEL"));
        assert!(response.get("raw").is_none());
    }
    f.provider.backoff.store(0, Ordering::SeqCst);
    *f.provider.hook.lock().unwrap() = Some((pool.clone(), f.actor.id));
    let (status, response, _, _) = get(&f.app, &f.actor, &path, true).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{response}");
    assert!(response.get("raw").is_none());
    assert!(!response.to_string().contains("PRIVATE SOURCE SENTINEL"));
}
#[sqlx::test(migrations = "../../migrations")]
async fn raw_reads_keep_pending_states_and_nontext_assets_explicit_without_fetching_media(
    pool: PgPool,
) {
    let f = fixture_source(&pool, true, "before", true).await;
    let (_, ticket, location, _) = get(&f.app, &f.actor, &prepare(&f, Uuid::new_v4()), true).await;
    let pending = format!(
        "/api/v1/proposals/{}/diff/files/{}?snapshot={}",
        f.proposal,
        "0".repeat(64),
        ticket["snapshot_id"].as_str().unwrap()
    );
    let response = get(&f.app, &f.actor, &pending, true).await;
    assert_eq!(response.0, StatusCode::CONFLICT);
    assert_eq!(response.1["error"]["code"], "COMPARISON_NOT_READY");
    assert_eq!(f.provider.reads.load(Ordering::SeqCst), 0);
    f.worker.run_once().await.unwrap();
    let ready = get(&f.app, &f.actor, &location.unwrap(), true).await.1;
    let asset = ready["page"]["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["kind"] == "asset")
        .unwrap();
    let reads = f.provider.reads.load(Ordering::SeqCst);
    let (status, response, _, _) = get(&f.app, &f.actor, &raw_path(&f, &ticket, asset), true).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(response["error"]["code"], "COMPARISON_FILE_NOT_TEXT");
    assert_eq!(f.provider.reads.load(Ordering::SeqCst), reads);
}
