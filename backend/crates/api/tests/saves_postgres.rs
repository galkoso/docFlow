use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use specspace_api::{AppState, auth::CookiePolicy, router};
use specspace_infrastructure::sessions::SessionStore;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;
const HEAD: &str = "1111111111111111111111111111111111111111";

struct Actor {
    id: Uuid,
    token: String,
    csrf: String,
}
struct Fixture {
    actor: Actor,
    organization: Uuid,
    proposal: Uuid,
}

async fn actor(pool: &PgPool, organization: Uuid, role: &str) -> Actor {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO users(id,issuer,external_subject,display_name) VALUES($1,'fixture',$1::text,'Synthetic recovery user')").bind(id).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO memberships(organization_id,user_id,role) VALUES($1,$2,$3)")
        .bind(organization)
        .bind(id)
        .bind(role)
        .execute(pool)
        .await
        .unwrap();
    let credentials = SessionStore::new(pool.clone())
        .mint(id, 3600)
        .await
        .unwrap();
    Actor {
        id,
        token: credentials.session_cookie_value().into(),
        csrf: credentials.csrf_cookie_value().into(),
    }
}
async fn fixture(pool: &PgPool) -> Fixture {
    let organization = Uuid::new_v4();
    let repository = Uuid::new_v4();
    let team = Uuid::new_v4();
    let proposal = Uuid::new_v4();
    sqlx::query("INSERT INTO organizations(id,name) VALUES($1,'Synthetic recovery organization')")
        .bind(organization)
        .execute(pool)
        .await
        .unwrap();
    let actor = actor(pool, organization, "CONTRIBUTOR").await;
    sqlx::query("INSERT INTO teams(id,organization_id,name) VALUES($1,$2,'Recovery team')")
        .bind(team)
        .bind(organization)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO repositories(id,organization_id,provider,remote_id,installation_id) VALUES($1,$2,'GITHUB','42','7')").bind(repository).bind(organization).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO proposals(id,organization_id,repository_id,title,branch,owner_id,author_id,team_id,state,description,ui_impact,base_sha,head_sha,target_sha) VALUES($1,$2,$3,'Synthetic recovery proposal','change/recovery',$4,$4,$5,'DRAFT','Recovery proposal description','UNDECIDED',$6,$6,$6)")
        .bind(proposal).bind(organization).bind(repository).bind(actor.id).bind(team).bind(HEAD).execute(pool).await.unwrap();
    Fixture {
        actor,
        organization,
        proposal,
    }
}

fn app(pool: &PgPool) -> Router {
    let mut state = AppState::new(pool.clone());
    state.cookies = CookiePolicy::new("http://localhost:4200", true).unwrap();
    router(state)
}
fn input() -> Value {
    json!({"expected_head_sha":HEAD,"expected_proposal_version":1,"message":"Update two chapters","documents":[{"document_id":"DOC-MEDIA","expected_document_sha256":"a".repeat(64),"markdown":"PRIVATE SOURCE SENTINEL"}]})
}
async fn request(
    app: &Router,
    actor: &Actor,
    path: &str,
    body: Option<&Value>,
    key: Option<&str>,
    bound: bool,
    csrf: bool,
) -> (StatusCode, Value, Option<String>) {
    let mut builder = Request::builder()
        .method(if body.is_some() { "POST" } else { "GET" })
        .uri(path)
        .header("Cookie", format!("specspace-session={}", actor.token));
    if bound {
        builder = builder.header("X-Expected-Actor", actor.id.to_string());
    }
    if csrf {
        builder = builder
            .header("Origin", "http://localhost:4200")
            .header("X-XSRF-TOKEN", &actor.csrf);
    }
    if let Some(key) = key {
        builder = builder.header("Idempotency-Key", key);
    }
    let response = app
        .clone()
        .oneshot(
            builder
                .header("Content-Type", "application/json")
                .body(body.map_or_else(Body::empty, |v| Body::from(serde_json::to_vec(v).unwrap())))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    assert_eq!(response.headers()["cache-control"], "no-store");
    let location = response
        .headers()
        .get("Location")
        .map(|v| v.to_str().unwrap().to_owned());
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap(), location)
}
async fn submit(app: &Router, f: &Fixture, key: &str) -> (Value, String) {
    let (status, ticket, location) = request(
        app,
        &f.actor,
        &format!("/api/v1/proposals/{}/commits", f.proposal),
        Some(&input()),
        Some(key),
        true,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{ticket}");
    assert!(ticket.get("head_sha").is_none());
    (ticket, location.unwrap())
}
#[sqlx::test(migrations = "../../migrations")]
async fn submission_is_queued_private_idempotent_and_replays_after_head_and_state_change(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    let app = app(&pool);
    let key = Uuid::new_v4().to_string();
    let (ticket, url) = submit(&app, &f, &key).await;
    let (status, job, _) = request(&app, &f.actor, &url, None, None, true, false).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(job["phase"], "QUEUED");
    assert!(job["save"].is_null());
    assert!(!job.to_string().contains("PRIVATE SOURCE"));
    assert!(!job.to_string().contains("installation"));
    sqlx::query(
        "UPDATE proposals SET head_sha=repeat('2',40),version=2,state='CLOSED' WHERE id=$1",
    )
    .bind(f.proposal)
    .execute(&pool)
    .await
    .unwrap();
    let (again, again_url) = submit(&app, &f, &key).await;
    assert_eq!(again, ticket);
    assert_eq!(again_url, url);
    let (status, error, _) = request(
        &app,
        &f.actor,
        &format!("/api/v1/proposals/{}/commits", f.proposal),
        Some(&input()),
        Some(&Uuid::new_v4().to_string()),
        true,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "SAVE_CONTEXT_CHANGED");
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM git_operations WHERE kind='COMMIT'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}
#[sqlx::test(migrations = "../../migrations")]
async fn mutation_requires_csrf_bound_actor_key_scope_and_valid_intent(pool: PgPool) {
    let f = fixture(&pool).await;
    let app = app(&pool);
    let path = format!("/api/v1/proposals/{}/commits", f.proposal);
    let key = Uuid::new_v4().to_string();
    for (bound, csrf, key_value, expected) in [
        (false, true, Some(key.as_str()), StatusCode::CONFLICT),
        (true, false, Some(key.as_str()), StatusCode::FORBIDDEN),
        (true, true, None, StatusCode::BAD_REQUEST),
    ] {
        assert_eq!(
            request(
                &app,
                &f.actor,
                &path,
                Some(&input()),
                key_value,
                bound,
                csrf
            )
            .await
            .0,
            expected
        );
    }
    let mut invalid = input();
    invalid["documents"][0]["expected_document_sha256"] = json!("wrong");
    assert_eq!(
        request(
            &app,
            &f.actor,
            &path,
            Some(&invalid),
            Some(&key),
            true,
            true
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let other = actor(&pool, f.organization, "CONTRIBUTOR").await;
    assert_eq!(
        request(&app, &other, &path, Some(&input()), Some(&key), true, true)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let reader = actor(&pool, f.organization, "READER").await;
    assert_eq!(
        request(&app, &reader, &path, Some(&input()), Some(&key), true, true)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let missing = format!("/api/v1/proposals/{}/commits", Uuid::new_v4());
    assert_eq!(
        request(
            &app,
            &f.actor,
            &missing,
            Some(&input()),
            Some(&key),
            true,
            true
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM git_operations")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}
#[sqlx::test(migrations = "../../migrations")]
async fn changed_intents_and_concurrent_mutations_are_explicit_conflicts(pool: PgPool) {
    let f = fixture(&pool).await;
    let app = app(&pool);
    let key = Uuid::new_v4().to_string();
    submit(&app, &f, &key).await;
    let mut changed = input();
    changed["documents"][0]["markdown"] = json!("Other source");
    let path = format!("/api/v1/proposals/{}/commits", f.proposal);
    let (status, error, _) = request(
        &app,
        &f.actor,
        &path,
        Some(&changed),
        Some(&key),
        true,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "IDEMPOTENCY_MISMATCH");
    let (_, error, _) = request(
        &app,
        &f.actor,
        &path,
        Some(&changed),
        Some(&Uuid::new_v4().to_string()),
        true,
        true,
    )
    .await;
    assert_eq!(error["error"]["code"], "GIT_OPERATION_PENDING");
}
#[sqlx::test(migrations = "../../migrations")]
async fn job_polling_requires_captured_actor_and_current_private_scope(pool: PgPool) {
    let f = fixture(&pool).await;
    let app = app(&pool);
    let (_, url) = submit(&app, &f, &Uuid::new_v4().to_string()).await;
    assert_eq!(
        request(&app, &f.actor, &url, None, None, false, false)
            .await
            .0,
        StatusCode::CONFLICT
    );
    let admin = actor(&pool, f.organization, "ADMIN").await;
    assert_eq!(
        request(&app, &admin, &url, None, None, true, false).await.0,
        StatusCode::NOT_FOUND
    );
    sqlx::query("UPDATE memberships SET role='READER' WHERE user_id=$1")
        .bind(f.actor.id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        request(&app, &f.actor, &url, None, None, true, false)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn job_result_contract_requires_both_publications_and_never_exposes_raw_details(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    let app = app(&pool);
    let (ticket, url) = submit(&app, &f, &Uuid::new_v4().to_string()).await;
    let op = Uuid::parse_str(ticket["operation_id"].as_str().unwrap()).unwrap();
    let job = Uuid::parse_str(ticket["job_id"].as_str().unwrap()).unwrap();
    // Synthetic persisted outcomes exercise HTTP projection, not Git verification.
    let result = json!({"proposal_id":f.proposal,"head_sha":HEAD,"proposal_version":1,"committed_at":null,"no_op":true,"reconciled":false,"documents":[],"private_source":"PRIVATE SOURCE SENTINEL"});
    sqlx::query("UPDATE jobs SET state='SUCCEEDED',result=$2 WHERE id=$1")
        .bind(job)
        .bind(result)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        request(&app, &f.actor, &url, None, None, true, false)
            .await
            .0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    sqlx::query("UPDATE git_operations SET state='SUCCEEDED',result_sha=$2 WHERE id=$1")
        .bind(op)
        .bind(HEAD)
        .execute(&pool)
        .await
        .unwrap();
    let (status, value, _) = request(&app, &f.actor, &url, None, None, true, false).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["save"]["no_op"], true);
    assert!(value["save"]["committed_at"].is_null());
    assert!(!value.to_string().contains("PRIVATE SOURCE"));
    sqlx::query("UPDATE git_operations SET state='UNKNOWN',error_code='SAVE_VERIFICATION_PENDING' WHERE id=$1").bind(op).execute(&pool).await.unwrap();
    sqlx::query("UPDATE jobs SET state='FAILED',result=$2 WHERE id=$1").bind(job).bind(json!({"details":{"code":"DOCUMENT_CHANGED","document_id":"DOC-MEDIA","markdown":"PRIVATE SOURCE SENTINEL"}})).execute(&pool).await.unwrap();
    let (_, value, _) = request(&app, &f.actor, &url, None, None, true, false).await;
    assert_eq!(value["phase"], "RECONCILIATION_REQUIRED");
    assert!(value["save"].is_null());
    assert_eq!(value["save_failure"]["document_id"], "DOC-MEDIA");
    assert!(!value.to_string().contains("PRIVATE SOURCE"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn intent_identity_cannot_follow_a_different_shared_cookie_and_sizes_are_explicit(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    let app = app(&pool);
    let other = actor(&pool, f.organization, "ADMIN").await;
    let path = format!("/api/v1/proposals/{}/commits", f.proposal);
    for bindings in [
        vec![f.actor.id.to_string()],
        vec![other.id.to_string(), other.id.to_string()],
        vec!["invalid".to_owned()],
    ] {
        let mut builder = Request::builder()
            .method("POST")
            .uri(&path)
            .header("Cookie", format!("specspace-session={}", other.token))
            .header("Origin", "http://localhost:4200")
            .header("X-XSRF-TOKEN", &other.csrf)
            .header("Idempotency-Key", Uuid::new_v4().to_string())
            .header("Content-Type", "application/json");
        for binding in bindings {
            builder = builder.header("X-Expected-Actor", binding);
        }
        let response = app
            .clone()
            .oneshot(builder.body(Body::from(input().to_string())).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }
    let mut large = input();
    large["documents"][0]["markdown"] = json!("x".repeat(2 * 1024 * 1024 + 1));
    assert_eq!(
        request(
            &app,
            &f.actor,
            &path,
            Some(&large),
            Some(&Uuid::new_v4().to_string()),
            true,
            true
        )
        .await
        .0,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM git_operations")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}
