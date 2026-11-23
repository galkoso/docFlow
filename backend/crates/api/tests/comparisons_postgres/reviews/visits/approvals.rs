use super::*;
fn intent(r: &Ready) -> Value {
    json!({"request_id":Uuid::new_v4(),"expected_fingerprint":r.fp,"domain":"PRODUCT","unit_id":r.unit["id"]})
}
fn endpoint(r: &Ready) -> String {
    format!("{}/unit-approvals", r.root)
}
fn status(r: &Ready) -> String {
    format!(
        "{}?expected_fingerprint={}&domain=PRODUCT",
        endpoint(r),
        r.fp
    )
}
async fn submit(r: &Ready, value: Value) -> (StatusCode, Value, Option<String>) {
    post_raw(
        &r.f.app,
        Some(&r.product),
        &endpoint(r),
        value.to_string(),
        PostOptions::new(&r.product, "unused"),
    )
    .await
}
async fn complete(r: &Ready) {
    for side in ["before", "after"] {
        if r.unit[side].is_null() {
            continue;
        }
        let w = get(
            &r.f.app,
            &r.product,
            &r.source.replace("side=after", &format!("side={side}")),
            true,
        )
        .await;
        assert_eq!(w.0, StatusCode::OK, "{}", w.1);
        assert_eq!(super::post(r, body(&w.1)).await.0, StatusCode::CREATED);
    }
}
async fn count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM review_unit_approvals")
        .fetch_one(pool)
        .await
        .unwrap()
}
#[sqlx::test(migrations = "../../migrations")]
async fn ordinary_official_approval_waits_for_all_individual_records_and_keeps_the_explicit_all_option_separate(
    pool: PgPool,
) {
    let (f, product, development) = configured(&pool, false).await;
    f.provider.base_updated.store(true, Ordering::SeqCst);
    sqlx::query("UPDATE proposals SET base_sha=$2 WHERE id=$1")
        .bind(f.proposal)
        .bind(MAIN)
        .execute(&pool)
        .await
        .unwrap();
    let (published, snapshot) = publish(&f, 2).await;
    let root = format!("{}/{snapshot}", history_path(&f));
    let fp = published["review"]["fingerprint"]
        .as_str()
        .unwrap()
        .to_owned();
    let units = get(
        &f.app,
        &product,
        &format!("{root}/units?offset=0&limit=100"),
        true,
    )
    .await;
    assert_eq!(units.0, StatusCode::OK);
    let first = units.1["units"][0].clone();
    let r = Ready {
        f,
        product,
        development,
        root: root.clone(),
        source: String::new(),
        fp: fp.clone(),
        unit: first,
    };
    let decision = json!({"request_id":Uuid::new_v4(),"expected_fingerprint":fp,"domain":"PRODUCT","decision":"APPROVE","explanation":""});
    let decisions = format!("{root}/decisions");
    let initial = post_raw(
        &r.f.app,
        Some(&r.product),
        &decisions,
        decision.to_string(),
        PostOptions::new(&r.product, "unused"),
    )
    .await;
    assert_eq!(initial.0, StatusCode::CONFLICT);
    assert_eq!(initial.1["error"]["code"], "REVIEW_SECTIONS_INCOMPLETE");
    for unit in units.1["units"].as_array().unwrap() {
        for side in ["before", "after"] {
            if unit[side].is_null() {
                continue;
            }
            let path = format!(
                "{root}/units/{}/source?expected_fingerprint={fp}&domain=PRODUCT&side={side}&offset=0",
                unit["id"].as_str().unwrap()
            );
            let w = get(&r.f.app, &r.product, &path, true).await;
            assert_eq!(w.0, StatusCode::OK, "{}", w.1);
            assert_eq!(super::post(&r, body(&w.1)).await.0, StatusCode::CREATED);
        }
        let mut approve = intent(&r);
        approve["unit_id"] = unit["id"].clone();
        assert_eq!(submit(&r, approve).await.0, StatusCode::CREATED);
    }
    let hints = get(&r.f.app, &r.product, &decisions, true).await;
    assert_eq!(hints.1["approve_domains"], json!(["PRODUCT"]));
    let created = post_raw(
        &r.f.app,
        Some(&r.product),
        &decisions,
        decision.to_string(),
        PostOptions::new(&r.product, "unused"),
    )
    .await;
    assert_eq!(created.0, StatusCode::CREATED, "{}", created.1);
    assert_eq!(created.1["decision"], "APPROVE");
    let replay = post_raw(
        &r.f.app,
        Some(&r.product),
        &decisions,
        decision.to_string(),
        PostOptions::new(&r.product, "unused"),
    )
    .await;
    assert_eq!(replay.0, StatusCode::OK);
    assert_eq!(replay.1["id"], created.1["id"]);
    let independent = get(&r.f.app, &r.development, &decisions, true).await;
    assert_eq!(independent.1["approve_domains"], json!([]));
    assert_eq!(independent.1["approve_all_available"], true);
}

#[sqlx::test(migrations = "../../migrations")]
async fn explicit_unit_approval_is_distinct_from_visits_and_concurrent_exact_requests_replay_without_git_changes(
    pool: PgPool,
) {
    let r = ready(&pool).await;
    let request = intent(&r);
    let before: Value = sqlx::query_scalar("SELECT to_jsonb(p) FROM proposals p WHERE id=$1")
        .bind(r.f.proposal)
        .fetch_one(&pool)
        .await
        .unwrap();
    let unread = submit(&r, request.clone()).await;
    assert_eq!(unread.0, StatusCode::CONFLICT);
    assert_eq!(unread.1["error"]["code"], "REVIEW_UNIT_UNVIEWED");
    let w = get(&r.f.app, &r.product, &r.source, true).await;
    assert_eq!(super::post(&r, body(&w.1)).await.0, StatusCode::CREATED);
    if !r.unit["before"].is_null() {
        assert_eq!(submit(&r, request.clone()).await.0, StatusCode::CONFLICT);
    }
    // A second delivered window and even a complete visit set are not approvals.
    complete(&r).await;
    assert_eq!(count(&pool).await, 0);
    let empty = get(&r.f.app, &r.product, &status(&r), true).await;
    assert_eq!(empty.0, StatusCode::OK);
    assert_eq!(empty.1["approved_units"], 0);
    let reads = r.f.provider.reads.load(Ordering::SeqCst);
    let refs = r.f.provider.ref_reads.load(Ordering::SeqCst);
    let (a, b) = tokio::join!(submit(&r, request.clone()), submit(&r, request.clone()));
    assert!([a.0, b.0].contains(&StatusCode::CREATED));
    assert!([a.0, b.0].contains(&StatusCode::OK));
    assert_eq!(a.1["id"], b.1["id"]);
    assert_eq!(a.1["actor_id"], r.product.id.to_string());
    assert_eq!(a.1["unit_id"], r.unit["id"]);
    assert_eq!(a.1["fingerprint"], r.fp);
    assert_eq!(a.1["head_sha"], HEAD);
    assert_eq!(a.1["target_sha"], MAIN);
    let page = get(&r.f.app, &r.product, &status(&r), true).await;
    assert_eq!(page.0, StatusCode::OK);
    assert_eq!(page.1["approved_units"], 1);
    assert_eq!(page.1["domain"], "PRODUCT");
    assert_eq!(page.1["actor_id"], r.product.id.to_string());
    assert_eq!(
        page.1["units"]
            .as_array()
            .unwrap()
            .iter()
            .find(|u| u["unit_id"] == r.unit["id"])
            .unwrap()["approval_id"],
        a.1["id"]
    );
    let independent = get(
        &r.f.app,
        &r.development,
        &status(&r).replace("domain=PRODUCT", "domain=DEVELOPMENT"),
        true,
    )
    .await;
    assert_eq!(independent.0, StatusCode::OK);
    assert_eq!(independent.1["approved_units"], 0);
    let mut changed = request.clone();
    changed["unit_id"] = "f".repeat(64).into();
    let conflict = submit(&r, changed).await;
    assert_eq!(conflict.0, StatusCode::CONFLICT);
    assert_eq!(conflict.1["error"]["code"], "UNIT_APPROVAL_CONFLICT");
    assert_eq!(count(&pool).await, 1);
    assert_eq!(r.f.provider.reads.load(Ordering::SeqCst), reads);
    assert_eq!(r.f.provider.ref_reads.load(Ordering::SeqCst), refs);
    assert_eq!(
        sqlx::query_scalar::<_, Value>("SELECT to_jsonb(p) FROM proposals p WHERE id=$1")
            .bind(r.f.proposal)
            .fetch_one(&pool)
            .await
            .unwrap(),
        before
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM official_reviews")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    assert!(!page.1.to_string().contains("PRIVATE SOURCE SENTINEL"));
}
#[sqlx::test(migrations = "../../migrations")]
async fn unit_approval_writes_require_session_actor_origin_csrf_and_strict_small_intents(
    pool: PgPool,
) {
    let r = ready(&pool).await;
    let request = intent(&r);
    let path = endpoint(&r);
    for fault in 0..5 {
        let mut options = PostOptions::new(&r.product, "unused");
        match fault {
            0 => options.origin = Some("https://foreign.invalid"),
            1 => options.csrf = None,
            2 => options.expected = None,
            3 => options.expected = Some(r.f.owner.id),
            _ => {}
        }
        let result = post_raw(
            &r.f.app,
            if fault == 4 { None } else { Some(&r.product) },
            &path,
            request.to_string(),
            options,
        )
        .await;
        assert_eq!(
            result.0,
            if fault == 4 {
                StatusCode::UNAUTHORIZED
            } else if fault == 2 || fault == 3 {
                StatusCode::CONFLICT
            } else {
                StatusCode::FORBIDDEN
            }
        );
    }
    for field in [
        "actor_id",
        "reviewer_id",
        "viewed",
        "approved",
        "source",
        "head_sha",
    ] {
        let mut value = request.clone();
        value[field] = json!(true);
        assert_eq!(submit(&r, value).await.0, StatusCode::BAD_REQUEST);
    }
    for (field, value) in [
        ("request_id", Uuid::nil().to_string()),
        ("unit_id", "x".repeat(64)),
        ("expected_fingerprint", "A".repeat(64)),
        ("domain", "OTHER".into()),
    ] {
        let mut value_body = request.clone();
        value_body[field] = value.into();
        assert_eq!(submit(&r, value_body).await.0, StatusCode::BAD_REQUEST);
    }
    for value in [
        "{".into(),
        format!(
            "{{\"request_id\":\"{}\",{}",
            Uuid::new_v4(),
            &request.to_string()[1..]
        ),
    ] {
        assert_eq!(
            post_raw(
                &r.f.app,
                Some(&r.product),
                &path,
                value,
                PostOptions::new(&r.product, "unused")
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
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
    assert_eq!(
        post_raw(
            &r.f.app,
            Some(&r.product),
            &format!("{path}?viewed=true"),
            request.to_string(),
            PostOptions::new(&r.product, "unused")
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(count(&pool).await, 0);
    assert_eq!(visits(&pool).await, 0);
}
#[sqlx::test(migrations = "../../migrations")]
async fn unit_approval_reads_enforce_current_identity_scope_and_bounded_strict_queries_without_writes(
    pool: PgPool,
) {
    let r = ready(&pool).await;
    let path = status(&r);
    let reads = r.f.provider.reads.load(Ordering::SeqCst);
    for query in [
        "offset=-1",
        "offset=40001",
        "limit=0",
        "limit=101",
        "offset=0&offset=1",
        "actor_id=foreign",
        "approved=true",
    ] {
        assert_eq!(
            get(&r.f.app, &r.product, &format!("{path}&{query}"), true)
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
    }
    for query in [
        path.replace("&domain=PRODUCT", ""),
        path.replace("domain=PRODUCT", "domain=OTHER"),
        path.replace(&format!("expected_fingerprint={}&", r.fp), ""),
    ] {
        assert_eq!(
            get(&r.f.app, &r.product, &query, true).await.0,
            StatusCode::BAD_REQUEST
        );
    }
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
    let page = get(&r.f.app, &r.product, &format!("{path}&limit=1"), true).await;
    assert_eq!(page.0, StatusCode::OK);
    assert_eq!(page.1["units"].as_array().unwrap().len(), 1);
    assert_eq!(page.1["next_offset"], 1);
    assert!(page.1["total_units"].as_i64().unwrap() > 1);
    let end = get(&r.f.app, &r.product, &format!("{path}&offset=40000"), true).await;
    assert_eq!(end.1["units"], json!([]));
    assert!(end.1["next_offset"].is_null());
    assert_eq!(page.1["total_units"], end.1["total_units"]);
    assert_eq!(r.f.provider.reads.load(Ordering::SeqCst), reads);
    assert_eq!(count(&pool).await, 0);
    assert_eq!(visits(&pool).await, 0);
}
#[sqlx::test(migrations = "../../migrations")]
async fn historical_unit_receipts_are_observations_and_revocation_denies_replay(pool: PgPool) {
    let r = ready(&pool).await;
    complete(&r).await;
    let request = intent(&r);
    let saved = submit(&r, request.clone()).await;
    assert_eq!(saved.0, StatusCode::CREATED);
    sqlx::query("UPDATE organizations SET policy_version=2 WHERE id=$1")
        .bind(r.f.organization)
        .execute(&pool)
        .await
        .unwrap();
    let old = get(&r.f.app, &r.product, &status(&r), true).await;
    assert_eq!(old.0, StatusCode::OK);
    assert_eq!(old.1["current_for_metadata"], false);
    assert_eq!(old.1["approved_units"], 1);
    let replay = submit(&r, request.clone()).await;
    assert_eq!(replay.0, StatusCode::OK);
    assert_eq!(replay.1["id"], saved.1["id"]);
    assert_eq!(replay.1["replayed"], true);
    let stale = submit(&r, intent(&r)).await;
    assert_eq!(stale.0, StatusCode::CONFLICT);
    assert_eq!(stale.1["error"]["code"], "REVIEW_CONTEXT_CHANGED");
    sqlx::query("UPDATE memberships SET review_domains='{}' WHERE user_id=$1")
        .bind(r.product.id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(submit(&r, request).await.0, StatusCode::FORBIDDEN);
    assert_eq!(
        get(&r.f.app, &r.product, &status(&r), true).await.0,
        StatusCode::FORBIDDEN
    );
    sqlx::query("DELETE FROM memberships WHERE user_id=$1")
        .bind(r.product.id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        get(&r.f.app, &r.product, &status(&r), true).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(count(&pool).await, 1);
}
#[sqlx::test(migrations = "../../migrations")]
async fn approval_storage_failure_returns_bounded_retry_and_rolls_back_all_evidence_before_manual_retry(
    pool: PgPool,
) {
    let r = ready(&pool).await;
    complete(&r).await;
    let request = intent(&r);
    sqlx::raw_sql("CREATE FUNCTION reject_unit_approval_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='REVIEW_UNIT_APPROVED' THEN RAISE EXCEPTION 'PRIVATE FAILURE SENTINEL'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_unit_approval_audit BEFORE INSERT ON audit_events FOR EACH ROW EXECUTE FUNCTION reject_unit_approval_audit();").execute(&pool).await.unwrap();
    let failed = submit(&r, request.clone()).await;
    assert_eq!(failed.0, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(failed.1["error"]["code"], "UNIT_APPROVAL_UNAVAILABLE");
    assert!(!failed.1.to_string().contains("PRIVATE FAILURE SENTINEL"));
    assert_eq!(count(&pool).await, 0);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM outbox WHERE event_type='REVIEW_UNIT_APPROVED'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        0
    );
    sqlx::raw_sql("DROP TRIGGER reject_unit_approval_audit ON audit_events; DROP FUNCTION reject_unit_approval_audit();").execute(&pool).await.unwrap();
    assert_eq!(submit(&r, request).await.0, StatusCode::CREATED);
    assert_eq!(count(&pool).await, 1);
}
#[sqlx::test(migrations = "../../migrations")]
async fn approval_http_quota_returns_retry_after_and_preserves_exact_receipt_replay(pool: PgPool) {
    let r = ready(&pool).await;
    complete(&r).await;
    let request = intent(&r);
    let saved = submit(&r, request.clone()).await;
    assert_eq!(saved.0, StatusCode::CREATED);
    let store = specspace_infrastructure::unit_approvals::PgUnitApprovals::new(pool.clone());
    let snapshot = Uuid::parse_str(r.root.rsplit('/').next().unwrap()).unwrap();
    for _ in 1..600 {
        store
            .approve(
                r.product.id,
                r.f.proposal,
                snapshot,
                serde_json::from_value(intent(&r)).unwrap(),
            )
            .await
            .unwrap();
    }
    let limit = submit(&r, intent(&r)).await;
    assert_eq!(limit.0, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(limit.1["error"]["code"], "UNIT_APPROVAL_LIMIT");
    let replay = submit(&r, request).await;
    assert_eq!(replay.0, StatusCode::OK);
    assert_eq!(replay.1["id"], saved.1["id"]);
    assert_eq!(count(&pool).await, 600);
}
