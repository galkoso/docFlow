use super::*;
fn root(f: &Fixture) -> String {
    format!("/api/v1/proposals/{}/questions", f.proposal)
}
fn body(version: i64) -> Value {
    json!({"request_id":Uuid::new_v4(),"expected_proposal_version":version,"text":"שאלת מוצר\r\n🦀","blocking":true,"proposed_answer":"Suggested answer"})
}
fn action(version: i64, answer: &str) -> Value {
    json!({"request_id":Uuid::new_v4(),"expected_version":version,"action":{"kind":"decide","answer":answer}})
}
async fn post(
    f: &Fixture,
    user: &User,
    path: &str,
    value: Value,
) -> (StatusCode, Value, Option<String>) {
    post_raw(
        &f.app,
        Some(user),
        path,
        value.to_string(),
        PostOptions::new(user, "unused"),
    )
    .await
}
#[sqlx::test(migrations = "../../migrations")]
async fn question_http_requires_explicit_product_decisions_and_replays_original_receipts(
    pool: PgPool,
) {
    let (f, product, development) = configured(&pool, false).await;
    let path = root(&f);
    let request = body(2);
    let created = post(&f, &f.owner, &path, request.clone()).await;
    assert_eq!(created.0, StatusCode::CREATED, "{}", created.1);
    let replay = post(&f, &f.owner, &path, request).await;
    assert_eq!(replay.0, StatusCode::OK);
    assert_eq!(replay.1["id"], created.1["id"]);
    assert_eq!(replay.1["replayed"], true);
    let q = created.1["question_id"].as_str().unwrap();
    let history = format!("{path}/{q}");
    let actions = format!("{history}/actions");
    let list = get(&f.app, &f.owner, &path, true).await;
    assert_eq!(list.0, StatusCode::OK);
    assert_eq!(list.1["unresolved_blockers"], 1);
    assert_eq!(list.1["questions"][0]["source"], "HUMAN");
    assert_eq!(list.1["questions"][0]["current"]["state"], "PROPOSED");
    assert!(list.1["questions"][0]["current"]["decision"].is_null());
    let decide = action(1, "Explicit Product answer");
    assert_eq!(
        post(&f, &development, &actions, decide.clone()).await.0,
        StatusCode::FORBIDDEN
    );
    let recorded = post(&f, &product, &actions, decide.clone()).await;
    assert_eq!(recorded.0, StatusCode::CREATED, "{}", recorded.1);
    assert_eq!(
        post(&f, &product, &actions, decide.clone()).await.0,
        StatusCode::OK
    );
    let mut changed = decide;
    changed["action"]["answer"] = json!("Changed intent");
    assert_eq!(
        post(&f, &product, &actions, changed).await.0,
        StatusCode::CONFLICT
    );
    let saved = get(&f.app, &f.owner, &history, true).await;
    assert_eq!(saved.0, StatusCode::OK);
    assert_eq!(saved.1["revisions"].as_array().unwrap().len(), 2);
    assert_eq!(
        saved.1["question"]["current"]["decision"]["actor"]["id"],
        product.id.to_string()
    );
    assert_eq!(saved.1["revisions"][0]["state"], "PROPOSED");
    assert_eq!(
        get(&f.app, &f.owner, &path, true).await.1["unresolved_blockers"],
        0
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn question_http_identity_scope_csrf_and_strict_bodies_reject_claimed_authority_before_writes(
    pool: PgPool,
) {
    let (f, _, _) = configured(&pool, false).await;
    let path = root(&f);
    let reads = f.provider.ref_reads.load(Ordering::SeqCst);
    for mode in 0..10 {
        let mut actor = Some(&f.owner);
        let mut options = PostOptions::new(&f.owner, "unused");
        let mut value = body(2);
        let mut uri = path.clone();
        let expected = match mode {
            0 => {
                actor = None;
                StatusCode::UNAUTHORIZED
            }
            1 => {
                options.expected = Some(f.actor.id);
                StatusCode::CONFLICT
            }
            2 => {
                options.csrf = None;
                StatusCode::FORBIDDEN
            }
            3 => {
                options.origin = Some("https://evil.example");
                StatusCode::FORBIDDEN
            }
            4 => {
                value["source"] = json!("IMPORTED");
                StatusCode::BAD_REQUEST
            }
            5 => {
                value["state"] = json!("DECIDED");
                StatusCode::BAD_REQUEST
            }
            6 => {
                value["decided_by"] = json!(f.owner.id);
                StatusCode::BAD_REQUEST
            }
            7 => {
                uri.push_str("?passed=true");
                StatusCode::BAD_REQUEST
            }
            8 => {
                value["expected_proposal_version"] = json!(1);
                StatusCode::CONFLICT
            }
            _ => {
                value["text"] = json!("x".repeat(270000));
                StatusCode::PAYLOAD_TOO_LARGE
            }
        };
        let result = post_raw(&f.app, actor, &uri, value.to_string(), options).await;
        assert_eq!(result.0, expected, "mode {mode}: {}", result.1);
    }
    let reader = user(&pool, f.organization, "READER").await;
    assert_eq!(
        post(&f, &reader, &path, body(2)).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        get(&f.app, &f.owner, &format!("{path}?after=invalid"), true)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        get(&f.app, &f.owner, &path, false).await.0,
        StatusCode::CONFLICT
    );
    let foreign = fixture(&pool, true).await;
    assert_eq!(
        get(&f.app, &foreign.owner, &path, true).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM product_questions")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(f.provider.ref_reads.load(Ordering::SeqCst), reads);
}
#[sqlx::test(migrations = "../../migrations")]
async fn question_http_updates_readiness_from_real_latest_state_without_touching_git_or_official_votes(
    pool: PgPool,
) {
    let (f, product, _) = configured(&pool, false).await;
    f.provider.base_updated.store(true, Ordering::SeqCst);
    sqlx::query("UPDATE proposals SET base_sha=$2 WHERE id=$1")
        .bind(f.proposal)
        .bind(MAIN)
        .execute(&pool)
        .await
        .unwrap();
    let (_, snapshot) = publish(&f, 2).await;
    let ready = format!("{}/{snapshot}/readiness", history_path(&f));
    let path = root(&f);
    let list = get(&f.app, &f.owner, &path, true).await;
    let version = list.1["proposal_version"].as_i64().unwrap();
    let reads = f.provider.ref_reads.load(Ordering::SeqCst);
    let created = post(&f, &product, &path, body(version)).await;
    assert_eq!(created.0, StatusCode::CREATED, "{}", created.1);
    let q = created.1["question_id"].as_str().unwrap();
    let before = get(&f.app, &f.owner, &ready, true).await;
    assert!(
        before.1["evaluation"]["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["gate"] == "BLOCKING_QUESTIONS" && i["code"] == "POLICY_EVIDENCE_PENDING")
    );
    assert_eq!(
        post(
            &f,
            &product,
            &format!("{path}/{q}/actions"),
            action(1, "Explicit Product answer")
        )
        .await
        .0,
        StatusCode::CREATED
    );
    let after = get(&f.app, &f.owner, &ready, true).await;
    assert!(
        !after.1["evaluation"]["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["gate"] == "BLOCKING_QUESTIONS")
    );
    assert_eq!(after.1["provider_refs_verified"], false);
    assert_ne!(after.1["evaluation"]["conclusion"], "SUCCESS");
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT head_sha FROM proposals WHERE id=$1")
            .bind(f.proposal)
            .fetch_one(&pool)
            .await
            .unwrap(),
        HEAD
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM official_reviews")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(f.provider.ref_reads.load(Ordering::SeqCst), reads);
}

#[sqlx::test(migrations = "../../migrations")]
async fn question_http_unavailable_write_rolls_back_and_preserves_exact_retry_semantics(
    pool: PgPool,
) {
    let (f, _, _) = configured(&pool, false).await;
    let path = root(&f);
    let request = body(2);
    sqlx::raw_sql("CREATE FUNCTION question_http_audit_failure() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='PRODUCT_QUESTION_CHANGED' THEN RAISE EXCEPTION 'fixture failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER question_http_audit_failure BEFORE INSERT ON audit_events FOR EACH ROW EXECUTE FUNCTION question_http_audit_failure();").execute(&pool).await.unwrap();
    let failed = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&path)
                .header("Cookie", format!("specspace-session={}", f.owner.token))
                .header("X-Expected-Actor", f.owner.id.to_string())
                .header("Origin", "http://localhost:4200")
                .header("X-XSRF-TOKEN", &f.owner.csrf)
                .header("Content-Type", "application/json")
                .body(Body::from(request.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(failed.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(failed.headers()["Retry-After"], "1");
    assert_eq!(failed.headers()["cache-control"], "no-store");
    let failed: Value =
        serde_json::from_slice(&failed.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(failed["error"]["code"], "PRODUCT_QUESTION_UNAVAILABLE");
    assert_eq!(
        get(&f.app, &f.owner, &path, true).await.1["questions"],
        json!([])
    );
    sqlx::query("DROP TRIGGER question_http_audit_failure ON audit_events")
        .execute(&pool)
        .await
        .unwrap();
    let saved = post(&f, &f.owner, &path, request.clone()).await;
    assert_eq!(saved.0, StatusCode::CREATED);
    let replay = post(&f, &f.owner, &path, request).await;
    assert_eq!(replay.0, StatusCode::OK);
    assert_eq!(replay.1["id"], saved.1["id"]);
}
