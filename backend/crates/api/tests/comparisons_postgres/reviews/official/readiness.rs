use super::*;

#[sqlx::test(migrations = "../../migrations")]
async fn readiness_http_reports_server_evidence_without_provider_reads_writes_or_merge_authority(
    pool: PgPool,
) {
    let (f, product, development, decisions, fp) = ready(&pool, true).await;
    for (reviewer, domain) in [(&product, "PRODUCT"), (&development, "DEVELOPMENT")] {
        let mut value = body(&fp, "APPROVE_ALL");
        value["domain"] = json!(domain);
        assert_eq!(
            post(&f, reviewer, &decisions, value).await.0,
            StatusCode::CREATED
        );
    }
    let path = decisions.replace("/decisions", "/readiness");
    let reads = f.provider.ref_reads.load(Ordering::SeqCst);
    let audit: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_events")
        .fetch_one(&pool)
        .await
        .unwrap();
    let report = get(&f.app, &f.actor, &path, true).await;
    assert_eq!(report.0, StatusCode::OK, "{}", report.1);
    assert_eq!(report.1["actor_id"], f.actor.id.to_string());
    assert_eq!(report.1["fingerprint"], fp);
    assert_eq!(report.1["head_sha"], HEAD);
    assert_eq!(report.1["target_sha"], MAIN);
    assert_eq!(report.1["current_for_metadata"], true);
    assert_eq!(report.1["provider_refs_verified"], false);
    assert_eq!(report.1["evaluation"]["conclusion"], "PENDING");
    let issues = report.1["evaluation"]["issues"].as_array().unwrap();
    assert_eq!(issues.len(), 1);
    assert!(
        issues
            .iter()
            .any(|i| i["code"] == "PROVIDER_REFS_UNVERIFIED")
    );
    assert!(!issues.iter().any(|i| i["gate"] == "BLOCKING_QUESTIONS"));
    assert!(!report.1.to_string().contains("PRIVATE SOURCE SENTINEL"));
    assert_eq!(f.provider.ref_reads.load(Ordering::SeqCst), reads);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM audit_events")
            .fetch_one(&pool)
            .await
            .unwrap(),
        audit
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM proposals WHERE id=$1")
            .bind(f.proposal)
            .fetch_one(&pool)
            .await
            .unwrap(),
        "IN_REVIEW"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn readiness_http_rejects_unbound_identity_foreign_scope_and_extra_query_evidence(
    pool: PgPool,
) {
    let (f, product, _, decisions, _) = ready(&pool, true).await;
    let path = decisions.replace("/decisions", "/readiness");
    let reads = f.provider.ref_reads.load(Ordering::SeqCst);
    for query in [
        "?passed=true",
        "?head_sha=forged",
        "?actor_id=forged",
        "?offset=0",
    ] {
        assert_eq!(
            get(&f.app, &product, &format!("{path}{query}"), true)
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
    }
    let foreign_org = Uuid::new_v4();
    sqlx::query("INSERT INTO organizations(id,name) VALUES($1,'Foreign readiness fixture')")
        .bind(foreign_org)
        .execute(&pool)
        .await
        .unwrap();
    let foreign = user(&pool, foreign_org, "READER").await;
    assert_eq!(
        get(&f.app, &foreign, &path, true).await.0,
        StatusCode::NOT_FOUND
    );
    for (cookie, expected_actor, expected_status) in [
        (None, None, StatusCode::UNAUTHORIZED),
        (Some(&product), Some(f.owner.id), StatusCode::CONFLICT),
    ] {
        let mut builder = Request::builder().uri(&path);
        if let Some(user) = cookie {
            builder = builder.header("Cookie", format!("specspace-session={}", user.token));
        }
        if let Some(actor) = expected_actor {
            builder = builder.header("X-Expected-Actor", actor.to_string());
        }
        let response = f
            .app
            .clone()
            .oneshot(builder.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), expected_status);
        assert_eq!(response.headers()["cache-control"], "no-store");
    }
    sqlx::query("UPDATE users SET status='DISABLED' WHERE id=$1")
        .bind(product.id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        get(&f.app, &product, &path, true).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(f.provider.ref_reads.load(Ordering::SeqCst), reads);
}
