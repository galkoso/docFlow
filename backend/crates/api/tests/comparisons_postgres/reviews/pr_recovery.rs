use super::*;
use specspace_application::pr_sync::PrSiteOrigin;
use specspace_infrastructure::pr_sync::{PgPrSync, PrSyncLease};

async fn stopped(pool: &PgPool, started: bool) -> (Fixture, User, PgPrSync, PrSyncLease, String) {
    let (f, _, _) = configured(pool, false).await;
    let (_, snapshot) = publish(&f, 2).await;
    let admin = user(pool, f.organization, "ADMIN").await;
    let store = PgPrSync::new(pool.clone());
    let event = sqlx::query_scalar(
        "SELECT id FROM outbox WHERE entity_id=$1 AND event_type='REVIEW_SNAPSHOT_PUBLISHED'",
    )
    .bind(f.proposal)
    .fetch_one(pool)
    .await
    .unwrap();
    store
        .reserve(
            f.owner.id,
            f.proposal,
            event,
            &PrSiteOrigin::parse("https://specs.example", false).unwrap(),
        )
        .await
        .unwrap();
    let lease = store.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    if started {
        store.start_write(&lease, &[]).await.unwrap();
    }
    store
        .defer(&lease, "PR_PROVIDER_UNAVAILABLE", None)
        .await
        .unwrap();
    let path = format!("{}/{snapshot}/pr-sync/recover", history_path(&f));
    (f, admin, store, lease, path)
}
fn body(lease: &PrSyncLease) -> Value {
    json!({"request_id":Uuid::new_v4(),"expected_fingerprint":lease.intent().source().fingerprint})
}

#[sqlx::test(migrations = "../../migrations")]
async fn original_editor_revocation_is_a_recovery_conflict_not_loss_of_operator_read_access(
    pool: PgPool,
) {
    let (f, admin, _, lease, path) = stopped(&pool, false).await;
    sqlx::query("UPDATE users SET status='DISABLED' WHERE id=$1")
        .bind(f.owner.id)
        .execute(&pool)
        .await
        .unwrap();
    let response = post_raw(
        &f.app,
        Some(&admin),
        &path,
        body(&lease).to_string(),
        PostOptions::new(&admin, "unused"),
    )
    .await;
    assert_eq!(response.0, StatusCode::CONFLICT);
    assert_eq!(response.1["error"]["code"], "PR_RECOVERY_CONTEXT_CHANGED");
    let status = path.strip_suffix("/recover").unwrap();
    assert_eq!(get(&f.app, &admin, status, true).await.0, StatusCode::OK);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM pr_sync_recoveries")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}
#[sqlx::test(migrations = "../../migrations")]
async fn operator_post_requeues_same_stopped_operation_and_replays_without_provider_io(
    pool: PgPool,
) {
    let (f, admin, store, lease, path) = stopped(&pool, false).await;
    let value = body(&lease);
    let reads = f.provider.reads.load(Ordering::SeqCst);
    let first = post_raw(
        &f.app,
        Some(&admin),
        &path,
        value.to_string(),
        PostOptions::new(&admin, "unused"),
    )
    .await;
    assert_eq!(first.0, StatusCode::ACCEPTED, "{}", first.1);
    assert_eq!(first.1["mode"], "RETRY_UNSTARTED");
    assert_eq!(first.1["replayed"], false);
    assert_eq!(first.2.unwrap(), path.trim_end_matches("/recover"));
    let replay = post_raw(
        &f.app,
        Some(&admin),
        &path,
        value.to_string(),
        PostOptions::new(&admin, "unused"),
    )
    .await;
    assert_eq!(replay.0, StatusCode::ACCEPTED);
    assert_eq!(replay.1["replayed"], true);
    assert!(replay.1.get("job_id").is_none() && replay.1.get("operation_id").is_none());
    assert_eq!(f.provider.reads.load(Ordering::SeqCst), reads);
    let resumed = store.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    assert_eq!(resumed.operation_id(), lease.operation_id());
    let busy = post_raw(
        &f.app,
        Some(&admin),
        &path,
        body(&lease).to_string(),
        PostOptions::new(&admin, "unused"),
    )
    .await;
    assert_eq!(busy.0, StatusCode::CONFLICT);
    assert_eq!(busy.1["error"]["code"], "PR_RECOVERY_NOT_STOPPED");
}
#[sqlx::test(migrations = "../../migrations")]
async fn recovery_http_rejects_untrusted_authority_queries_and_payloads_before_admission(
    pool: PgPool,
) {
    let (f, admin, _, lease, path) = stopped(&pool, false).await;
    let value = body(&lease);
    for mode in 0..7 {
        let mut options = PostOptions::new(&admin, "unused");
        let mut user = Some(&admin);
        let mut uri = path.clone();
        let mut payload = value.clone();
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
            _ => {
                payload["operation_id"] = json!(lease.operation_id());
                StatusCode::BAD_REQUEST
            }
        };
        let response = post_raw(&f.app, user, &uri, payload.to_string(), options).await;
        assert_eq!(response.0, expected, "{mode}: {}", response.1);
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM pr_sync_recoveries")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}
#[sqlx::test(migrations = "../../migrations")]
async fn recovery_http_keeps_unknown_write_unknown_and_limits_new_operator_windows(pool: PgPool) {
    let (f, admin, store, lease, path) = stopped(&pool, true).await;
    let mut first_body = None;
    for attempt in 0..3 {
        let value = body(&lease);
        if attempt == 0 {
            first_body = Some(value.clone());
        }
        let response = post_raw(
            &f.app,
            Some(&admin),
            &path,
            value.to_string(),
            PostOptions::new(&admin, "unused"),
        )
        .await;
        if attempt == 2 {
            assert_eq!(response.0, StatusCode::TOO_MANY_REQUESTS);
            break;
        }
        assert_eq!(response.0, StatusCode::ACCEPTED, "{}", response.1);
        assert_eq!(response.1["mode"], "OBSERVE_STARTED");
        let resumed = store.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
        assert!(store.start_write(&resumed, &[]).await.is_err());
        store
            .defer(&resumed, "PR_RESULT_UNKNOWN", None)
            .await
            .unwrap();
    }
    let replay = post_raw(
        &f.app,
        Some(&admin),
        &path,
        first_body.unwrap().to_string(),
        PostOptions::new(&admin, "unused"),
    )
    .await;
    assert_eq!(replay.0, StatusCode::ACCEPTED);
    assert_eq!(replay.1["replayed"], true);
    let state: String = sqlx::query_scalar("SELECT state FROM git_operations WHERE id=$1")
        .bind(lease.operation_id())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state, "UNKNOWN");
}
