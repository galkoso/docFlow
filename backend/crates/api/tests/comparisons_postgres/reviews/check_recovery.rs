use super::*;
use specspace_infrastructure::review_checks::{CheckLease, PgReviewChecks};
async fn stopped(
    pool: &PgPool,
    started: bool,
) -> (Fixture, User, PgReviewChecks, CheckLease, String) {
    let (f, _, _) = configured(pool, false).await;
    f.provider.base_updated.store(true, Ordering::SeqCst);
    sqlx::query("UPDATE proposals SET base_sha=$2 WHERE id=$1")
        .bind(f.proposal)
        .bind(MAIN)
        .execute(pool)
        .await
        .unwrap();
    let (_, snapshot) = publish(&f, 2).await;
    let repo = sqlx::query_scalar::<_, Uuid>("SELECT repository_id FROM proposals WHERE id=$1")
        .bind(f.proposal)
        .fetch_one(pool)
        .await
        .unwrap();
    sqlx::query("UPDATE repositories SET check_app_id=7 WHERE id=$1")
        .bind(repo)
        .execute(pool)
        .await
        .unwrap();
    let store = PgReviewChecks::new(
        pool.clone(),
        Arc::new(Factory {
            provider: f.provider.clone(),
            repository: repo,
        }),
    );
    store
        .evaluate(
            f.owner.id,
            f.proposal,
            Uuid::parse_str(&snapshot).unwrap(),
            Uuid::new_v4(),
        )
        .await
        .unwrap();
    let lease = store.claim(Uuid::new_v4(), 120).await.unwrap().unwrap();
    if started {
        store.start_write(&lease, &[], true).await.unwrap();
    }
    store
        .defer(&lease, "CHECK_PROVIDER_UNAVAILABLE", None, None)
        .await
        .unwrap();
    let admin = user(pool, f.organization, "ADMIN").await;
    let path = format!(
        "/api/v1/proposals/{}/review/checks/{}",
        f.proposal,
        lease.operation_id()
    );
    (f, admin, store, lease, path)
}
fn body(lease: &CheckLease) -> Value {
    json!({"request_id":Uuid::new_v4(),"expected_request_hash":lease.intent().request_hash()})
}
#[sqlx::test(migrations = "../../migrations")]
async fn admin_journal_status_is_read_only_private_and_explicitly_not_a_provider_observation(
    pool: PgPool,
) {
    let (f, admin, _, lease, path) = stopped(&pool, true).await;
    let reads = f.provider.ref_reads.load(Ordering::SeqCst);
    let audit = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM audit_events")
        .fetch_one(&pool)
        .await
        .unwrap();
    let response = get(&f.app, &admin, &path, true).await;
    assert_eq!(response.0, StatusCode::OK, "{}", response.1);
    for (key, value) in [
        ("viewer_id", admin.id.to_string()),
        ("operation_id", lease.operation_id().to_string()),
        ("proposal_id", f.proposal.to_string()),
        ("request_hash", lease.intent().request_hash().to_owned()),
        ("job_state", "FAILED".into()),
        ("operation_state", "UNKNOWN".into()),
        ("write_phase", "CREATE_STARTED".into()),
    ] {
        assert_eq!(response.1[key], value);
    }
    assert_eq!(response.1["provider_verified"], false);
    assert_eq!(response.1["published"], false);
    assert!(response.1.get("job_id").is_none());
    assert!(!response.1.to_string().contains("PRIVATE SOURCE SENTINEL"));
    assert_eq!(f.provider.ref_reads.load(Ordering::SeqCst), reads);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM audit_events")
            .fetch_one(&pool)
            .await
            .unwrap(),
        audit
    );
    assert_eq!(
        get(&f.app, &f.actor, &path, true).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        get(&f.app, &admin, &format!("{path}?fresh=true"), true)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        get(&f.app, &admin, &path, false).await.0,
        StatusCode::CONFLICT
    );
    let foreign_org = Uuid::new_v4();
    sqlx::query("INSERT INTO organizations(id,name) VALUES($1,'Synthetic foreign operation')")
        .bind(foreign_org)
        .execute(&pool)
        .await
        .unwrap();
    let foreign = user(&pool, foreign_org, "ADMIN").await;
    assert_eq!(
        get(&f.app, &foreign, &path, true).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        get(
            &f.app,
            &admin,
            &path.replace(
                &lease.operation_id().to_string(),
                &Uuid::new_v4().to_string()
            ),
            true
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    sqlx::query("UPDATE git_operations SET error_code='PRIVATE SOURCE SENTINEL' WHERE id=$1")
        .bind(lease.operation_id())
        .execute(&pool)
        .await
        .unwrap();
    let invalid = get(&f.app, &admin, &path, true).await;
    assert_eq!(invalid.0, StatusCode::SERVICE_UNAVAILABLE);
    assert!(!invalid.1.to_string().contains("PRIVATE SOURCE SENTINEL"));
    sqlx::query("UPDATE memberships SET role='READER' WHERE user_id=$1")
        .bind(admin.id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        get(&f.app, &admin, &path, true).await.0,
        StatusCode::FORBIDDEN
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn http_recovery_accepts_both_modes_and_exactly_replays_without_changing_write_markers(
    pool: PgPool,
) {
    for started in [false, true] {
        let (f, admin, store, lease, path) = stopped(&pool, started).await;
        let request = body(&lease);
        let uri = format!("{path}/recover");
        let reads = f.provider.ref_reads.load(Ordering::SeqCst);
        for replayed in [false, true] {
            let result = post_raw(
                &f.app,
                Some(&admin),
                &uri,
                request.to_string(),
                PostOptions::new(&admin, "unused"),
            )
            .await;
            assert_eq!(result.0, StatusCode::ACCEPTED, "{}", result.1);
            assert_eq!(result.1["replayed"], replayed);
            assert_eq!(
                result.1["mode"],
                if started {
                    "OBSERVE_STARTED"
                } else {
                    "RETRY_UNSTARTED"
                }
            );
            assert_eq!(result.1["max_attempts"], 16);
            assert!(result.1.get("job_id").is_none());
            assert_eq!(result.2.unwrap(), path);
        }
        assert_eq!(f.provider.ref_reads.load(Ordering::SeqCst), reads);
        let status = get(&f.app, &admin, &path, true).await;
        assert_eq!(status.1["job_state"], "QUEUED");
        assert_eq!(status.1["attempts"], 1);
        let next = store.claim(Uuid::new_v4(), 120).await.unwrap().unwrap();
        assert_eq!(next.operation_id(), lease.operation_id());
        if started {
            assert!(store.start_write(&next, &[], true).await.is_err());
        }
        store
            .defer(&next, "CHECK_PROVIDER_UNAVAILABLE", None, None)
            .await
            .unwrap();
    }
}
#[sqlx::test(migrations = "../../migrations")]
async fn recovery_http_rejects_untrusted_identity_csrf_queries_and_body_fields(pool: PgPool) {
    let (f, admin, _, lease, path) = stopped(&pool, false).await;
    let uri = format!("{path}/recover");
    let value = body(&lease);
    for mode in 0..9 {
        let mut options = PostOptions::new(&admin, "unused");
        let mut actor = Some(&admin);
        let mut uri = uri.clone();
        let mut value = value.clone();
        let expected = match mode {
            0 => {
                actor = None;
                StatusCode::UNAUTHORIZED
            }
            1 => {
                actor = Some(&f.actor);
                options = PostOptions::new(&f.actor, "unused");
                StatusCode::FORBIDDEN
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
                options.expected = Some(f.owner.id);
                StatusCode::CONFLICT
            }
            5 => {
                uri.push_str("?retry=true");
                StatusCode::BAD_REQUEST
            }
            6 => {
                value["actor_id"] = json!(admin.id);
                StatusCode::BAD_REQUEST
            }
            7 => {
                value["result"] = json!("SUCCESS");
                StatusCode::BAD_REQUEST
            }
            _ => {
                value["expected_request_hash"] = json!("x".repeat(2000));
                StatusCode::PAYLOAD_TOO_LARGE
            }
        };
        let response = post_raw(&f.app, actor, &uri, value.to_string(), options).await;
        assert_eq!(response.0, expected, "{mode}: {}", response.1);
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM review_check_recoveries")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn recovery_http_rechecks_original_authority_and_admin_replay_privilege(pool: PgPool) {
    let (f, admin, _, lease, path) = stopped(&pool, false).await;
    let uri = format!("{path}/recover");
    let request = body(&lease);
    sqlx::query("UPDATE users SET status='DISABLED' WHERE id=$1")
        .bind(f.owner.id)
        .execute(&pool)
        .await
        .unwrap();
    let response = post_raw(
        &f.app,
        Some(&admin),
        &uri,
        request.to_string(),
        PostOptions::new(&admin, "unused"),
    )
    .await;
    assert_eq!(response.0, StatusCode::CONFLICT);
    assert_eq!(
        response.1["error"]["code"],
        "CHECK_RECOVERY_CONTEXT_CHANGED"
    );
    assert_eq!(get(&f.app, &admin, &path, true).await.0, StatusCode::OK);
    sqlx::query("UPDATE users SET status='ACTIVE' WHERE id=$1")
        .bind(f.owner.id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        post_raw(
            &f.app,
            Some(&admin),
            &uri,
            request.to_string(),
            PostOptions::new(&admin, "unused")
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    sqlx::query("UPDATE memberships SET role='MAINTAINER' WHERE user_id=$1")
        .bind(admin.id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        post_raw(
            &f.app,
            Some(&admin),
            &uri,
            request.to_string(),
            PostOptions::new(&admin, "unused")
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn recovery_http_distinguishes_busy_mismatched_requests_and_quota_without_new_markers(
    pool: PgPool,
) {
    let (f, admin, store, lease, path) = stopped(&pool, true).await;
    let uri = format!("{path}/recover");
    let first = body(&lease);
    for value in [first.clone(), body(&lease), body(&lease)] {
        let result = post_raw(
            &f.app,
            Some(&admin),
            &uri,
            value.to_string(),
            PostOptions::new(&admin, "unused"),
        )
        .await;
        if result.0 == StatusCode::TOO_MANY_REQUESTS {
            assert_eq!(result.1["error"]["code"], "CHECK_RECOVERY_LIMIT");
            break;
        }
        assert_eq!(result.0, StatusCode::ACCEPTED, "{}", result.1);
        let busy = post_raw(
            &f.app,
            Some(&admin),
            &uri,
            body(&lease).to_string(),
            PostOptions::new(&admin, "unused"),
        )
        .await;
        assert_eq!(busy.0, StatusCode::CONFLICT);
        assert_eq!(busy.1["error"]["code"], "CHECK_RECOVERY_NOT_STOPPED");
        let resumed = store.claim(Uuid::new_v4(), 120).await.unwrap().unwrap();
        store
            .defer(&resumed, "CHECK_PROVIDER_UNAVAILABLE", None, None)
            .await
            .unwrap();
    }
    let replay = post_raw(
        &f.app,
        Some(&admin),
        &uri,
        first.to_string(),
        PostOptions::new(&admin, "unused"),
    )
    .await;
    assert_eq!(replay.0, StatusCode::ACCEPTED);
    assert_eq!(replay.1["replayed"], true);
    let mut wrong = first.clone();
    wrong["expected_request_hash"] = json!("0".repeat(64));
    let mismatch = post_raw(
        &f.app,
        Some(&admin),
        &uri,
        wrong.to_string(),
        PostOptions::new(&admin, "unused"),
    )
    .await;
    assert_eq!(mismatch.0, StatusCode::CONFLICT);
    assert_eq!(
        mismatch.1["error"]["code"],
        "CHECK_RECOVERY_IDEMPOTENCY_MISMATCH"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM review_check_recoveries")
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM review_check_writes")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn provider_cooldown_blocks_new_recovery_but_not_status_or_exact_receipt_observation(
    pool: PgPool,
) {
    let (f, admin, store, lease, path) = stopped(&pool, true).await;
    let uri = format!("{path}/recover");
    let first = body(&lease);
    assert_eq!(
        post_raw(
            &f.app,
            Some(&admin),
            &uri,
            first.to_string(),
            PostOptions::new(&admin, "unused")
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    let resumed = store.claim(Uuid::new_v4(), 120).await.unwrap().unwrap();
    store
        .defer(&resumed, "CHECK_PROVIDER_RATE_LIMITED", None, Some(17))
        .await
        .unwrap();
    let request = Request::builder()
        .method("POST")
        .uri(&uri)
        .header("Content-Type", "application/json")
        .header("Cookie", format!("specspace-session={}", admin.token))
        .header("X-Expected-Actor", admin.id.to_string())
        .header("Origin", "http://localhost:4200")
        .header("X-XSRF-TOKEN", &admin.csrf)
        .body(Body::from(body(&lease).to_string()))
        .unwrap();
    let response = f.app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(
        (1..=17).contains(
            &response.headers()["Retry-After"]
                .to_str()
                .unwrap()
                .parse::<u32>()
                .unwrap()
        )
    );
    let value: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(value["error"]["code"], "CHECK_PROVIDER_COOLDOWN");
    assert_eq!(get(&f.app, &admin, &path, true).await.0, StatusCode::OK);
    let replay = post_raw(
        &f.app,
        Some(&admin),
        &uri,
        first.to_string(),
        PostOptions::new(&admin, "unused"),
    )
    .await;
    assert_eq!(replay.0, StatusCode::ACCEPTED);
    assert_eq!(replay.1["replayed"], true);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM review_check_recoveries")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
}
