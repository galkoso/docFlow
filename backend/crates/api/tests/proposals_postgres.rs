use async_trait::async_trait;
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use specspace_api::{AppState, auth::CookiePolicy, router};
use specspace_application::{book::BookError, reader::BookProviderFactory};
use specspace_domain::{BranchName, RepositoryPath};
use specspace_git_adapter::{
    CommitMetadata, CommitRequest, CommitSha, FileEntry, GitError, GitProvider,
};
use specspace_infrastructure::{
    proposal_worker::{ProposalWorker, ProvisionWork},
    sessions::SessionStore,
};
use sqlx::PgPool;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
    },
};
use tower::ServiceExt;
use uuid::Uuid;
const ORIGIN: &str = "http://localhost:4200";
const OLD: &str = "1111111111111111111111111111111111111111";
const NEW: &str = "2222222222222222222222222222222222222222";

struct User {
    actor: Uuid,
    repository: Uuid,
    team: Uuid,
    token: String,
    csrf: String,
}
async fn user(pool: &PgPool, role: &str) -> User {
    let (actor, organization, repository, team) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    sqlx::query("INSERT INTO organizations(id,name) VALUES($1,'Synthetic proposal API')")
        .bind(organization)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO users(id,issuer,external_subject,display_name) VALUES($1,'fixture',$1::text,'Synthetic proposal user')").bind(actor).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO memberships(organization_id,user_id,role) VALUES($1,$2,$3)")
        .bind(organization)
        .bind(actor)
        .bind(role)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO repositories(id,organization_id,provider,remote_id,installation_id) VALUES($1,$2,'GITHUB','42','7')").bind(repository).bind(organization).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO teams(id,organization_id,name) VALUES($1,$2,'Synthetic team')")
        .bind(team)
        .bind(organization)
        .execute(pool)
        .await
        .unwrap();
    let credentials = SessionStore::new(pool.clone())
        .mint(actor, 3600)
        .await
        .unwrap();
    User {
        actor,
        repository,
        team,
        token: credentials.session_cookie_value().into(),
        csrf: credentials.csrf_cookie_value().into(),
    }
}
fn input(user: &User) -> serde_json::Value {
    serde_json::json!({"repository_id":user.repository,"title":"הצעת שינוי חדשה","branch":"change/25-new-proposal","source_branch":"main","expected_source_sha":OLD,"description":"תיאור מפורט של השינוי המוצע","team_id":user.team,"initial_mode":"manual","ui_impact":"undecided","links":[{"kind":"ticket","url":"https://tickets.example.test/ISSUE-1"}]})
}

struct Provider {
    main: AtomicU8,
    online: AtomicBool,
    reads: AtomicUsize,
    writes: AtomicUsize,
    refs: Mutex<HashMap<String, CommitSha>>,
    unknown: AtomicBool,
    revoke: Mutex<Option<(PgPool, Uuid)>>,
}
#[async_trait]
impl GitProvider for Provider {
    async fn main_head(&self) -> Result<CommitSha, GitError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        let revoke = self.revoke.lock().unwrap().take();
        if let Some((pool, actor)) = revoke {
            sqlx::query("UPDATE users SET status='DISABLED' WHERE id=$1")
                .bind(actor)
                .execute(&pool)
                .await
                .unwrap();
        }
        CommitSha::parse(if self.main.load(Ordering::SeqCst) == 0 {
            OLD
        } else {
            NEW
        })
    }
    async fn branch_head(&self, branch: &BranchName) -> Result<CommitSha, GitError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.refs
            .lock()
            .unwrap()
            .get(branch.as_str())
            .cloned()
            .ok_or(GitError::BranchMissing)
    }
    async fn create_branch(
        &self,
        branch: &BranchName,
        source: &CommitSha,
    ) -> Result<CommitSha, GitError> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        let previous = self
            .refs
            .lock()
            .unwrap()
            .insert(branch.as_str().into(), source.clone());
        assert!(previous.is_none(), "duplicate Git write");
        if self.unknown.load(Ordering::SeqCst) {
            Err(GitError::ResultUnknown)
        } else {
            Ok(source.clone())
        }
    }
    async fn commit(&self, _: CommitRequest<'_>) -> Result<CommitSha, GitError> {
        panic!("creation must not commit")
    }
    async fn read_text(&self, _: &CommitSha, _: &RepositoryPath) -> Result<String, GitError> {
        panic!("creation must not load the book")
    }
    async fn read_bytes(
        &self,
        _: &CommitSha,
        _: &RepositoryPath,
        _: usize,
    ) -> Result<Vec<u8>, GitError> {
        panic!("creation must not load assets")
    }
    async fn list_files(&self, _: &CommitSha) -> Result<Vec<FileEntry>, GitError> {
        panic!("creation must not load the tree")
    }
    async fn commit_receipt(
        &self,
        _: &specspace_git_adapter::CommitSha,
    ) -> Result<specspace_git_adapter::CommitReceipt, specspace_git_adapter::GitError> {
        Err(specspace_git_adapter::GitError::ProviderRejected)
    }
    async fn commit_metadata(&self, _: &CommitSha) -> Result<CommitMetadata, GitError> {
        panic!("creation must not load metadata")
    }
    async fn merge_base(&self, _: &CommitSha, _: &CommitSha) -> Result<CommitSha, GitError> {
        Err(GitError::ProviderRejected)
    }
    async fn is_ancestor(&self, _: &CommitSha, _: &CommitSha) -> Result<bool, GitError> {
        panic!("creation must use the displayed source")
    }
}
struct Source {
    repository: Uuid,
    provider: Arc<Provider>,
}
#[async_trait]
impl BookProviderFactory for Source {
    async fn provider(&self, repository: Uuid) -> Result<Arc<dyn GitProvider>, BookError> {
        assert_eq!(repository, self.repository, "foreign source lookup");
        if !self.provider.online.load(Ordering::SeqCst) {
            return Err(BookError::Provider(GitError::ProviderUnavailable));
        }
        Ok(self.provider.clone())
    }
}
fn app(pool: &PgPool, user: &User) -> (Router, Arc<Provider>, ProposalWorker) {
    let provider = Arc::new(Provider {
        main: AtomicU8::new(0),
        online: AtomicBool::new(true),
        reads: AtomicUsize::new(0),
        writes: AtomicUsize::new(0),
        refs: Mutex::new(HashMap::new()),
        unknown: AtomicBool::new(false),
        revoke: Mutex::new(None),
    });
    let mut state = AppState::new(pool.clone());
    state.cookies = CookiePolicy::new(ORIGIN, true).unwrap();
    state.configure_book_source(Arc::new(Source {
        repository: user.repository,
        provider: provider.clone(),
    }));
    let worker = state
        .proposal_worker(std::time::Duration::from_secs(30))
        .unwrap();
    (router(state), provider, worker)
}
async fn response(
    app: &Router,
    user: &User,
    method: &str,
    uri: &str,
    body: Option<&serde_json::Value>,
    key: Option<&str>,
    verification: (Option<&str>, Option<&str>),
) -> axum::response::Response {
    let (origin, csrf) = verification;
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header("Cookie", format!("specspace-session={}", user.token))
        .header("X-Role", "ADMIN");
    if let Some(origin) = origin {
        request = request.header("Origin", origin);
    }
    if let Some(csrf) = csrf {
        request = request.header("X-XSRF-TOKEN", csrf);
    }
    if let Some(key) = key {
        request = request.header("Idempotency-Key", key);
    }
    if body.is_some() {
        request = request.header("Content-Type", "application/json");
    }
    app.clone()
        .oneshot(
            request
                .body(body.map_or_else(Body::empty, |body| {
                    Body::from(serde_json::to_vec(body).unwrap())
                }))
                .unwrap(),
        )
        .await
        .unwrap()
}
async fn json(response: axum::response::Response) -> (StatusCode, serde_json::Value) {
    let status = response.status();
    assert_eq!(response.headers()["cache-control"], "no-store");
    let body =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    (status, body)
}
async fn create(
    app: &Router,
    user: &User,
    body: &serde_json::Value,
    key: &str,
) -> (StatusCode, serde_json::Value) {
    json(
        response(
            app,
            user,
            "POST",
            "/api/v1/proposals",
            Some(body),
            Some(key),
            (Some(ORIGIN), Some(&user.csrf)),
        )
        .await,
    )
    .await
}
async fn get(app: &Router, user: &User, uri: &str) -> (StatusCode, serde_json::Value) {
    json(response(app, user, "GET", uri, None, None, (None, None)).await).await
}

#[sqlx::test(migrations = "../../migrations")]
async fn creation_is_accepted_then_worker_result_is_observable_without_secret_payloads(
    pool: PgPool,
) {
    let user = user(&pool, "CONTRIBUTOR").await;
    let (app, provider, worker) = app(&pool, &user);
    let body = input(&user);
    let accepted = response(
        &app,
        &user,
        "POST",
        "/api/v1/proposals",
        Some(&body),
        Some("first"),
        (Some(ORIGIN), Some(&user.csrf)),
    )
    .await;
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
    let location = accepted.headers()["location"].to_str().unwrap().to_owned();
    let (_, ticket) = json(accepted).await;
    assert!(ticket["number"].as_i64().unwrap() > 0);
    assert_eq!(provider.writes.load(Ordering::SeqCst), 0);
    let (_, queued) = get(&app, &user, &location).await;
    assert_eq!(queued["state"], "QUEUED");
    assert_eq!(queued["phase"], "QUEUED");
    assert!(queued["creation"].is_null());
    assert!(!queued.to_string().contains("remote_id"));
    assert!(!queued.to_string().contains("installation_id"));
    assert!(queued["payload"].is_null());
    assert_eq!(
        worker.run_once().await.unwrap(),
        ProvisionWork::Completed(Uuid::parse_str(ticket["job_id"].as_str().unwrap()).unwrap())
    );
    let (_, ready) = get(&app, &user, &location).await;
    assert_eq!(ready["state"], "SUCCEEDED");
    assert_eq!(ready["phase"], "READY");
    assert_eq!(ready["creation"]["proposal_id"], ticket["proposal_id"]);
    assert_eq!(ready["creation"]["head_sha"], OLD);
    let reads = provider.reads.load(Ordering::SeqCst);
    provider.main.store(1, Ordering::SeqCst);
    provider.online.store(false, Ordering::SeqCst);
    let (status, replayed) = create(&app, &user, &body, "first").await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(ticket, replayed);
    assert_eq!(provider.reads.load(Ordering::SeqCst), reads);
    assert_eq!(provider.writes.load(Ordering::SeqCst), 1);
    let mut different = body;
    different["title"] = "הצעה אחרת".into();
    let (status, error) = create(&app, &user, &different, "first").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "IDEMPOTENCY_MISMATCH");
}

#[sqlx::test(migrations = "../../migrations")]
async fn concurrent_http_replays_share_the_same_provisioning_ticket(pool: PgPool) {
    let user = user(&pool, "CONTRIBUTOR").await;
    let (app, provider, _) = app(&pool, &user);
    let body = input(&user);
    let (a, b) = tokio::join!(
        create(&app, &user, &body, "double-click"),
        create(&app, &user, &body, "double-click")
    );
    assert_eq!(a.0, StatusCode::ACCEPTED);
    assert_eq!(a, b);
    let (status, error) = create(&app, &user, &body, "another-key").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "BRANCH_EXISTS");
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(provider.writes.load(Ordering::SeqCst), 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn moved_source_requires_an_explicit_new_sha_and_validation_never_writes_git(pool: PgPool) {
    let user = user(&pool, "CONTRIBUTOR").await;
    let (app, provider, _) = app(&pool, &user);
    let mut body = input(&user);
    provider.main.store(1, Ordering::SeqCst);
    let (status, error) = create(&app, &user, &body, "source-check").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "SOURCE_MOVED");
    assert_eq!(error["error"]["details"]["expected_source_sha"], OLD);
    assert_eq!(error["error"]["details"]["actual_source_sha"], NEW);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM proposals")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    body["branch"] = "main".into();
    body["ui_impact"] = "no".into();
    let (status, error) = create(&app, &user, &body, "invalid").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(error["error"]["field_errors"]["branch"].is_object());
    assert!(error["error"]["field_errors"]["ui_impact_reason"].is_object());
    body = input(&user);
    body["expected_source_sha"] = NEW.into();
    body["client_role"] = "ADMIN".into();
    assert_eq!(
        create(&app, &user, &body, "unknown-fields").await.0,
        StatusCode::BAD_REQUEST
    );
    body.as_object_mut().unwrap().remove("client_role");
    assert_eq!(
        create(&app, &user, &body, "source-check").await.0,
        StatusCode::ACCEPTED
    );
    assert_eq!(provider.writes.load(Ordering::SeqCst), 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn csrf_roles_foreign_scope_and_revocation_are_enforced_before_queueing(pool: PgPool) {
    let actor = user(&pool, "CONTRIBUTOR").await;
    let foreign = user(&pool, "ADMIN").await;
    let reader = user(&pool, "READER").await;
    let (app, provider, _) = app(&pool, &actor);
    let body = input(&actor);
    for (origin, csrf) in [
        (None, Some(actor.csrf.as_str())),
        (Some("https://other.example"), Some(actor.csrf.as_str())),
        (Some(ORIGIN), None),
    ] {
        assert_eq!(
            response(
                &app,
                &actor,
                "POST",
                "/api/v1/proposals",
                Some(&body),
                Some("csrf"),
                (origin, csrf)
            )
            .await
            .status(),
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        response(
            &app,
            &actor,
            "POST",
            "/api/v1/proposals",
            Some(&body),
            None,
            (Some(ORIGIN), Some(&actor.csrf))
        )
        .await
        .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        create(&app, &reader, &input(&reader), "reader").await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        create(&app, &actor, &input(&foreign), "foreign").await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(provider.reads.load(Ordering::SeqCst), 0);
    *provider.revoke.lock().unwrap() = Some((pool.clone(), actor.actor));
    assert_eq!(
        create(&app, &actor, &body, "revoke").await.0,
        StatusCode::NOT_FOUND
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn job_reads_are_scoped_and_uncertain_results_never_claim_success(pool: PgPool) {
    let actor = user(&pool, "CONTRIBUTOR").await;
    let foreign = user(&pool, "ADMIN").await;
    let (app, provider, worker) = app(&pool, &actor);
    provider.unknown.store(true, Ordering::SeqCst);
    let (_, ticket) = create(&app, &actor, &input(&actor), "unknown").await;
    let location = format!("/api/v1/jobs/{}", ticket["job_id"].as_str().unwrap());
    assert_eq!(
        get(&app, &foreign, &location).await.0,
        StatusCode::NOT_FOUND
    );
    assert!(matches!(
        worker.run_once().await.unwrap(),
        ProvisionWork::Deferred(_)
    ));
    let (_, status) = get(&app, &actor, &location).await;
    assert_eq!(status["state"], "QUEUED");
    assert_eq!(status["phase"], "RECONCILIATION_REQUIRED");
    assert_eq!(status["error_code"], "CREATE_RESULT_UNKNOWN");
    assert!(status["creation"].is_null());
    sqlx::query("UPDATE jobs SET available_at=clock_timestamp()-interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        worker.run_once().await.unwrap(),
        ProvisionWork::Completed(_)
    ));
    assert_eq!(get(&app, &actor, &location).await.1["phase"], "READY");
    assert_eq!(provider.writes.load(Ordering::SeqCst), 1);
    sqlx::query("UPDATE users SET status='DISABLED' WHERE id=$1")
        .bind(actor.actor)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        get(&app, &actor, &location).await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn index_job_status_uses_snapshot_scope_and_reports_real_worker_phase(pool: PgPool) {
    use specspace_application::{
        book::{BookDocument, BookSnapshot},
        search::IndexSnapshot,
    };
    use specspace_infrastructure::search_index::PgSearchIndex;
    let actor = user(&pool, "READER").await;
    let foreign = user(&pool, "ADMIN").await;
    let (app, _, _) = app(&pool, &actor);
    let commit = CommitSha::parse(OLD).unwrap();
    let index = PgSearchIndex::new(pool);
    let ticket = index
        .enqueue(actor.actor, actor.repository, &commit)
        .await
        .unwrap();
    let uri = format!("/api/v1/jobs/{}", ticket.job_id);
    let (status, queued) = get(&app, &actor, &uri).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(queued["kind"], "INDEX_SNAPSHOT");
    assert_eq!(queued["phase"], "QUEUED");
    assert_eq!(get(&app, &foreign, &uri).await.0, StatusCode::NOT_FOUND);
    let lease = index.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    assert_eq!(get(&app, &actor, &uri).await.1["phase"], "INDEXING");
    let document = specspace_document_engine::markdown::parse("---\ndocument_id: DOC\ntitle: Synthetic\nowner_team: TEAM\nschema_version: 1\n---\n<!-- ss:section:SEC -->\n# כותרת\n\nטקסט לחיפוש\n").unwrap();
    let snapshot = BookSnapshot {
        commit,
        updated_at: "2026-11-15T00:00:00Z".into(),
        documents: vec![BookDocument {
            document_id: "DOC".into(),
            path: "docs/one.md".into(),
            content: Some(document),
            error_code: None,
        }],
        findings: vec![],
    };
    index
        .publish(&lease, &IndexSnapshot::build(&snapshot).unwrap())
        .await
        .unwrap();
    let (_, ready) = get(&app, &actor, &uri).await;
    assert_eq!(ready["state"], "SUCCEEDED");
    assert_eq!(ready["phase"], "READY");
    assert!(ready["creation"].is_null());
}

async fn colleague(pool: &PgPool, existing: &User, role: &str) -> User {
    let actor = Uuid::new_v4();
    sqlx::query("INSERT INTO users(id,issuer,external_subject,display_name) VALUES($1,'fixture',$1::text,'Synthetic colleague')").bind(actor).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO memberships(organization_id,user_id,role) SELECT organization_id,$2,$3 FROM repositories WHERE id=$1").bind(existing.repository).bind(actor).bind(role).execute(pool).await.unwrap();
    let credentials = SessionStore::new(pool.clone())
        .mint(actor, 3600)
        .await
        .unwrap();
    User {
        actor,
        repository: existing.repository,
        team: existing.team,
        token: credentials.session_cookie_value().into(),
        csrf: credentials.csrf_cookie_value().into(),
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn proposal_list_filters_and_cursors_preserve_scope_order_and_literal_queries(pool: PgPool) {
    let actor = user(&pool, "CONTRIBUTOR").await;
    let foreign = user(&pool, "ADMIN").await;
    let reader = colleague(&pool, &actor, "READER").await;
    let (app, provider, _) = app(&pool, &actor);
    let mut ids = vec![];
    for (index, title) in ["Alpha literal%", "Beta הצעה", "Gamma הצעה"]
        .into_iter()
        .enumerate()
    {
        let mut body = input(&actor);
        body["branch"] = format!("change/{index}-browse").into();
        body["title"] = title.into();
        let (status, ticket) = create(&app, &actor, &body, &format!("browse-{index}")).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        ids.push(ticket["proposal_id"].clone());
    }
    sqlx::query("UPDATE proposals SET updated_at='2026-11-15T00:00:00.123456Z'")
        .execute(&pool)
        .await
        .unwrap();
    let foreign_id = Uuid::new_v4();
    sqlx::query("INSERT INTO proposals(id,organization_id,repository_id,title,branch,owner_id,author_id,team_id,description,ui_impact) SELECT $1,r.organization_id,r.id,'Private foreign','change/foreign',$3,$3,$4,'Private foreign description','UNDECIDED' FROM repositories r WHERE r.id=$2")
        .bind(foreign_id).bind(foreign.repository).bind(foreign.actor).bind(foreign.team).execute(&pool).await.unwrap();
    let reads = provider.reads.load(Ordering::SeqCst);
    let (status, first) = get(&app, &actor, "/api/v1/proposals?limit=2").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first["proposals"].as_array().unwrap().len(), 2);
    assert!(first["proposals"][0]["head_sha"].is_null());
    assert!(first["proposals"][0]["changed_documents"].is_null());
    let cursor = first["next_cursor"].as_str().unwrap();
    let (status, last) = get(
        &app,
        &actor,
        &format!("/api/v1/proposals?limit=2&cursor={cursor}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(last["proposals"].as_array().unwrap().len(), 1);
    assert!(last["next_cursor"].is_null());
    let mut seen: Vec<_> = first["proposals"]
        .as_array()
        .unwrap()
        .iter()
        .chain(last["proposals"].as_array().unwrap())
        .map(|row| row["proposal_id"].clone())
        .collect();
    seen.sort_by_key(|value| value.as_str().unwrap().to_owned());
    ids.sort_by_key(|value| value.as_str().unwrap().to_owned());
    assert_eq!(seen, ids);
    for suffix in ["limit=1", "limit=2&q=Beta", "limit=2&state=DRAFT"] {
        assert_eq!(
            get(
                &app,
                &actor,
                &format!("/api/v1/proposals?{suffix}&cursor={cursor}")
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        get(
            &app,
            &reader,
            &format!("/api/v1/proposals?limit=2&cursor={cursor}")
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    for query in ["limit=0", "limit=201", "state=INVALID", "cursor=bad"] {
        assert_eq!(
            get(&app, &actor, &format!("/api/v1/proposals?{query}"))
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
    }
    for (query, count) in [("q=ALPHA", 1), ("q=%25", 1), ("q=_", 0), ("state=DRAFT", 0)] {
        let (status, page) = get(&app, &actor, &format!("/api/v1/proposals?{query}")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(page["proposals"].as_array().unwrap().len(), count);
    }
    let (_, page) = get(
        &app,
        &actor,
        &format!(
            "/api/v1/proposals?owner_id={}&team_id={}&repository_id={}",
            actor.actor, actor.team, actor.repository
        ),
    )
    .await;
    assert_eq!(page["proposals"].as_array().unwrap().len(), 3);
    assert_eq!(
        get(
            &app,
            &actor,
            &format!("/api/v1/proposals?repository_id={}", foreign.repository)
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        get(&app, &actor, &format!("/api/v1/proposals/{foreign_id}"))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        provider.reads.load(Ordering::SeqCst),
        reads,
        "list and denied reads must not consult Git"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn proposal_detail_keeps_recorded_versions_while_reporting_provider_changes(pool: PgPool) {
    let actor = user(&pool, "CONTRIBUTOR").await;
    let reader = colleague(&pool, &actor, "READER").await;
    let editor = colleague(&pool, &actor, "CONTRIBUTOR").await;
    let (app, provider, worker) = app(&pool, &actor);
    let (_, ticket) = create(&app, &actor, &input(&actor), "detail").await;
    let id = ticket["proposal_id"].as_str().unwrap();
    let uri = format!("/api/v1/proposals/{id}");
    let reads = provider.reads.load(Ordering::SeqCst);
    let (_, pending) = get(&app, &actor, &uri).await;
    assert_eq!(pending["synchronization"]["state"], "provisioning");
    assert_eq!(pending["base_sha"], OLD);
    assert_eq!(pending["target_sha"], OLD);
    assert!(pending["head_sha"].is_null());
    assert_eq!(pending["capabilities"]["can_edit"], false);
    assert_eq!(pending["creation_job_id"], ticket["job_id"]);
    assert_eq!(provider.reads.load(Ordering::SeqCst), reads);
    // Pending rows written before source persistence still expose their journaled source.
    sqlx::query("UPDATE proposals SET base_sha=NULL,target_sha=NULL WHERE id=$1")
        .bind(Uuid::parse_str(id).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    let (_, legacy_pending) = get(&app, &actor, &uri).await;
    assert_eq!(legacy_pending["base_sha"], OLD);
    assert_eq!(legacy_pending["target_sha"], OLD);
    assert!(legacy_pending["head_sha"].is_null());
    assert_eq!(provider.reads.load(Ordering::SeqCst), reads);
    assert!(matches!(
        worker.run_once().await.unwrap(),
        ProvisionWork::Completed(_)
    ));
    let (status, ready) = get(&app, &actor, &uri).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ready["state"], "DRAFT");
    assert_eq!(ready["head_sha"], OLD);
    assert_eq!(ready["changed_documents"], 0);
    assert_eq!(ready["capabilities"]["can_edit"], true);
    assert_eq!(ready["synchronization"]["state"], "in_sync");
    assert_eq!(ready["synchronization"]["base_outdated"], false);
    assert_eq!(ready["links"][0]["kind"], "ticket");
    assert_eq!(
        get(&app, &reader, &uri).await.1["capabilities"]["can_edit"],
        false
    );
    assert_eq!(
        get(&app, &editor, &uri).await.1["capabilities"]["can_edit"],
        false
    );
    sqlx::query("INSERT INTO proposal_collaborators(organization_id,proposal_id,user_id,access) SELECT organization_id,id,$2,'EDIT' FROM proposals WHERE id=$1").bind(Uuid::parse_str(id).unwrap()).bind(editor.actor).execute(&pool).await.unwrap();
    assert_eq!(
        get(&app, &editor, &uri).await.1["capabilities"]["can_edit"],
        true
    );
    provider.main.store(1, Ordering::SeqCst);
    let (_, outdated) = get(&app, &actor, &uri).await;
    assert_eq!(outdated["synchronization"]["base_outdated"], true);
    assert_eq!(outdated["synchronization"]["current_main_sha"], NEW);
    assert_eq!(outdated["target_sha"], OLD);
    provider.refs.lock().unwrap().insert(
        "change/25-new-proposal".into(),
        CommitSha::parse(NEW).unwrap(),
    );
    let (_, moved) = get(&app, &actor, &uri).await;
    assert_eq!(moved["synchronization"]["state"], "head_moved");
    assert_eq!(moved["synchronization"]["actual_head_sha"], NEW);
    assert_eq!(moved["head_sha"], OLD);
    provider.online.store(false, Ordering::SeqCst);
    let (status, unavailable) = get(&app, &actor, &uri).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        unavailable["synchronization"]["state"],
        "provider_unavailable"
    );
    assert!(unavailable["synchronization"]["actual_head_sha"].is_null());
    assert_eq!(unavailable["head_sha"], OLD);
    assert_eq!(unavailable["state"], "DRAFT");
    provider.online.store(true, Ordering::SeqCst);
    provider.refs.lock().unwrap().clear();
    assert_eq!(
        get(&app, &actor, &uri).await.1["synchronization"]["state"],
        "provider_missing"
    );
    sqlx::query("UPDATE proposals SET state='CLOSED' WHERE id=$1")
        .bind(Uuid::parse_str(id).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE memberships SET role='MAINTAINER' WHERE user_id=$1")
        .bind(actor.actor)
        .execute(&pool)
        .await
        .unwrap();
    let (_, closed) = get(&app, &actor, &uri).await;
    assert_eq!(closed["capabilities"]["can_edit"], false);
    assert_eq!(closed["capabilities"]["can_reopen"], false);
    provider.refs.lock().unwrap().insert(
        "change/25-new-proposal".into(),
        CommitSha::parse(OLD).unwrap(),
    );
    assert_eq!(
        get(&app, &actor, &uri).await.1["capabilities"]["can_reopen"],
        true
    );
    *provider.revoke.lock().unwrap() = Some((pool.clone(), actor.actor));
    assert_eq!(get(&app, &actor, &uri).await.0, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = "../../migrations")]
async fn proposal_choices_include_only_current_scoped_teams_and_eligible_owners(pool: PgPool) {
    let actor = user(&pool, "CONTRIBUTOR").await;
    let reader = colleague(&pool, &actor, "READER").await;
    let editor = colleague(&pool, &actor, "CONTRIBUTOR").await;
    let foreign = user(&pool, "ADMIN").await;
    let (app, provider, _) = app(&pool, &actor);
    let (status, options) = get(&app, &actor, "/api/v1/proposals/options").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(options["repositories"].as_array().unwrap().len(), 1);
    assert_eq!(options["repositories"][0]["can_create"], true);
    assert_eq!(options["teams"][0]["team_id"], actor.team.to_string());
    let owners = options["owners"].as_array().unwrap();
    assert_eq!(owners.len(), 2);
    assert!(
        owners
            .iter()
            .all(|owner| owner["owner_id"] != reader.actor.to_string()
                && owner["owner_id"] != foreign.actor.to_string())
    );
    assert_eq!(
        get(&app, &reader, "/api/v1/proposals/options").await.1["repositories"][0]["can_create"],
        false
    );
    assert_eq!(
        get(
            &app,
            &actor,
            &format!(
                "/api/v1/proposals/options?repository_id={}",
                foreign.repository
            )
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    sqlx::query("UPDATE users SET status='DISABLED' WHERE id=$1")
        .bind(editor.actor)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        get(&app, &actor, "/api/v1/proposals/options").await.1["owners"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(provider.reads.load(Ordering::SeqCst), 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn creation_context_previews_exact_number_source_and_survives_idempotent_replay(
    pool: PgPool,
) {
    let actor = user(&pool, "CONTRIBUTOR").await;
    let (app, provider, worker) = app(&pool, &actor);
    assert_eq!(
        get(&app, &actor, "/api/v1/repositories").await.1["repositories"][0]["can_create"],
        true
    );
    let prepare = serde_json::json!({"repository_id":actor.repository});
    let (status, context) = json(
        response(
            &app,
            &actor,
            "POST",
            "/api/v1/proposals/context",
            Some(&prepare),
            Some("prepare"),
            (Some(ORIGIN), Some(&actor.csrf)),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(context["source_sha"], OLD);
    assert!(context["number"].as_i64().unwrap() > 0);
    assert!(context["remote_id"].is_null());
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM proposals")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    assert_eq!(provider.writes.load(Ordering::SeqCst), 0);
    provider.main.store(1, Ordering::SeqCst);
    let reads = provider.reads.load(Ordering::SeqCst);
    let (_, repeated) = json(
        response(
            &app,
            &actor,
            "POST",
            "/api/v1/proposals/context",
            Some(&prepare),
            Some("prepare"),
            (Some(ORIGIN), Some(&actor.csrf)),
        )
        .await,
    )
    .await;
    assert_eq!(context, repeated);
    assert_eq!(provider.reads.load(Ordering::SeqCst), reads);
    let mut body = input(&actor);
    body["creation_context_id"] = context["context_id"].clone();
    body["branch"] = format!(
        "change/{}-new-proposal",
        context["number"].as_i64().unwrap()
    )
    .into();
    assert_eq!(
        create(&app, &actor, &body, "create").await.1["error"]["code"],
        "SOURCE_MOVED"
    );
    let (_, fresh) = json(
        response(
            &app,
            &actor,
            "POST",
            "/api/v1/proposals/context",
            Some(&prepare),
            Some("fresh-source"),
            (Some(ORIGIN), Some(&actor.csrf)),
        )
        .await,
    )
    .await;
    assert_eq!(fresh["source_sha"], NEW);
    assert_ne!(fresh["number"], context["number"]);
    body["creation_context_id"] = fresh["context_id"].clone();
    body["expected_source_sha"] = NEW.into();
    body["branch"] = format!("change/{}-new-proposal", fresh["number"].as_i64().unwrap()).into();
    let (status, ticket) = create(&app, &actor, &body, "create").await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(ticket["number"], fresh["number"]);
    assert!(matches!(
        worker.run_once().await.unwrap(),
        ProvisionWork::Completed(_)
    ));
    sqlx::query(
        "UPDATE proposal_creation_contexts SET expires_at=clock_timestamp()-interval '1 second'",
    )
    .execute(&pool)
    .await
    .unwrap();
    provider.online.store(false, Ordering::SeqCst);
    assert_eq!(create(&app, &actor, &body, "create").await.1, ticket);
}
