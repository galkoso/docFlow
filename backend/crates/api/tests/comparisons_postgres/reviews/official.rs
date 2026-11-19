use super::*;
async fn ready(pool: &PgPool, updated: bool) -> (Fixture, User, User, String, String) {
    let (f, product, development) = configured(pool, false).await;
    if updated {
        f.provider.base_updated.store(true, Ordering::SeqCst);
        sqlx::query("UPDATE proposals SET base_sha=$2 WHERE id=$1")
            .bind(f.proposal)
            .bind(MAIN)
            .execute(pool)
            .await
            .unwrap();
    }
    let (result, snapshot) = publish(&f, 2).await;
    let path = format!("{}/{snapshot}/decisions", history_path(&f));
    (
        f,
        product,
        development,
        path,
        result["review"]["fingerprint"].as_str().unwrap().into(),
    )
}
fn body(fp: &str, decision: &str) -> Value {
    json!({"request_id":Uuid::new_v4(),"expected_fingerprint":fp,"domain":"PRODUCT","decision":decision,"explanation":"Reviewed all saved changes"})
}
async fn post(
    f: &Fixture,
    user: &User,
    path: &str,
    body: Value,
) -> (StatusCode, Value, Option<String>) {
    post_raw(
        &f.app,
        Some(user),
        path,
        body.to_string(),
        PostOptions::new(user, "unused"),
    )
    .await
}
#[sqlx::test(migrations = "../../migrations")]
async fn shared_history_and_qualified_snapshot_decisions_keep_comments_and_replays_separate(
    pool: PgPool,
) {
    let (f, product, _, path, fp) = ready(&pool, true).await;
    let empty = get(&f.app, &f.actor, &path, true).await;
    assert_eq!(empty.0, StatusCode::OK);
    assert_eq!(empty.1["entries"], json!([]));
    assert_eq!(empty.1["domains"], json!([]));
    let hints = get(&f.app, &product, &path, true).await;
    assert_eq!(hints.1["domains"], json!(["PRODUCT"]));
    assert_eq!(hints.1["approve_all_available"], true);
    assert_eq!(hints.1["unviewed_content_warning"], true);
    let value = body(&fp, "APPROVE_ALL");
    let created = post(&f, &product, &path, value.clone()).await;
    assert_eq!(created.0, StatusCode::CREATED, "{}", created.1);
    assert_eq!(created.1["reviewer_id"], product.id.to_string());
    assert_eq!(created.1["head_sha"], HEAD);
    assert_eq!(created.1["target_sha"], MAIN);
    let reads = f.provider.ref_reads.load(Ordering::SeqCst);
    let replay = post(&f, &product, &path, value).await;
    assert_eq!(replay.0, StatusCode::OK);
    assert_eq!(replay.1["id"], created.1["id"]);
    assert_eq!(replay.1["replayed"], true);
    assert_eq!(f.provider.ref_reads.load(Ordering::SeqCst), reads);
    assert_eq!(
        post(&f, &product, &path, body(&fp, "COMMENT")).await.0,
        StatusCode::CREATED
    );
    let history = get(&f.app, &f.actor, &path, true).await;
    assert_eq!(history.1["entries"][0]["effective"], false);
    assert_eq!(history.1["entries"][1]["valid_for_stored_context"], true);
    sqlx::query(
        "UPDATE memberships SET review_domains='{}' WHERE organization_id=$1 AND user_id=$2",
    )
    .bind(f.organization)
    .bind(product.id)
    .execute(&pool)
    .await
    .unwrap();
    let revoked = get(&f.app, &f.actor, &path, true).await;
    assert_eq!(revoked.1["entries"][1]["effective"], true);
    assert_eq!(revoked.1["entries"][1]["reviewer_eligible"], false);
    assert_eq!(revoked.1["entries"][1]["valid_for_stored_context"], false);
    sqlx::query("UPDATE proposals SET state='CLOSED' WHERE id=$1")
        .bind(f.proposal)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        get(&f.app, &f.actor, &path, true).await.1["current_for_metadata"],
        false
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn decision_http_identity_csrf_assignment_and_strict_payloads_fail_before_provider_reads(
    pool: PgPool,
) {
    let (f, product, _, path, fp) = ready(&pool, true).await;
    let reads = f.provider.ref_reads.load(Ordering::SeqCst);
    for mode in 0..8 {
        let mut user = Some(&product);
        let mut options = PostOptions::new(&product, "unused");
        let mut uri = path.clone();
        let mut value = body(&fp, "APPROVE_ALL");
        let expected = match mode {
            0 => {
                user = None;
                StatusCode::UNAUTHORIZED
            }
            1 => {
                user = Some(&f.actor);
                options = PostOptions::new(&f.actor, "unused");
                StatusCode::FORBIDDEN
            }
            2 => {
                user = Some(&f.owner);
                options = PostOptions::new(&f.owner, "unused");
                StatusCode::FORBIDDEN
            }
            3 => {
                options.csrf = None;
                StatusCode::FORBIDDEN
            }
            4 => {
                options.origin = Some("https://evil.example");
                StatusCode::FORBIDDEN
            }
            5 => {
                options.expected = Some(f.owner.id);
                StatusCode::CONFLICT
            }
            6 => {
                uri.push_str("?head_sha=other");
                StatusCode::BAD_REQUEST
            }
            _ => {
                value["reviewer_id"] = json!(product.id);
                StatusCode::BAD_REQUEST
            }
        };
        let result = post_raw(&f.app, user, &uri, value.to_string(), options).await;
        assert_eq!(result.0, expected, "{mode}: {}", result.1);
    }
    assert_eq!(reads, f.provider.ref_reads.load(Ordering::SeqCst));
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM official_review_reads")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}
#[sqlx::test(migrations = "../../migrations")]
async fn history_queries_and_cursors_require_exact_current_reader_scope_without_provider_io(
    pool: PgPool,
) {
    let (f, product, _, path, fp) = ready(&pool, true).await;
    post(&f, &product, &path, body(&fp, "COMMENT")).await;
    let reads = f.provider.ref_reads.load(Ordering::SeqCst);
    for suffix in [
        "?head_sha=other".into(),
        format!("?after={}", Uuid::new_v4()),
        "?after=bad".into(),
    ] {
        assert_eq!(
            get(&f.app, &f.actor, &format!("{path}{suffix}"), true)
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        get(&f.app, &f.actor, &path, false).await.0,
        StatusCode::CONFLICT
    );
    let foreign = fixture(&pool, true).await;
    assert_eq!(
        get(&f.app, &foreign.owner, &path, true).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(reads, f.provider.ref_reads.load(Ordering::SeqCst));
}
#[sqlx::test(migrations = "../../migrations")]
async fn outdated_base_blocks_full_approval_but_allows_a_qualified_revision_request(pool: PgPool) {
    let (f, product, _, path, fp) = ready(&pool, false).await;
    assert_eq!(
        get(&f.app, &product, &path, true).await.1["approve_all_available"],
        false
    );
    let approve = post(&f, &product, &path, body(&fp, "APPROVE_ALL")).await;
    assert_eq!(approve.0, StatusCode::CONFLICT);
    assert_eq!(approve.1["error"]["code"], "REVIEW_BASE_OUTDATED");
    let mut empty = body(&fp, "REQUEST_CHANGES");
    empty["explanation"] = json!("  ");
    assert_eq!(
        post(&f, &product, &path, empty).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        post(&f, &product, &path, body(&fp, "REQUEST_CHANGES"))
            .await
            .0,
        StatusCode::CREATED
    );
    let state: String = sqlx::query_scalar("SELECT state FROM proposals WHERE id=$1")
        .bind(f.proposal)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state, "CHANGES_REQUESTED");
}
#[sqlx::test(migrations = "../../migrations")]
async fn provider_cooldown_preserves_retry_after_without_repeating_provider_reads(pool: PgPool) {
    let (f, product, development, path, fp) = ready(&pool, true).await;
    f.provider.backoff.store(1, Ordering::SeqCst);
    for (index, user) in [&product, &development].into_iter().enumerate() {
        let mut value = body(&fp, "COMMENT");
        if user.id == development.id {
            value["domain"] = json!("DEVELOPMENT");
        }
        let request = Request::builder()
            .method("POST")
            .uri(&path)
            .header("Content-Type", "application/json")
            .header("Cookie", format!("specspace-session={}", user.token))
            .header("X-Expected-Actor", user.id.to_string())
            .header("Origin", "http://localhost:4200")
            .header("X-XSRF-TOKEN", &user.csrf)
            .body(Body::from(value.to_string()))
            .unwrap();
        let response = f.app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        let delay: u32 = response.headers()["Retry-After"]
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        if index == 0 {
            assert_eq!(delay, 13);
        } else {
            assert!((1..=13).contains(&delay));
        }
        assert_eq!(response.headers()["cache-control"], "no-store");
    }
    assert_eq!(get(&f.app, &f.actor, &path, true).await.0, StatusCode::OK);
}
