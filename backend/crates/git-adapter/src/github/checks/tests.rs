use super::*;
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, Method},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU8, AtomicUsize, Ordering},
};

const HEAD: &str = "1111111111111111111111111111111111111111";
const OP: Uuid = Uuid::from_u128(1);
struct TestState {
    mode: AtomicU8,
    reads: AtomicUsize,
    row: Mutex<Value>,
    listing: Mutex<Value>,
    writes: Mutex<Vec<(Method, Value)>>,
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
fn scope() -> CheckScope {
    CheckScope::new(42, 7, CommitSha::parse(HEAD).unwrap()).unwrap()
}
fn id() -> CheckRunId {
    CheckRunId::new(81).unwrap()
}
fn row(id: u64) -> Value {
    json!({"id":id,"name":CHECK_NAME,"head_sha":HEAD,"external_id":external_id(OP).unwrap(),"app":{"id":7},"status":"in_progress","conclusion":null,"completed_at":null,
        "output":{"title":CheckResult::Pending.title(),"summary":CheckResult::Pending.summary()}})
}
fn headers(headers: &HeaderMap) {
    assert_eq!(headers["authorization"], "Bearer synthetic-only");
    assert_eq!(headers["x-github-api-version"], "2026-03-10");
}
fn failure(mode: u8) -> Option<Response> {
    match mode {
        1 => Some((StatusCode::INTERNAL_SERVER_ERROR, Json(json!({}))).into_response()),
        2 => Some(
            (
                StatusCode::TOO_MANY_REQUESTS,
                [("Retry-After", "17")],
                Json(json!({})),
            )
                .into_response(),
        ),
        3 => Some((StatusCode::NOT_FOUND, Json(json!({}))).into_response()),
        4 => Some((StatusCode::FORBIDDEN, Json(json!({}))).into_response()),
        5 => Some(
            (
                StatusCode::FOUND,
                [("Location", "http://127.0.0.1:9/never-follow")],
            )
                .into_response(),
        ),
        6 => Some((StatusCode::OK, "broken-json").into_response()),
        7 => Some((StatusCode::ACCEPTED, Json(json!({}))).into_response()),
        _ => None,
    }
}
async fn read(
    State(s): State<Arc<TestState>>,
    Path(number): Path<u64>,
    request_headers: HeaderMap,
) -> Response {
    headers(&request_headers);
    assert_eq!(number, 81);
    s.reads.fetch_add(1, Ordering::SeqCst);
    if let Some(error) = failure(s.mode.load(Ordering::SeqCst)) {
        return error;
    }
    Json(s.row.lock().unwrap().clone()).into_response()
}
async fn list(
    State(s): State<Arc<TestState>>,
    Query(query): Query<std::collections::HashMap<String, String>>,
    request_headers: HeaderMap,
) -> Response {
    headers(&request_headers);
    s.queries.lock().unwrap().push(query);
    if let Some(error) = failure(s.mode.load(Ordering::SeqCst)) {
        return error;
    }
    Json(s.listing.lock().unwrap().clone()).into_response()
}
async fn create(
    State(s): State<Arc<TestState>>,
    request_headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    headers(&request_headers);
    mutate(s, body, true)
}
async fn update(
    State(s): State<Arc<TestState>>,
    Path(number): Path<u64>,
    request_headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    headers(&request_headers);
    assert_eq!(number, 81);
    mutate(s, body, false)
}
fn mutate(s: Arc<TestState>, body: Value, create: bool) -> Response {
    s.writes.lock().unwrap().push((
        if create { Method::POST } else { Method::PATCH },
        body.clone(),
    ));
    let mode = s.mode.load(Ordering::SeqCst);
    if let Some(error) = failure(mode) {
        return error;
    }
    let mut value = s.row.lock().unwrap().clone();
    value["status"] = body["status"].clone();
    value["output"] = body["output"].clone();
    value["conclusion"] = body.get("conclusion").cloned().unwrap_or(Value::Null);
    value["completed_at"] = if value["status"] == "completed" {
        json!("2026-11-25T12:00:00Z")
    } else {
        Value::Null
    };
    match mode {
        8 => value["app"]["id"] = json!(8),
        9 => value["id"] = json!(82),
        10 => value["external_id"] = json!(external_id(Uuid::from_u128(2)).unwrap()),
        11 => value["output"]["summary"] = json!("Not applied"),
        12 => value["head_sha"] = json!("2".repeat(40)),
        13 => {
            value["status"] = json!("completed");
            value["conclusion"] = json!("success");
        }
        14 => value["name"] = json!("unrelated"),
        _ => {}
    }
    (
        if create {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        Json(value),
    )
        .into_response()
}
async fn fixture() -> Fixture {
    let state = Arc::new(TestState {
        mode: AtomicU8::new(0),
        reads: AtomicUsize::new(0),
        row: Mutex::new(row(81)),
        listing: Mutex::new(json!({"total_count":1,"check_runs":[row(81)]})),
        writes: Mutex::new(vec![]),
        queries: Mutex::new(vec![]),
    });
    let app = Router::new()
        .route("/repos/synthetic/specs/check-runs", post(create))
        .route(
            "/repos/synthetic/specs/check-runs/{number}",
            get(read).patch(update),
        )
        .route(
            &format!("/repos/synthetic/specs/commits/{HEAD}/check-runs"),
            get(list),
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
async fn creation_is_pending_and_content_free_with_one_exact_operation_identity() {
    let f = fixture().await;
    let run = f.adapter.create_check(&scope(), OP).await.unwrap();
    assert_eq!(
        (run.id, run.operation, run.state),
        (id(), OP, CheckState::Pending)
    );
    let writes = f.state.writes.lock().unwrap();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].0, Method::POST);
    let body = &writes[0].1;
    assert_eq!(body.as_object().unwrap().len(), 5);
    assert_eq!(body["name"], CHECK_NAME);
    assert_eq!(body["head_sha"], HEAD);
    assert_eq!(body["status"], "in_progress");
    assert_eq!(body["external_id"], external_id(OP).unwrap());
    assert!(body.get("conclusion").is_none());
    assert_eq!(
        body["output"],
        json!({"title":CheckResult::Pending.title(),"summary":CheckResult::Pending.summary()})
    );
}
#[tokio::test]
async fn updates_preflight_identity_and_publish_only_fixed_status_output() {
    let f = fixture().await;
    for result in [
        CheckResult::Pending,
        CheckResult::Failure,
        CheckResult::Success,
    ] {
        let run = f
            .adapter
            .update_check(&scope(), id(), OP, result)
            .await
            .unwrap();
        assert_eq!(run.state, CheckState::from(result));
        let writes = f.state.writes.lock().unwrap();
        let body = &writes.last().unwrap().1;
        assert_eq!(writes.last().unwrap().0, Method::PATCH);
        assert!(
            body.get("head_sha").is_none()
                && body.get("external_id").is_none()
                && body.get("name").is_none()
        );
        assert_eq!(body["output"]["title"], result.title());
        assert_eq!(
            body["status"],
            if result == CheckResult::Pending {
                "in_progress"
            } else {
                "completed"
            }
        );
    }
    assert_eq!(f.state.reads.load(Ordering::SeqCst), 3);
    assert!(matches!(
        f.adapter
            .update_check(&scope(), id(), Uuid::from_u128(2), CheckResult::Success)
            .await,
        Err(CheckError::IdentityMismatch)
    ));
    assert_eq!(f.state.writes.lock().unwrap().len(), 3);
}
#[tokio::test]
async fn malformed_or_unbound_evidence_cannot_become_a_successful_read() {
    let f = fixture().await;
    for (key, value) in [
        ("id", json!(82)),
        ("head_sha", json!("2".repeat(40))),
        ("name", json!("other")),
        ("external_id", json!("not-an-operation")),
        (
            "external_id",
            json!("specspace-review-policy:00000000-0000-0000-0000-000000000000"),
        ),
        ("status", json!("completed")),
        ("conclusion", json!("success")),
    ] {
        let mut v = row(81);
        v[key] = value;
        *f.state.row.lock().unwrap() = v;
        assert!(f.adapter.check_run(&scope(), id()).await.is_err(), "{key}");
    }
    let mut v = row(81);
    v["app"]["id"] = json!(8);
    *f.state.row.lock().unwrap() = v;
    assert!(matches!(
        f.adapter.check_run(&scope(), id()).await,
        Err(CheckError::IdentityMismatch)
    ));
    for result in [
        "neutral",
        "skipped",
        "cancelled",
        "timed_out",
        "action_required",
        "stale",
    ] {
        let mut v = row(81);
        v["status"] = json!("completed");
        v["conclusion"] = json!(result);
        v["completed_at"] = json!("2026-11-25T12:00:00Z");
        *f.state.row.lock().unwrap() = v;
        assert_eq!(
            f.adapter.check_run(&scope(), id()).await.unwrap().state,
            CheckState::Other
        );
    }
}
#[tokio::test]
async fn scope_and_operation_validation_prevent_network_mutations() {
    let f = fixture().await;
    let wrong = CheckScope::new(43, 7, scope().head().clone()).unwrap();
    assert!(matches!(
        f.adapter.create_check(&wrong, OP).await,
        Err(CheckError::InvalidInput)
    ));
    assert!(matches!(
        f.adapter.create_check(&scope(), Uuid::nil()).await,
        Err(CheckError::InvalidInput)
    ));
    assert!(matches!(
        f.adapter
            .update_check(&scope(), id(), Uuid::nil(), CheckResult::Success)
            .await,
        Err(CheckError::InvalidInput)
    ));
    assert!(f.state.writes.lock().unwrap().is_empty());
    assert_eq!(f.state.reads.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn uncertain_creations_never_retry_or_return_a_forged_receipt() {
    let f = fixture().await;
    for mode in [1, 5, 6, 7, 8, 10, 11, 12, 13, 14] {
        f.state.mode.store(mode, Ordering::SeqCst);
        let before = f.state.writes.lock().unwrap().len();
        assert!(
            matches!(
                f.adapter.create_check(&scope(), OP).await,
                Err(CheckError::Provider(GitError::ResultUnknown))
            ),
            "mode {mode}"
        );
        assert_eq!(f.state.writes.lock().unwrap().len(), before + 1);
    }
}

#[tokio::test]
async fn updates_reject_uncertain_or_substituted_receipts_without_retrying() {
    let f = fixture().await;
    for mode in [8, 9, 10, 11, 12, 13, 14] {
        f.state.mode.store(mode, Ordering::SeqCst);
        let before = f.state.writes.lock().unwrap().len();
        assert!(
            matches!(
                f.adapter
                    .update_check(&scope(), id(), OP, CheckResult::Pending)
                    .await,
                Err(CheckError::Provider(GitError::ResultUnknown))
            ),
            "mode {mode}"
        );
        assert_eq!(f.state.writes.lock().unwrap().len(), before + 1);
    }
}

#[tokio::test]
async fn completed_success_cannot_be_reset_to_pending_without_explicit_invalidation() {
    let f = fixture().await;
    let mut v = row(81);
    v["status"] = json!("completed");
    v["conclusion"] = json!("success");
    v["completed_at"] = json!("2026-11-25T12:00:00Z");
    *f.state.row.lock().unwrap() = v;
    assert!(matches!(
        f.adapter
            .update_check(&scope(), id(), OP, CheckResult::Pending)
            .await,
        Err(CheckError::CannotRestart)
    ));
    assert!(f.state.writes.lock().unwrap().is_empty());
    assert_eq!(
        f.adapter
            .update_check(&scope(), id(), OP, CheckResult::Failure)
            .await
            .unwrap()
            .state,
        CheckState::Failure
    );
}
#[tokio::test]
async fn provider_backoff_denial_and_read_failures_stay_distinct() {
    let f = fixture().await;
    for (mode, expected) in [
        (1, CheckError::Provider(GitError::ProviderUnavailable)),
        (
            2,
            CheckError::Provider(GitError::RateLimited {
                retry_after_seconds: Some(17),
            }),
        ),
        (3, CheckError::NotFound),
        (4, CheckError::Provider(GitError::Unauthorized)),
        (6, CheckError::Provider(GitError::InvalidResponse)),
        (7, CheckError::InvalidResponse),
    ] {
        f.state.mode.store(mode, Ordering::SeqCst);
        assert!(matches!(f.adapter.check_run(&scope(), id()).await, Err(e) if e == expected));
    }
    f.state.mode.store(2, Ordering::SeqCst);
    assert!(matches!(
        f.adapter.create_check(&scope(), OP).await,
        Err(CheckError::Provider(GitError::RateLimited {
            retry_after_seconds: Some(17)
        }))
    ));
}
#[tokio::test]
async fn listings_include_all_evaluations_and_require_complete_bounded_pages() {
    let f = fixture().await;
    *f.state.listing.lock().unwrap() = json!({"total_count":2,"check_runs":[row(81)]});
    let first = f
        .adapter
        .check_runs(&scope(), CheckPage::new(1, 1).unwrap())
        .await
        .unwrap();
    assert_eq!(first.total_count, 2);
    assert_eq!(first.next, Some(CheckPage::new(2, 1).unwrap()));
    *f.state.listing.lock().unwrap() = json!({"total_count":2,"check_runs":[row(82)]});
    let last = f
        .adapter
        .check_runs(&scope(), first.next.unwrap())
        .await
        .unwrap();
    assert!(last.next.is_none());
    let query = f.state.queries.lock().unwrap()[0].clone();
    assert_eq!(query["check_name"], CHECK_NAME);
    assert_eq!(query["app_id"], "7");
    assert_eq!(query["filter"], "all");
    assert_eq!(query["per_page"], "1");
    for value in [
        json!({"total_count":1,"check_runs":[row(81),row(81)]}),
        json!({"total_count":3,"check_runs":[row(81)]}),
        json!({"total_count":0,"check_runs":[row(81)]}),
        json!({"total_count":2,"check_runs":[row(81),row(81)]}),
        json!({"check_runs":[]}),
    ] {
        *f.state.listing.lock().unwrap() = value;
        assert!(
            f.adapter
                .check_runs(&scope(), CheckPage::new(1, 2).unwrap())
                .await
                .is_err()
        );
    }
    *f.state.listing.lock().unwrap() = json!({"total_count":101,"check_runs":[row(81)]});
    assert!(matches!(
        f.adapter
            .check_runs(&scope(), CheckPage::new(100, 1).unwrap())
            .await,
        Err(CheckError::Provider(GitError::ResponseTooLarge))
    ));
}
