use super::*;
use specspace_application::{
    proposal::UiImpact,
    review_configuration::{
        ReviewArea, ReviewConfigurationRequest, ReviewSettings, ReviewerSelection,
    },
};
use specspace_infrastructure::{
    review_configuration::PgReviewConfiguration, review_worker::ReviewWork,
};

async fn configured(pool: &PgPool, media: bool) -> (Fixture, User, User) {
    let f = fixture_review_options(
        pool,
        true,
        "PRIVATE SOURCE SENTINEL before",
        media,
        None,
        false,
        true,
    )
    .await;
    let product = user(pool, f.organization, "REVIEWER").await;
    let development = user(pool, f.organization, "REVIEWER").await;
    for (id, domain) in [(product.id, "PRODUCT"), (development.id, "DEVELOPMENT")] {
        sqlx::query("UPDATE memberships SET review_domains=ARRAY[$3::text] WHERE organization_id=$1 AND user_id=$2").bind(f.organization).bind(id).bind(domain).execute(pool).await.unwrap();
    }
    PgReviewConfiguration::new(pool.clone())
        .update(
            f.owner.id,
            f.proposal,
            ReviewConfigurationRequest {
                request_id: Uuid::new_v4(),
                expected_head_sha: HEAD.into(),
                expected_proposal_version: 1,
                expected_configuration_version: 1,
                settings: ReviewSettings {
                    ui_impact: UiImpact::No,
                    ui_impact_reason: Some("No UI changes".into()),
                    assignments: vec![
                        ReviewerSelection {
                            reviewer_id: product.id,
                            domain: ReviewArea::Product,
                            required: true,
                        },
                        ReviewerSelection {
                            reviewer_id: development.id,
                            domain: ReviewArea::Development,
                            required: true,
                        },
                    ],
                },
            },
        )
        .await
        .unwrap();
    (f, product, development)
}
fn intent(version: i64) -> Value {
    json!({"expected_head_sha":HEAD,"expected_target_sha":MAIN,"expected_proposal_version":version,"expected_configuration_version":2,"expected_policy_version":1})
}
#[derive(Clone)]
struct PostOptions<'a> {
    key: Option<&'a str>,
    expected: Option<Uuid>,
    origin: Option<&'a str>,
    csrf: Option<&'a str>,
}
impl<'a> PostOptions<'a> {
    fn new(user: &'a User, key: &'a str) -> Self {
        Self {
            key: Some(key),
            expected: Some(user.id),
            origin: Some("http://localhost:4200"),
            csrf: Some(&user.csrf),
        }
    }
}
async fn post_raw(
    app: &Router,
    user: Option<&User>,
    uri: &str,
    body: String,
    options: PostOptions<'_>,
) -> (StatusCode, Value, Option<String>) {
    let mut request = Request::builder()
        .method("POST")
        .uri(uri)
        .header("Content-Type", "application/json");
    if let Some(user) = user {
        request = request.header("Cookie", format!("specspace-session={}", user.token));
    }
    if let Some(key) = options.key {
        request = request.header("Idempotency-Key", key);
    }
    if let Some(actor) = options.expected {
        request = request.header("X-Expected-Actor", actor.to_string());
    }
    if let Some(origin) = options.origin {
        request = request.header("Origin", origin);
    }
    if let Some(csrf) = options.csrf {
        request = request.header("X-XSRF-TOKEN", csrf);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    assert_eq!(response.headers()["cache-control"], "no-store");
    let status = response.status();
    if status == StatusCode::ACCEPTED {
        assert_eq!(response.headers()["Retry-After"], "2");
    }
    if status == StatusCode::TOO_MANY_REQUESTS {
        assert_eq!(response.headers()["Retry-After"], "60");
    }
    let location = response
        .headers()
        .get("Location")
        .map(|value| value.to_str().unwrap().to_owned());
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap(), location)
}
async fn submit(
    f: &Fixture,
    user: &User,
    version: i64,
    key: &str,
) -> (StatusCode, Value, Option<String>) {
    post_raw(
        &f.app,
        Some(user),
        &format!("/api/v1/proposals/{}/submit", f.proposal),
        intent(version).to_string(),
        PostOptions::new(user, key),
    )
    .await
}
fn history_path(f: &Fixture) -> String {
    format!("/api/v1/proposals/{}/review/snapshots", f.proposal)
}
async fn publish(f: &Fixture, version: i64) -> (Value, String) {
    let (status, ticket, location) =
        submit(f, &f.owner, version, &Uuid::new_v4().to_string()).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{ticket}");
    let job = Uuid::parse_str(ticket["job_id"].as_str().unwrap()).unwrap();
    assert_eq!(
        f.review_worker.run_once().await.unwrap(),
        ReviewWork::Completed(job)
    );
    let result = get(&f.app, &f.owner, &location.unwrap(), true).await;
    assert_eq!(result.0, StatusCode::OK);
    assert_eq!(result.1["phase"], "SNAPSHOT_READY");
    let snapshot = result.1["review"]["snapshot_id"]
        .as_str()
        .unwrap()
        .to_owned();
    (result.1, snapshot)
}

#[sqlx::test(migrations = "../../migrations")]
async fn review_http_queue_worker_shared_evidence_and_actor_private_progress(pool: PgPool) {
    let (f, product, _) = configured(&pool, true).await;
    let empty = get(&f.app, &f.actor, &history_path(&f), true).await;
    assert_eq!(empty.0, StatusCode::OK);
    assert_eq!(empty.1["snapshots"], json!([]));
    assert!(empty.1["active_snapshot_id"].is_null());
    assert_eq!(f.provider.reads.load(Ordering::SeqCst), 0);
    let key = Uuid::new_v4().to_string();
    let (a, b) = tokio::join!(submit(&f, &f.owner, 2, &key), submit(&f, &f.owner, 2, &key));
    assert_eq!(a.0, StatusCode::ACCEPTED);
    assert_eq!(a.1, b.1);
    assert_eq!(a.2, b.2);
    let job = Uuid::parse_str(a.1["job_id"].as_str().unwrap()).unwrap();
    let location = a.2.unwrap();
    assert_eq!(location, format!("/api/v1/jobs/{job}"));
    let queued = get(&f.app, &f.owner, &location, true).await;
    assert_eq!(queued.0, StatusCode::OK);
    assert_eq!(queued.1["state"], "QUEUED");
    assert_eq!(queued.1["phase"], "QUEUED");
    assert!(queued.1["review"].is_null());
    assert_eq!(
        get(&f.app, &f.owner, &location, false).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        get(&f.app, &product, &location, true).await.0,
        StatusCode::NOT_FOUND
    );
    let admin = user(&pool, f.organization, "ADMIN").await;
    assert_eq!(
        get(&f.app, &admin, &location, true).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(f.provider.reads.load(Ordering::SeqCst), 0);
    assert_eq!(
        f.review_worker.run_once().await.unwrap(),
        ReviewWork::Completed(job)
    );
    let ready = get(&f.app, &f.owner, &location, true).await;
    assert_eq!(ready.1["state"], "SUCCEEDED");
    assert_eq!(ready.1["phase"], "SNAPSHOT_READY");
    assert_eq!(ready.1["review"]["proposal_version"], 3);
    let snapshot = ready.1["review"]["snapshot_id"].as_str().unwrap();
    let path = format!("{}/{snapshot}", history_path(&f));
    let reads = f.provider.reads.load(Ordering::SeqCst);
    let metadata = get(&f.app, &f.actor, &path, true).await;
    assert_eq!(metadata.0, StatusCode::OK, "{}", metadata.1);
    assert_eq!(metadata.1["current_metadata"], true);
    assert_eq!(metadata.1["metadata"]["schema_version"], 1);
    assert_eq!(metadata.1["file_count"], 4);
    assert_eq!(metadata.1["media_count"], 1);
    assert_eq!(metadata.1["created_by"], f.owner.id.to_string());
    sqlx::types::chrono::DateTime::parse_from_rfc3339(metadata.1["created_at"].as_str().unwrap())
        .unwrap();
    assert_eq!(metadata.1["metadata"]["summary"]["head_sha"], HEAD);
    assert_eq!(metadata.1["metadata"]["summary"]["target_sha"], MAIN);
    assert_eq!(
        metadata.1["metadata"]["assignments"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let first = get(&f.app, &f.actor, &format!("{path}/files?limit=2"), true).await;
    assert_eq!(first.0, StatusCode::OK);
    assert_eq!(first.1["total"], 4);
    assert_eq!(first.1["next_offset"], 2);
    let last = get(
        &f.app,
        &product,
        &format!("{path}/files?offset=2&limit=2"),
        true,
    )
    .await;
    assert_eq!(last.0, StatusCode::OK);
    assert_eq!(last.1["files"].as_array().unwrap().len(), 2);
    assert!(last.1["next_offset"].is_null());
    let media = get(&f.app, &f.actor, &format!("{path}/media?limit=1"), true).await;
    assert_eq!(media.0, StatusCode::OK);
    assert_eq!(media.1["total"], 1);
    assert_eq!(media.1["receipts"].as_array().unwrap().len(), 1);
    assert!(media.1["receipts"][0]["display_width"].as_u64().unwrap() > 0);
    assert!(
        get(&f.app, &f.actor, &format!("{path}/media?offset=1"), true)
            .await
            .1["receipts"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let history = get(&f.app, &f.actor, &history_path(&f), true).await;
    assert_eq!(history.1["active_snapshot_id"], snapshot);
    assert_eq!(history.1["snapshots"][0]["snapshot_id"], snapshot);
    for body in [metadata.1, first.1, last.1, media.1, history.1, ready.1] {
        let text = body.to_string();
        for secret in [
            "PRIVATE SOURCE SENTINEL",
            "installation_id",
            "remote_id",
            "scope_hash",
            "request_hash",
            "payload",
        ] {
            assert!(!text.contains(secret), "{secret}");
        }
    }
    assert_eq!(f.provider.reads.load(Ordering::SeqCst), reads);
    assert_eq!(submit(&f, &f.owner, 2, &key).await.1, a.1);
    assert_eq!(f.provider.reads.load(Ordering::SeqCst), reads);
}

#[sqlx::test(migrations = "../../migrations")]
async fn review_http_write_authentication_csrf_actor_and_scope_cannot_queue_work(pool: PgPool) {
    let (f, product, _) = configured(&pool, false).await;
    let key = Uuid::new_v4().to_string();
    let uri = format!("/api/v1/proposals/{}/submit", f.proposal);
    let base = PostOptions::new(&f.owner, &key);
    assert_eq!(
        post_raw(&f.app, None, &uri, intent(2).to_string(), base.clone())
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    for fault in 0..5 {
        let mut options = base.clone();
        let expected = match fault {
            0 => {
                options.expected = None;
                StatusCode::CONFLICT
            }
            1 => {
                options.expected = Some(product.id);
                StatusCode::CONFLICT
            }
            2 => {
                options.origin = Some("https://untrusted.example");
                StatusCode::FORBIDDEN
            }
            3 => {
                options.csrf = Some("wrong");
                StatusCode::FORBIDDEN
            }
            _ => {
                options.key = None;
                StatusCode::BAD_REQUEST
            }
        };
        assert_eq!(
            post_raw(&f.app, Some(&f.owner), &uri, intent(2).to_string(), options)
                .await
                .0,
            expected
        );
    }
    assert_eq!(submit(&f, &f.actor, 2, &key).await.0, StatusCode::FORBIDDEN);
    assert_eq!(submit(&f, &product, 2, &key).await.0, StatusCode::FORBIDDEN);
    let other = fixture(&pool, true).await;
    assert_eq!(
        submit(&f, &other.owner, 2, &key).await.0,
        StatusCode::NOT_FOUND
    );
    let (jobs, ops): (i64, i64) =
        sqlx::query_as("SELECT (SELECT count(*) FROM jobs),(SELECT count(*) FROM git_operations)")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((jobs, ops), (0, 0));
    assert_eq!(f.provider.reads.load(Ordering::SeqCst), 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn review_http_malformed_and_stale_intents_never_reserve_work(pool: PgPool) {
    let (f, _, _) = configured(&pool, false).await;
    let key = Uuid::new_v4().to_string();
    let uri = format!("/api/v1/proposals/{}/submit", f.proposal);
    for fault in 0..10 {
        let mut value = intent(2);
        let (body, status) = match fault {
            0 => ("{".into(), StatusCode::BAD_REQUEST),
            1 => {
                value["actor_id"] = json!(f.owner.id);
                (value.to_string(), StatusCode::BAD_REQUEST)
            }
            2 => {
                value["expected_proposal_version"] = json!(0);
                (value.to_string(), StatusCode::UNPROCESSABLE_ENTITY)
            }
            3 => {
                value["expected_head_sha"] = json!("not-a-sha");
                (value.to_string(), StatusCode::UNPROCESSABLE_ENTITY)
            }
            4 => {
                value["expected_proposal_version"] = json!(3);
                (value.to_string(), StatusCode::CONFLICT)
            }
            5 => {
                value["expected_configuration_version"] = json!(3);
                (value.to_string(), StatusCode::CONFLICT)
            }
            6 => {
                value["expected_policy_version"] = json!(2);
                (value.to_string(), StatusCode::CONFLICT)
            }
            7 => {
                value["expected_head_sha"] = json!(OLD);
                (value.to_string(), StatusCode::CONFLICT)
            }
            8 => (" ".repeat(9000), StatusCode::PAYLOAD_TOO_LARGE),
            _ => (
                value.to_string().replace(
                    "\"expected_proposal_version\":2",
                    "\"expected_proposal_version\":2,\"expected_proposal_version\":2",
                ),
                StatusCode::BAD_REQUEST,
            ),
        };
        let response = post_raw(
            &f.app,
            Some(&f.owner),
            &uri,
            body,
            PostOptions::new(&f.owner, &key),
        )
        .await;
        assert_eq!(response.0, status, "fault {fault}: {}", response.1);
    }
    assert_eq!(
        post_raw(
            &f.app,
            Some(&f.owner),
            &format!("{uri}?head={HEAD}"),
            intent(2).to_string(),
            PostOptions::new(&f.owner, &key)
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM review_submissions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn review_http_replay_busy_terminal_and_changed_key_behavior_is_explicit(pool: PgPool) {
    let (f, _, _) = configured(&pool, false).await;
    let key = Uuid::new_v4().to_string();
    let first = submit(&f, &f.owner, 2, &key).await;
    assert_eq!(first.0, StatusCode::ACCEPTED);
    assert_eq!(submit(&f, &f.owner, 3, &key).await.0, StatusCode::CONFLICT);
    assert_eq!(
        submit(&f, &f.owner, 2, &Uuid::new_v4().to_string()).await.1["error"]["code"],
        "PROPOSAL_BUSY"
    );
    let job = Uuid::parse_str(first.1["job_id"].as_str().unwrap()).unwrap();
    assert_eq!(
        f.review_worker.run_once().await.unwrap(),
        ReviewWork::Completed(job)
    );
    sqlx::query("UPDATE proposals SET state='CLOSED',version=version+1 WHERE id=$1")
        .bind(f.proposal)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(submit(&f, &f.owner, 2, &key).await.1, first.1);
    assert_eq!(
        submit(&f, &f.owner, 4, &Uuid::new_v4().to_string()).await.1["error"]["code"],
        "PROPOSAL_NOT_EDITABLE"
    );
    let status = get(&f.app, &f.owner, &first.2.unwrap(), true).await;
    assert_eq!(status.1["phase"], "SNAPSHOT_READY");
}

#[sqlx::test(migrations = "../../migrations")]
async fn review_http_shared_history_is_scoped_strict_and_available_without_git(pool: PgPool) {
    let (f, _, _) = configured(&pool, true).await;
    let (_, snapshot) = publish(&f, 2).await;
    let path = format!("{}/{snapshot}", history_path(&f));
    let original = get(&f.app, &f.actor, &path, true).await.1;
    let reads = f.provider.reads.load(Ordering::SeqCst);
    let foreign = fixture(&pool, true).await;
    for uri in [
        history_path(&f),
        path.clone(),
        format!("{path}/files"),
        format!("{path}/media"),
    ] {
        assert_eq!(
            get(&f.app, &foreign.owner, &uri, true).await.0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            get(&f.app, &f.actor, &uri, false).await.0,
            StatusCode::CONFLICT
        );
        let response = f
            .app
            .clone()
            .oneshot(Request::builder().uri(&uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    for uri in [
        format!("{path}?head={HEAD}"),
        format!("{path}/files?offset=-1"),
        format!("{path}/files?limit=101"),
        format!("{path}/files?offset=0&offset=1"),
        format!("{path}/files?actor_id={}", f.owner.id),
        format!("{path}/media?offset=20001"),
        format!("{path}/media?limit=0"),
        format!("{}?limit=51", history_path(&f)),
        format!("{}?after=invalid", history_path(&f)),
    ] {
        assert_eq!(
            get(&f.app, &f.actor, &uri, true).await.0,
            StatusCode::BAD_REQUEST,
            "{uri}"
        );
    }
    assert_eq!(
        get(
            &f.app,
            &f.actor,
            &format!("{}?after={}", history_path(&f), Uuid::new_v4()),
            true
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    sqlx::query("UPDATE proposals SET head_sha=$2,version=version+1,state='CLOSED' WHERE id=$1")
        .bind(f.proposal)
        .bind(OLD)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE repositories SET remote_id='unavailable' WHERE id=(SELECT repository_id FROM proposals WHERE id=$1)").bind(f.proposal).execute(&pool).await.unwrap();
    let old = get(&f.app, &f.actor, &path, true).await;
    assert_eq!(old.0, StatusCode::OK);
    assert_eq!(old.1["metadata"], original["metadata"]);
    assert_eq!(old.1["current_metadata"], false);
    assert_eq!(
        get(&f.app, &f.actor, &format!("{path}/files"), true)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        get(&f.app, &f.actor, &format!("{path}/media"), true)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        get(&f.app, &f.actor, &history_path(&f), true).await.1["current_proposal_state"],
        "CLOSED"
    );
    assert_eq!(f.provider.reads.load(Ordering::SeqCst), reads);
    sqlx::query("DELETE FROM memberships WHERE organization_id=$1 AND user_id=$2")
        .bind(f.organization)
        .bind(f.actor.id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        get(&f.app, &f.actor, &path, true).await.0,
        StatusCode::UNAUTHORIZED
    );
}

async fn invalidate_and_restore(pool: &PgPool, f: &Fixture) -> i64 {
    sqlx::query("UPDATE proposals SET head_sha=$2,version=version+1 WHERE id=$1")
        .bind(f.proposal)
        .bind(OLD)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query_scalar(
        "UPDATE proposals SET head_sha=$2,version=version+1 WHERE id=$1 RETURNING version",
    )
    .bind(f.proposal)
    .bind(HEAD)
    .fetch_one(pool)
    .await
    .unwrap()
}
#[sqlx::test(migrations = "../../migrations")]
async fn review_http_history_cursor_is_stable_when_new_snapshots_arrive_and_reuse_does_not_duplicate_history(
    pool: PgPool,
) {
    let (f, _, _) = configured(&pool, false).await;
    let (_, first) = publish(&f, 2).await;
    let version = invalidate_and_restore(&pool, &f).await;
    let (_, second) = publish(&f, version).await;
    let page = get(
        &f.app,
        &f.actor,
        &format!("{}?limit=1", history_path(&f)),
        true,
    )
    .await;
    assert_eq!(page.0, StatusCode::OK);
    assert_eq!(page.1["snapshots"][0]["snapshot_id"], second);
    assert_eq!(page.1["next_after"], second);
    let version = invalidate_and_restore(&pool, &f).await;
    let (_, third) = publish(&f, version).await;
    let later = get(
        &f.app,
        &f.actor,
        &format!("{}?after={second}&limit=1", history_path(&f)),
        true,
    )
    .await;
    assert_eq!(later.0, StatusCode::OK);
    assert_eq!(later.1["snapshots"][0]["snapshot_id"], first);
    assert!(later.1["next_after"].is_null());
    assert_eq!(later.1["active_snapshot_id"], third);
    let (reused, snapshot) = publish(&f, version + 1).await;
    assert_eq!(snapshot, third);
    assert_eq!(reused["review"]["activated"], false);
    assert_eq!(reused["review"]["proposal_version"], version + 1);
    let all = get(&f.app, &f.actor, &history_path(&f), true).await;
    assert_eq!(all.1["snapshots"].as_array().unwrap().len(), 3);
    let empty_media = get(
        &f.app,
        &f.actor,
        &format!("{}/{third}/media", history_path(&f)),
        true,
    )
    .await;
    assert_eq!(empty_media.0, StatusCode::OK);
    assert_eq!(empty_media.1["total"], 0);
    assert!(empty_media.1["receipts"].as_array().unwrap().is_empty());
}

#[sqlx::test(migrations = "../../migrations")]
async fn review_http_job_rechecks_authority_and_rejects_unbacked_success_or_ambiguous_queries(
    pool: PgPool,
) {
    let (f, _, _) = configured(&pool, false).await;
    let ticket = submit(&f, &f.owner, 2, &Uuid::new_v4().to_string()).await;
    let location = ticket.2.unwrap();
    assert_eq!(
        get(
            &f.app,
            &f.owner,
            &format!("{location}?actor_id={}", f.owner.id),
            true
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    sqlx::query("UPDATE jobs SET state='SUCCEEDED',result=jsonb_build_object('phase','SNAPSHOT_READY','snapshot_id',$2::uuid) WHERE id=$1")
        .bind(Uuid::parse_str(ticket.1["job_id"].as_str().unwrap()).unwrap()).bind(Uuid::new_v4()).execute(&pool).await.unwrap();
    sqlx::query("UPDATE git_operations SET state='SUCCEEDED' WHERE id=$1")
        .bind(Uuid::parse_str(ticket.1["operation_id"].as_str().unwrap()).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    let corrupt = get(&f.app, &f.owner, &location, true).await;
    assert_eq!(corrupt.0, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(corrupt.1["error"]["code"], "REVIEW_EVIDENCE_INVALID");
    sqlx::query("UPDATE memberships SET role='READER' WHERE organization_id=$1 AND user_id=$2")
        .bind(f.organization)
        .bind(f.owner.id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        get(&f.app, &f.owner, &location, true).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        submit(&f, &f.owner, 2, &Uuid::new_v4().to_string()).await.0,
        StatusCode::FORBIDDEN
    );
}

async fn another_proposal(pool: &PgPool, f: &Fixture, n: usize) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO proposals(id,organization_id,repository_id,title,description,branch,owner_id,author_id,team_id,state,ui_impact,base_sha,head_sha,target_sha) SELECT $2,organization_id,repository_id,title,description,$3,owner_id,author_id,team_id,'DRAFT','UNDECIDED',base_sha,head_sha,target_sha FROM proposals WHERE id=$1")
        .bind(f.proposal).bind(id).bind(format!("change/quota-{n}")).execute(pool).await.unwrap();
    id
}
async fn submit_to(
    f: &Fixture,
    user: &User,
    proposal: Uuid,
    key: &str,
) -> (StatusCode, Value, Option<String>) {
    let mut request = intent(1);
    request["expected_configuration_version"] = json!(1);
    post_raw(
        &f.app,
        Some(user),
        &format!("/api/v1/proposals/{proposal}/submit"),
        request.to_string(),
        PostOptions::new(user, key),
    )
    .await
}
async fn fail_next(pool: &PgPool) {
    let store =
        specspace_infrastructure::review_submissions::PgReviewSubmissions::new(pool.clone());
    let lease = store.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    store.fail(&lease, "TEST_REJECTION", None).await.unwrap();
}
#[sqlx::test(migrations = "../../migrations")]
async fn review_http_concurrent_actor_admission_is_bounded_but_exact_replay_remains_available(
    pool: PgPool,
) {
    let (f, _, _) = configured(&pool, false).await;
    let p1 = another_proposal(&pool, &f, 1).await;
    let p2 = another_proposal(&pool, &f, 2).await;
    let p3 = another_proposal(&pool, &f, 3).await;
    let (a, b, c) = tokio::join!(
        submit_to(&f, &f.owner, p1, "quota-1"),
        submit_to(&f, &f.owner, p2, "quota-2"),
        submit_to(&f, &f.owner, p3, "quota-3")
    );
    assert_eq!(
        [a.0, b.0, c.0]
            .into_iter()
            .filter(|status| *status == StatusCode::ACCEPTED)
            .count(),
        2
    );
    assert_eq!(
        [a.0, b.0, c.0]
            .into_iter()
            .filter(|status| *status == StatusCode::TOO_MANY_REQUESTS)
            .count(),
        1
    );
    for (response, proposal, key) in [(a, p1, "quota-1"), (b, p2, "quota-2"), (c, p3, "quota-3")] {
        if response.0 == StatusCode::ACCEPTED {
            assert_eq!(submit_to(&f, &f.owner, proposal, key).await.1, response.1);
        } else {
            assert_eq!(response.1["error"]["code"], "REVIEW_SUBMISSION_LIMIT");
        }
    }
    assert_eq!(f.provider.reads.load(Ordering::SeqCst), 0);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM review_submissions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 2);
}
#[sqlx::test(migrations = "../../migrations")]
async fn review_http_recent_actor_quota_counts_terminal_jobs_and_still_replays_the_first_ticket(
    pool: PgPool,
) {
    let (f, _, _) = configured(&pool, false).await;
    let mut first = Value::Null;
    for n in 0..10 {
        let response = submit(&f, &f.owner, 2, &format!("recent-{n}")).await;
        assert_eq!(response.0, StatusCode::ACCEPTED, "{}", response.1);
        if n == 0 {
            first = response.1;
        }
        fail_next(&pool).await;
    }
    assert_eq!(
        submit(&f, &f.owner, 2, "recent-10").await.0,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(submit(&f, &f.owner, 2, "recent-0").await.1, first);
}
#[sqlx::test(migrations = "../../migrations")]
async fn review_http_organization_active_quota_applies_across_independent_actors(pool: PgPool) {
    let (f, _, _) = configured(&pool, false).await;
    for actor in 0..6 {
        let admin = user(&pool, f.organization, "ADMIN").await;
        for n in 0..2 {
            let proposal = another_proposal(&pool, &f, actor * 2 + n).await;
            assert_eq!(
                submit_to(&f, &admin, proposal, &Uuid::new_v4().to_string())
                    .await
                    .0,
                StatusCode::ACCEPTED
            );
        }
    }
    let last = user(&pool, f.organization, "ADMIN").await;
    let proposal = another_proposal(&pool, &f, 13).await;
    assert_eq!(
        submit_to(&f, &last, proposal, "org-quota").await.0,
        StatusCode::TOO_MANY_REQUESTS
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn review_http_organization_recent_quota_survives_actor_changes_and_terminal_jobs(
    pool: PgPool,
) {
    let (f, _, _) = configured(&pool, false).await;
    for actor in 0..6 {
        let admin = user(&pool, f.organization, "ADMIN").await;
        for n in 0..10 {
            let response = submit(&f, &admin, 2, &format!("org-recent-{actor}-{n}")).await;
            assert_eq!(response.0, StatusCode::ACCEPTED, "{}", response.1);
            fail_next(&pool).await;
        }
    }
    let last = user(&pool, f.organization, "ADMIN").await;
    assert_eq!(
        submit(&f, &last, 2, "org-recent-limit").await.0,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn proposal_detail_honors_an_optional_captured_actor_before_review_preflight(pool: PgPool) {
    let (f, product, _) = configured(&pool, false).await;
    for actors in [vec![product.id], vec![f.owner.id, f.owner.id]] {
        let mut request = Request::builder()
            .uri(format!("/api/v1/proposals/{}", f.proposal))
            .header("Cookie", format!("specspace-session={}", f.owner.token));
        for actor in actors {
            request = request.header("X-Expected-Actor", actor.to_string());
        }
        let response = f
            .app
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }
    assert_eq!(
        get(
            &f.app,
            &f.owner,
            &format!("/api/v1/proposals/{}", f.proposal),
            true
        )
        .await
        .0,
        StatusCode::OK
    );
}
