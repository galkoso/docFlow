use super::*;
#[path = "visits/approvals.rs"]
mod approvals;
use sha2::{Digest, Sha256};
struct Ready {
    f: Fixture,
    product: User,
    development: User,
    root: String,
    source: String,
    fp: String,
    unit: Value,
}
async fn ready(pool: &PgPool) -> Ready {
    let (f, product, development) = configured(pool, false).await;
    let (result, snapshot) = publish(&f, 2).await;
    let root = format!("{}/{snapshot}", history_path(&f));
    let fp = result["review"]["fingerprint"].as_str().unwrap().to_owned();
    let unit:Value=sqlx::query_scalar("SELECT unit FROM review_snapshot_units WHERE snapshot_id=$1 AND unit->'after'->'range'<>'null'::jsonb ORDER BY ordinal LIMIT 1").bind(Uuid::parse_str(&snapshot).unwrap()).fetch_one(pool).await.unwrap();
    let source = format!(
        "{root}/units/{}/source?expected_fingerprint={fp}&domain=PRODUCT&side=after&offset=0",
        unit["id"].as_str().unwrap()
    );
    Ready {
        f,
        product,
        development,
        root,
        source,
        fp,
        unit,
    }
}
fn body(w: &Value) -> Value {
    json!({"request_id":Uuid::new_v4(),"delivery_id":w["delivery_id"],"expected_fingerprint":w["fingerprint"],"domain":"PRODUCT"})
}
async fn post(r: &Ready, body: Value) -> (StatusCode, Value, Option<String>) {
    post_raw(
        &r.f.app,
        Some(&r.product),
        &format!("{}/visits", r.root),
        body.to_string(),
        PostOptions::new(&r.product, "unused"),
    )
    .await
}
async fn visits(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM review_source_visits")
        .fetch_one(pool)
        .await
        .unwrap()
}
#[sqlx::test(migrations = "../../migrations")]
async fn http_delivery_and_explicit_ack_keep_source_reads_visits_and_approvals_separate(
    pool: PgPool,
) {
    let r = ready(&pool).await;
    let w = get(&r.f.app, &r.product, &r.source, true).await;
    assert_eq!(w.0, StatusCode::OK, "{}", w.1);
    assert_eq!(w.1["actor_id"], r.product.id.to_string());
    assert_eq!(w.1["fingerprint"], r.fp);
    assert_eq!(w.1["head_sha"], HEAD);
    assert_eq!(w.1["source"], r.unit["after"]);
    assert_eq!(w.1["unit_id"], r.unit["id"]);
    let text = w.1["text"].as_str().unwrap();
    assert!(text.len() <= 65536);
    assert_eq!(
        w.1["window_sha256"],
        format!("{:x}", Sha256::digest(text.as_bytes()))
    );
    assert_eq!(visits(&pool).await, 0);
    let request = body(&w.1);
    let reads = r.f.provider.reads.load(Ordering::SeqCst);
    let (a, b) = tokio::join!(post(&r, request.clone()), post(&r, request.clone()));
    assert!([a.0, b.0].contains(&StatusCode::CREATED));
    assert!([a.0, b.0].contains(&StatusCode::OK));
    assert_eq!(a.1["id"], b.1["id"]);
    assert_eq!(a.1["request_id"], request["request_id"]);
    assert_eq!(a.1["delivery_id"], w.1["delivery_id"]);
    assert_eq!(a.1["unit_id"], r.unit["id"]);
    assert_eq!(a.1["start_byte"], w.1["start_byte"]);
    assert_eq!(a.1["end_byte"], w.1["end_byte"]);
    assert_eq!(visits(&pool).await, 1);
    assert_eq!(r.f.provider.reads.load(Ordering::SeqCst), reads);
    let second = get(&r.f.app, &r.product, &r.source, true).await;
    assert_eq!(second.0, StatusCode::OK);
    assert_ne!(second.1["delivery_id"], w.1["delivery_id"]);
    assert_eq!(visits(&pool).await, 1);
    let mut changed = request;
    changed["delivery_id"] = second.1["delivery_id"].clone();
    assert_eq!(post(&r, changed).await.0, StatusCode::CONFLICT);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM official_reviews")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn http_source_queries_are_strict_scoped_session_and_actor_bound_before_io(pool: PgPool) {
    let r = ready(&pool).await;
    let reads = r.f.provider.reads.load(Ordering::SeqCst);
    for path in [
        format!("{}&path=docs/other.md", r.source),
        format!("{}&offset=1", r.source),
        r.source.replace("offset=0", "offset=-1"),
        r.source.replace("offset=0", "offset=2097153"),
        r.source.replace("side=after", "side=arbitrary"),
        r.source.replace("domain=PRODUCT", "domain=product"),
        r.source.replace("&offset=0", ""),
    ] {
        assert_eq!(
            get(&r.f.app, &r.product, &path, true).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        get(
            &r.f.app,
            &r.product,
            &r.source.replace(&r.fp, &"a".repeat(64)),
            true
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        get(&r.f.app, &r.product, &r.source, false).await.0,
        StatusCode::CONFLICT
    );
    for user in [&r.f.owner, &r.f.actor, &r.development] {
        assert_eq!(
            get(&r.f.app, user, &r.source, true).await.0,
            StatusCode::FORBIDDEN
        );
    }
    let foreign = fixture(&pool, true).await;
    assert_eq!(
        get(&r.f.app, &foreign.owner, &r.source, true).await.0,
        StatusCode::NOT_FOUND
    );
    let anonymous =
        r.f.app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(&r.source)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(anonymous.headers()["cache-control"], "no-store");
    assert_eq!(r.f.provider.reads.load(Ordering::SeqCst), reads);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM review_source_attempts")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn http_acknowledgement_requires_csrf_bound_actor_and_strict_bounded_intent(pool: PgPool) {
    let r = ready(&pool).await;
    let w = get(&r.f.app, &r.product, &r.source, true).await;
    assert_eq!(w.0, StatusCode::OK, "{}", w.1);
    let request = body(&w.1);
    let path = format!("{}/visits", r.root);
    for fault in 0..5 {
        let mut options = PostOptions::new(&r.product, "unused");
        match fault {
            0 => options.origin = None,
            1 => options.csrf = None,
            2 => options.expected = None,
            3 => options.expected = Some(r.f.owner.id),
            _ => {}
        }
        let user = if fault == 4 { None } else { Some(&r.product) };
        let response = post_raw(&r.f.app, user, &path, request.to_string(), options).await;
        assert_eq!(
            response.0,
            if fault == 4 {
                StatusCode::UNAUTHORIZED
            } else if fault == 2 || fault == 3 {
                StatusCode::CONFLICT
            } else {
                StatusCode::FORBIDDEN
            }
        );
    }
    let mut extra = request.clone();
    extra["source"] = json!("untrusted");
    assert_eq!(post(&r, extra).await.0, StatusCode::BAD_REQUEST);
    let mut nil = request.clone();
    nil["request_id"] = json!(Uuid::nil());
    assert_eq!(post(&r, nil).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(
        post_raw(
            &r.f.app,
            Some(&r.product),
            &format!("{path}?approve=true"),
            request.to_string(),
            PostOptions::new(&r.product, "unused")
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        post_raw(
            &r.f.app,
            Some(&r.product),
            &path,
            "{".into(),
            PostOptions::new(&r.product, "unused")
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        post_raw(
            &r.f.app,
            Some(&r.product),
            &path,
            json!({"request_id":"x".repeat(5000)}).to_string(),
            PostOptions::new(&r.product, "unused")
        )
        .await
        .0,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert_eq!(visits(&pool).await, 0);
}
#[sqlx::test(migrations = "../../migrations")]
async fn http_source_rate_limits_share_persistent_backoff_and_return_retry_after(pool: PgPool) {
    let r = ready(&pool).await;
    r.f.provider.backoff.store(2, Ordering::SeqCst);
    let first = get(&r.f.app, &r.product, &r.source, true).await;
    assert_eq!(first.0, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(first.3.as_deref(), Some("13"));
    assert_eq!(first.1["error"]["code"], "REVIEW_PROVIDER_RATE_LIMIT");
    let reads = r.f.provider.reads.load(Ordering::SeqCst);
    let next = get(&r.f.app, &r.product, &r.source, true).await;
    assert_eq!(next.0, StatusCode::TOO_MANY_REQUESTS);
    assert!(next.3.unwrap().parse::<u32>().unwrap() <= 13);
    assert_eq!(r.f.provider.reads.load(Ordering::SeqCst), reads);
    assert_eq!(visits(&pool).await, 0);
}
#[sqlx::test(migrations = "../../migrations")]
async fn http_unverified_source_never_returns_a_delivery_or_visit(pool: PgPool) {
    let r = ready(&pool).await;
    r.f.provider.backoff.store(3, Ordering::SeqCst);
    let result = get(&r.f.app, &r.product, &r.source, true).await;
    assert_eq!(result.0, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(result.3.as_deref(), Some("30"));
    assert!(!result.1.to_string().contains("forged source"));
    assert!(result.1.get("delivery_id").is_none());
    assert_eq!(visits(&pool).await, 0);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM review_source_deliveries")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn http_expired_unacknowledged_delivery_is_gone_but_recorded_exact_replay_survives(
    pool: PgPool,
) {
    let r = ready(&pool).await;
    let w = get(&r.f.app, &r.product, &r.source, true).await;
    let request = body(&w.1);
    assert_eq!(post(&r, request.clone()).await.0, StatusCode::CREATED);
    let pending = get(&r.f.app, &r.product, &r.source, true).await;
    let reads = r.f.provider.reads.load(Ordering::SeqCst);
    sqlx::raw_sql("ALTER TABLE review_source_deliveries DISABLE TRIGGER review_source_deliveries_immutable; UPDATE review_source_deliveries SET created_at=clock_timestamp()-interval '31 minutes',expires_at=clock_timestamp()-interval '1 minute'; ALTER TABLE review_source_deliveries ENABLE TRIGGER review_source_deliveries_immutable;").execute(&pool).await.unwrap();
    let expired = post(&r, body(&pending.1)).await;
    assert_eq!(expired.0, StatusCode::GONE);
    assert_eq!(expired.1["error"]["code"], "REVIEW_DELIVERY_EXPIRED");
    let replay = post(&r, request).await;
    assert_eq!(replay.0, StatusCode::OK);
    assert_eq!(replay.1["replayed"], true);
    assert_eq!(visits(&pool).await, 1);
    assert_eq!(r.f.provider.reads.load(Ordering::SeqCst), reads);
}
#[sqlx::test(migrations = "../../migrations")]
async fn http_acknowledgement_rechecks_role_and_snapshot_instead_of_trusting_delivery(
    pool: PgPool,
) {
    let r = ready(&pool).await;
    let w = get(&r.f.app, &r.product, &r.source, true).await;
    let request = body(&w.1);
    sqlx::query("UPDATE memberships SET role='READER' WHERE user_id=$1")
        .bind(r.product.id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(post(&r, request.clone()).await.0, StatusCode::FORBIDDEN);
    sqlx::query("UPDATE memberships SET role='REVIEWER' WHERE user_id=$1")
        .bind(r.product.id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE proposals SET head_sha=$2,version=version+1 WHERE id=$1")
        .bind(r.f.proposal)
        .bind("c".repeat(40))
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(post(&r, request).await.0, StatusCode::CONFLICT);
    assert_eq!(
        get(&r.f.app, &r.product, &r.source, true).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(visits(&pool).await, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn http_access_loss_during_source_read_discards_delivery_and_returns_scoped_denial(
    pool: PgPool,
) {
    let r = ready(&pool).await;
    *r.f.provider.hook.lock().unwrap() = Some((pool.clone(), r.product.id));
    let result = get(&r.f.app, &r.product, &r.source, true).await;
    assert_eq!(result.0, StatusCode::NOT_FOUND);
    assert!(result.1.get("text").is_none());
    assert_eq!(visits(&pool).await, 0);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM review_source_deliveries")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn http_coverage_reads_exact_acknowledged_ranges_without_provider_io_or_approval(
    pool: PgPool,
) {
    let r = ready(&pool).await;
    let path = format!(
        "{}/coverage?expected_fingerprint={}&domain=PRODUCT",
        r.root, r.fp
    );
    let initial = get(&r.f.app, &r.product, &path, true).await;
    assert_eq!(initial.0, StatusCode::OK, "{}", initial.1);
    assert_eq!(initial.1["actor_id"], r.product.id.to_string());
    assert_eq!(initial.1["fingerprint"], r.fp);
    assert_eq!(initial.1["viewed_units"], 0);
    assert_eq!(initial.1["available"], true);
    let decisions = format!("{}/decisions", r.root);
    let warning = get(&r.f.app, &r.product, &decisions, true).await;
    assert_eq!(warning.1["unviewed_domains"], json!(["PRODUCT"]));
    let w = get(&r.f.app, &r.product, &r.source, true).await;
    post(&r, body(&w.1)).await;
    if !r.unit["before"].is_null() {
        let before = get(
            &r.f.app,
            &r.product,
            &r.source.replace("side=after", "side=before"),
            true,
        )
        .await;
        assert_eq!(before.0, StatusCode::OK, "{}", before.1);
        assert_eq!(post(&r, body(&before.1)).await.0, StatusCode::CREATED);
    }
    let reads = r.f.provider.reads.load(Ordering::SeqCst);
    let result = get(&r.f.app, &r.product, &path, true).await;
    assert_eq!(result.0, StatusCode::OK);
    assert_eq!(result.1["viewed_units"], 1);
    assert_eq!(result.1["current_for_metadata"], true);
    let item = result.1["units"]
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["unit_id"] == r.unit["id"])
        .unwrap();
    assert_eq!(item["viewed"], true);
    assert_eq!(item["after"], "viewed");
    let other = get(
        &r.f.app,
        &r.development,
        &path.replace("domain=PRODUCT", "domain=DEVELOPMENT"),
        true,
    )
    .await;
    assert_eq!(other.0, StatusCode::OK);
    assert_eq!(other.1["viewed_units"], 0);
    let read = get(&r.f.app, &r.product, &decisions, true).await;
    assert_eq!(read.0, StatusCode::OK);
    let unread = result.1["unviewed_units"].as_i64().unwrap() > 0;
    assert_eq!(read.1["unviewed_content_warning"], unread);
    assert_eq!(
        read.1["unviewed_domains"],
        if unread {
            json!(["PRODUCT"])
        } else {
            json!([])
        }
    );
    let independent = get(&r.f.app, &r.development, &decisions, true).await;
    assert_eq!(independent.1["unviewed_domains"], json!(["DEVELOPMENT"]));
    assert_eq!(r.f.provider.reads.load(Ordering::SeqCst), reads);
    assert!(!result.1.to_string().contains("PRIVATE SOURCE SENTINEL"));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM official_reviews")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn http_coverage_queries_require_current_scoped_identity_and_strict_bounded_coordinates(
    pool: PgPool,
) {
    let r = ready(&pool).await;
    let path = format!(
        "{}/coverage?expected_fingerprint={}&domain=PRODUCT",
        r.root, r.fp
    );
    let reads = r.f.provider.reads.load(Ordering::SeqCst);
    for query in [
        "offset=-1",
        "offset=40001",
        "limit=0",
        "limit=101",
        "offset=0&offset=1",
        "reviewer_id=other",
    ] {
        assert_eq!(
            get(&r.f.app, &r.product, &format!("{path}&{query}"), true)
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        get(
            &r.f.app,
            &r.product,
            &path.replace("&domain=PRODUCT", ""),
            true
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        get(
            &r.f.app,
            &r.product,
            &path.replace(&r.fp, &"0".repeat(64)),
            true
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        get(&r.f.app, &r.product, &path, false).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        get(&r.f.app, &r.f.owner, &path, true).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        get(&r.f.app, &r.development, &path, true).await.0,
        StatusCode::FORBIDDEN
    );
    let foreign = fixture(&pool, true).await;
    assert_eq!(
        get(&r.f.app, &foreign.owner, &path, true).await.0,
        StatusCode::NOT_FOUND
    );
    let anonymous =
        r.f.app
            .clone()
            .oneshot(Request::builder().uri(&path).body(Body::empty()).unwrap())
            .await
            .unwrap();
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(anonymous.headers()["cache-control"], "no-store");
    let end = get(&r.f.app, &r.product, &format!("{path}&offset=40000"), true).await;
    assert_eq!(end.0, StatusCode::OK);
    assert_eq!(end.1["units"], json!([]));
    assert!(end.1["next_offset"].is_null());
    sqlx::query("DELETE FROM memberships WHERE user_id=$1")
        .bind(r.product.id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        get(&r.f.app, &r.product, &path, true).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(r.f.provider.reads.load(Ordering::SeqCst), reads);
    assert_eq!(visits(&pool).await, 0);
}
