use super::*;
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU8, Ordering},
};
const HEAD: &str = "1111111111111111111111111111111111111111";
const BASE: &str = "2222222222222222222222222222222222222222";
struct TestState {
    mode: AtomicU8,
    row: Mutex<Value>,
    listing: Mutex<Vec<Value>>,
    writes: Mutex<Vec<Value>>,
    queries: Mutex<Vec<std::collections::HashMap<String, String>>>,
}
struct Fixture {
    adapter: GitHub,
    state: Arc<TestState>,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
fn row(number: u64) -> Value {
    json!({"id":100+number,"node_id":format!("PR_SYNTHETIC_{number}"),"number":number,"state":"open","merged_at":null,"draft":true,
        "title":"Original title","body":"Original body","html_url":format!("https://github.com/synthetic/specs/pull/{number}"),
        "head":{"ref":"change/42-review","sha":HEAD,"repo":{"id":42,"full_name":"synthetic/specs"}},
        "base":{"ref":"main","sha":BASE,"repo":{"id":42,"full_name":"synthetic/specs"}}})
}
fn scope() -> PullRequestScope {
    PullRequestScope::new(42, BranchName::parse("change/42-review").unwrap()).unwrap()
}
fn content() -> PullRequestContent {
    PullRequestContent::new(
        "New Hebrew כותרת".into(),
        "Description\n<!-- proposal-identity -->".into(),
    )
    .unwrap()
}
fn headers(headers: &HeaderMap) {
    assert_eq!(headers["accept"], "application/vnd.github+json");
    assert_eq!(headers["x-github-api-version"], "2026-03-10");
    assert_eq!(headers["authorization"], "Bearer synthetic-only");
}
fn failure(mode: u8) -> Option<Response> {
    match mode {
        1 => Some(
            (
                StatusCode::TOO_MANY_REQUESTS,
                [("Retry-After", "13")],
                Json(json!({})),
            )
                .into_response(),
        ),
        2 => Some((StatusCode::INTERNAL_SERVER_ERROR, Json(json!({}))).into_response()),
        3 => Some((StatusCode::NOT_FOUND, Json(json!({}))).into_response()),
        4 => Some((StatusCode::UNPROCESSABLE_ENTITY, Json(json!({}))).into_response()),
        5 => Some(
            (
                StatusCode::FOUND,
                [("Location", "http://127.0.0.1:9/never-follow")],
            )
                .into_response(),
        ),
        6 => Some((StatusCode::OK, "broken-json").into_response()),
        13 => Some((StatusCode::ACCEPTED, Json(json!([]))).into_response()),
        14 => Some((StatusCode::ACCEPTED, Json(row(7))).into_response()),
        _ => None,
    }
}
async fn read(
    State(state): State<Arc<TestState>>,
    Path(number): Path<u64>,
    request_headers: HeaderMap,
) -> Response {
    headers(&request_headers);
    assert!(number > 0);
    if let Some(error) = failure(state.mode.load(Ordering::SeqCst)) {
        return error;
    }
    Json(state.row.lock().unwrap().clone()).into_response()
}
async fn list(
    State(state): State<Arc<TestState>>,
    Query(query): Query<std::collections::HashMap<String, String>>,
    request_headers: HeaderMap,
) -> Response {
    headers(&request_headers);
    state.queries.lock().unwrap().push(query);
    if let Some(error) = failure(state.mode.load(Ordering::SeqCst)) {
        return error;
    }
    Json(state.listing.lock().unwrap().clone()).into_response()
}
async fn create(
    State(state): State<Arc<TestState>>,
    request_headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    headers(&request_headers);
    assert_eq!(body["head"], "change/42-review");
    assert_eq!(body["base"], "main");
    assert_eq!(body["draft"], true);
    assert_eq!(body["maintainer_can_modify"], false);
    assert!(body.get("state").is_none());
    assert!(body.get("head_repo").is_none());
    mutate(state, body, true)
}
async fn update(
    State(state): State<Arc<TestState>>,
    Path(number): Path<u64>,
    request_headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    headers(&request_headers);
    assert_eq!(number, 7);
    assert_eq!(body.as_object().unwrap().len(), 2);
    assert!(body.get("title").is_some() && body.get("body").is_some());
    mutate(state, body, false)
}
fn mutate(state: Arc<TestState>, body: Value, create: bool) -> Response {
    state.writes.lock().unwrap().push(body.clone());
    let mode = state.mode.load(Ordering::SeqCst);
    if let Some(error) = failure(mode) {
        return error;
    }
    let mut row = state.row.lock().unwrap().clone();
    row["title"] = body["title"].clone();
    row["body"] = body["body"].clone();
    match mode {
        7 => row["head"]["repo"]["id"] = json!(99),
        8 => row["body"] = json!("Not applied"),
        9 => row["number"] = json!(8),
        10 => row["draft"] = json!(false),
        11 => row["state"] = json!("closed"),
        _ => {}
    }
    (
        if mode == 12 {
            StatusCode::ACCEPTED
        } else if create {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        Json(row),
    )
        .into_response()
}
async fn fixture() -> Fixture {
    let state = Arc::new(TestState {
        mode: AtomicU8::new(0),
        row: Mutex::new(row(7)),
        listing: Mutex::new(vec![row(7)]),
        writes: Mutex::new(vec![]),
        queries: Mutex::new(vec![]),
    });
    let app = Router::new()
        .route("/repos/synthetic/specs/pulls", post(create).get(list))
        .route(
            "/repos/synthetic/specs/pulls/{number}",
            get(read).patch(update),
        )
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let adapter = GitHub::configured(
        "synthetic/specs",
        "synthetic-only",
        &format!("http://{address}"),
        &format!("http://{address}/graphql"),
    )
    .unwrap()
    .bound_to(42)
    .unwrap();
    Fixture {
        adapter,
        state,
        server,
    }
}
#[tokio::test]
async fn reads_pin_repository_branch_main_and_full_provider_state_without_returning_unsafe_links() {
    let f = fixture().await;
    let pr = f
        .adapter
        .pull_request(&scope(), PullRequestNumber::new(7).unwrap())
        .await
        .unwrap();
    assert_eq!(pr.repository_id, 42);
    assert_eq!(pr.number.get(), 7);
    assert_eq!(pr.head.as_str(), HEAD);
    assert_eq!(pr.target.as_str(), BASE);
    assert_eq!(pr.state, PullRequestState::Open);
    assert!(pr.draft);
    assert_eq!(pr.url, "https://github.com/synthetic/specs/pull/7");
    f.state.row.lock().unwrap()["state"] = json!("closed");
    assert_eq!(
        f.adapter
            .pull_request(&scope(), pr.number)
            .await
            .unwrap()
            .state,
        PullRequestState::Closed
    );
    f.state.row.lock().unwrap()["merged_at"] = json!("2026-11-18T00:00:00Z");
    assert_eq!(
        f.adapter
            .pull_request(&scope(), pr.number)
            .await
            .unwrap()
            .state,
        PullRequestState::Merged
    );
}
#[tokio::test]
async fn all_state_branch_discovery_is_bounded_and_never_hides_retargeted_candidates() {
    let f = fixture().await;
    let page = f
        .adapter
        .pull_requests(&scope(), PullRequestPage::new(1, 1).unwrap())
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.next.unwrap().page(), 2);
    let query = f.state.queries.lock().unwrap()[0].clone();
    assert_eq!(query["head"], "synthetic:change/42-review");
    assert_eq!(query["state"], "all");
    assert!(!query.contains_key("base"));
    assert_eq!(query["sort"], "created");
    assert_eq!(query["direction"], "asc");
    assert_eq!(query["per_page"], "1");
    assert!(matches!(
        f.adapter
            .pull_requests(&scope(), PullRequestPage::new(MAX_PR_PAGES, 1).unwrap())
            .await,
        Err(PullRequestError::Provider(GitError::ResponseTooLarge))
    ));
    f.state.listing.lock().unwrap()[0]["base"]["ref"] = json!("other-main");
    assert!(matches!(
        f.adapter
            .pull_requests(&scope(), PullRequestPage::new(1, 100).unwrap())
            .await,
        Err(PullRequestError::InvalidResponse)
    ));
    f.state.listing.lock().unwrap().clear();
    assert!(
        f.adapter
            .pull_requests(&scope(), PullRequestPage::new(2, 100).unwrap())
            .await
            .unwrap()
            .next
            .is_none()
    );
}
#[tokio::test]
async fn forks_wrong_refs_malformed_state_and_unsafe_urls_never_become_observations() {
    let f = fixture().await;
    for (pointer, value) in [
        ("/head/repo/id", json!(99)),
        ("/base/repo/id", json!(99)),
        ("/head/repo/full_name", json!("other/specs")),
        ("/head/ref", json!("change/other")),
        ("/base/ref", json!("other")),
        ("/html_url", json!("https://evil.example/pull/7")),
        ("/head/sha", json!("main")),
        ("/merged_at", json!("invalid")),
        ("/node_id", json!("bad\nnode")),
        ("/draft", Value::Null),
        ("/body", json!("x".repeat(MAX_PR_BODY_BYTES + 1))),
    ] {
        let mut bad = row(7);
        *bad.pointer_mut(pointer).unwrap() = value;
        *f.state.row.lock().unwrap() = bad;
        assert!(
            matches!(
                f.adapter
                    .pull_request(&scope(), PullRequestNumber::new(7).unwrap())
                    .await,
                Err(PullRequestError::InvalidResponse)
            ),
            "{pointer}"
        );
    }
    *f.state.row.lock().unwrap() = row(8);
    assert!(matches!(
        f.adapter
            .pull_request(&scope(), PullRequestNumber::new(7).unwrap())
            .await,
        Err(PullRequestError::InvalidResponse)
    ));
}
#[tokio::test]
async fn discovery_rejects_duplicate_or_overfull_pages_instead_of_proving_absence() {
    let f = fixture().await;
    *f.state.listing.lock().unwrap() = vec![row(7), row(7)];
    assert!(matches!(
        f.adapter
            .pull_requests(&scope(), PullRequestPage::new(1, 100).unwrap())
            .await,
        Err(PullRequestError::InvalidResponse)
    ));
    assert!(matches!(
        f.adapter
            .pull_requests(&scope(), PullRequestPage::new(1, 1).unwrap())
            .await,
        Err(PullRequestError::InvalidResponse)
    ));
    assert!(f.state.writes.lock().unwrap().is_empty());
}
#[tokio::test]
async fn creation_is_an_explicit_draft_and_updates_change_metadata_without_state_or_target_writes()
{
    let f = fixture().await;
    let content = content();
    let created = f
        .adapter
        .create_pull_request(&scope(), &content)
        .await
        .unwrap();
    assert!(created.draft);
    assert_eq!(created.title, content.title());
    assert_eq!(created.body, content.body());
    let updated = f
        .adapter
        .update_pull_request(&scope(), created.number, &content)
        .await
        .unwrap();
    assert_eq!(updated.id, created.id);
    assert_eq!(f.state.writes.lock().unwrap().len(), 2);
    f.state.row.lock().unwrap()["state"] = json!("closed");
    assert!(matches!(
        f.adapter
            .update_pull_request(&scope(), created.number, &content)
            .await,
        Err(PullRequestError::NotOpen)
    ));
    assert_eq!(f.state.writes.lock().unwrap().len(), 2);
}
#[tokio::test]
async fn unbound_and_wrong_repository_scopes_fail_before_any_provider_write() {
    let f = fixture().await;
    let wrong = PullRequestScope::new(99, scope().branch().clone()).unwrap();
    assert!(matches!(
        f.adapter.create_pull_request(&wrong, &content()).await,
        Err(PullRequestError::InvalidInput)
    ));
    let unbound = GitHub::new("synthetic/specs", "synthetic-only").unwrap();
    assert!(matches!(
        unbound.create_pull_request(&scope(), &content()).await,
        Err(PullRequestError::InvalidInput)
    ));
    assert!(f.state.writes.lock().unwrap().is_empty());
    assert!(f.state.queries.lock().unwrap().is_empty());
}
#[tokio::test]
async fn mutation_failures_and_malformed_success_preserve_uncertainty_without_automatic_retries() {
    let f = fixture().await;
    for mode in [2, 5, 6, 7, 8, 9, 10, 11, 12] {
        f.state.mode.store(mode, Ordering::SeqCst);
        let before = f.state.writes.lock().unwrap().len();
        assert!(
            matches!(
                f.adapter.create_pull_request(&scope(), &content()).await,
                Err(PullRequestError::Provider(GitError::ResultUnknown))
            ),
            "mode {mode}"
        );
        assert_eq!(f.state.writes.lock().unwrap().len(), before + 1);
    }
    f.state.mode.store(1, Ordering::SeqCst);
    assert!(matches!(
        f.adapter.create_pull_request(&scope(), &content()).await,
        Err(PullRequestError::Provider(GitError::RateLimited {
            retry_after_seconds: Some(13)
        }))
    ));
    f.state.mode.store(4, Ordering::SeqCst);
    assert!(matches!(
        f.adapter.create_pull_request(&scope(), &content()).await,
        Err(PullRequestError::Provider(GitError::ProviderRejected))
    ));
}
#[tokio::test]
async fn failed_update_preflight_never_writes_and_mismatched_update_receipts_remain_unknown() {
    let f = fixture().await;
    for mode in [1, 2, 3, 4, 5, 6] {
        f.state.mode.store(mode, Ordering::SeqCst);
        assert!(
            f.adapter
                .update_pull_request(&scope(), PullRequestNumber::new(7).unwrap(), &content())
                .await
                .is_err()
        );
        assert!(f.state.writes.lock().unwrap().is_empty());
    }
    for mode in [7, 8, 9, 10, 11, 12] {
        f.state.mode.store(mode, Ordering::SeqCst);
        assert!(
            matches!(
                f.adapter
                    .update_pull_request(&scope(), PullRequestNumber::new(7).unwrap(), &content())
                    .await,
                Err(PullRequestError::Provider(GitError::ResultUnknown))
            ),
            "mode {mode}"
        );
    }
}

#[tokio::test]
async fn a_cut_off_success_reply_is_unknown_and_never_causes_an_automatic_second_create() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0u8; 8192];
        let _ = socket.read(&mut request).await.unwrap();
        socket.write_all(b"HTTP/1.1 201 Created\r\nContent-Length: 100\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"number\":7").await.unwrap();
    });
    let adapter = GitHub::configured(
        "synthetic/specs",
        "synthetic-only",
        &format!("http://{address}"),
        &format!("http://{address}/graphql"),
    )
    .unwrap()
    .bound_to(42)
    .unwrap();
    assert!(matches!(
        adapter.create_pull_request(&scope(), &content()).await,
        Err(PullRequestError::Provider(GitError::ResultUnknown))
    ));
    server.await.unwrap();
}

#[tokio::test]
async fn pending_reads_never_prove_pr_presence_or_branch_exhaustion() {
    let f = fixture().await;
    f.state.mode.store(13, Ordering::SeqCst);
    assert!(matches!(
        f.adapter
            .pull_requests(&scope(), PullRequestPage::new(1, 100).unwrap())
            .await,
        Err(PullRequestError::InvalidResponse)
    ));
    f.state.mode.store(14, Ordering::SeqCst);
    assert!(matches!(
        f.adapter
            .pull_request(&scope(), PullRequestNumber::new(7).unwrap())
            .await,
        Err(PullRequestError::InvalidResponse)
    ));
    assert!(f.state.writes.lock().unwrap().is_empty());
}
