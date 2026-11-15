use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use specspace_api::{AppState, auth::CookiePolicy, router};
use specspace_application::draft::{DraftRetention, MAX_DRAFT_BYTES};
use specspace_infrastructure::{drafts::PgDrafts, sessions::SessionStore};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

const HEAD: &str = "1111111111111111111111111111111111111111";
const NEW: &str = "2222222222222222222222222222222222222222";
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
fn app(pool: &PgPool, days: Option<u16>) -> Router {
    let mut state = AppState::new(pool.clone());
    state.cookies = CookiePolicy::new("http://localhost:4200", true).unwrap();
    if let Some(days) = days {
        state.configure_draft_retention(DraftRetention::new(days).unwrap());
    }
    router(state)
}
fn input(document: &str, version: i64, content: &str) -> Value {
    json!({"save_id": Uuid::new_v4(), "document_id": document,"base_head_sha": HEAD,"expected_draft_version":version,"content_utf8":content})
}
async fn request(
    app: &Router,
    actor: &Actor,
    proposal: Uuid,
    body: Option<&Value>,
    csrf: bool,
    document: Option<&str>,
) -> (StatusCode, Value) {
    let uri = format!(
        "/api/v1/proposals/{proposal}/draft{}",
        document
            .map(|id| format!("?document_id={id}"))
            .unwrap_or_default()
    );
    let mut builder = Request::builder()
        .method(if body.is_some() { "PUT" } else { "GET" })
        .uri(uri)
        .header("Cookie", format!("specspace-session={}", actor.token));
    if csrf {
        builder = builder
            .header("Origin", "http://localhost:4200")
            .header("X-XSRF-TOKEN", &actor.csrf);
    }
    let response = app
        .clone()
        .oneshot(
            builder
                .header("Content-Type", "application/json")
                .body(body.map_or_else(Body::empty, |body| {
                    Body::from(serde_json::to_vec(body).unwrap())
                }))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    assert_eq!(response.headers()["cache-control"], "no-store");
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[sqlx::test(migrations = "../../migrations")]
async fn recovery_is_private_source_preserving_and_independent_of_git(pool: PgPool) {
    let f = fixture(&pool).await;
    let other = actor(&pool, f.organization, "ADMIN").await;
    let app = app(&pool, Some(30));
    let content = "---\r\nunfinished ** שָׁלוֹם\r\n<!-- private recovery only -->";
    let body = input("DOC-MEDIA", 0, content);
    let (status, saved) = request(&app, &f.actor, f.proposal, Some(&body), true, None).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["context"]["retention_days"], 30);
    assert_eq!(saved["context"]["current_head_sha"], HEAD);
    assert!(saved["buffer"]["version"].as_i64().unwrap() > 0);
    assert!(saved["buffer"]["content_utf8"].is_null());
    let (_, list) = request(&app, &f.actor, f.proposal, None, false, None).await;
    assert_eq!(list["buffers"].as_array().unwrap().len(), 1);
    assert!(list["buffers"][0]["content_utf8"].is_null());
    let (_, document) = request(&app, &f.actor, f.proposal, None, false, Some("DOC-MEDIA")).await;
    assert_eq!(document["buffers"][0]["content_utf8"], content);
    assert_eq!(document["buffers"][0]["base_is_current"], true);
    let (_, private) = request(&app, &other, f.proposal, None, false, Some("DOC-MEDIA")).await;
    assert_eq!(
        private["buffers"],
        json!([]),
        "even an administrator cannot read another user's buffer"
    );
    let persisted: (String, String, i64) =
        sqlx::query_as("SELECT state,head_sha,version FROM proposals WHERE id=$1")
            .bind(f.proposal)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(persisted, ("DRAFT".into(), HEAD.into(), 1));
    let (operations, jobs): (i64, i64) =
        sqlx::query_as("SELECT (SELECT count(*) FROM git_operations),(SELECT count(*) FROM jobs)")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((operations, jobs), (0, 0));
    let audit: (String, String, String) =
        sqlx::query_as("SELECT action,result,after_ref FROM audit_events")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(audit.0, "DRAFT_RECOVERY_SAVED");
    assert_eq!(audit.1, "RECOVERY_ONLY");
    assert!(audit.2.starts_with("draft:"));
    assert!(!format!("{audit:?}").contains(content));
}

#[sqlx::test(migrations = "../../migrations")]
async fn recovery_tabs_use_compare_and_swap_and_lost_responses_replay_without_overwrite(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    let app = app(&pool, Some(30));
    let first = input("DOC-MEDIA", 0, "first unfinished source");
    let second = input("DOC-MEDIA", 0, "second unfinished source");
    let (one, two) = tokio::join!(
        request(&app, &f.actor, f.proposal, Some(&first), true, None),
        request(&app, &f.actor, f.proposal, Some(&second), true, None)
    );
    let (accepted, winner, loser) = if one.0 == StatusCode::OK {
        (&one, &first, &two)
    } else {
        (&two, &second, &one)
    };
    assert_eq!(accepted.0, StatusCode::OK, "{accepted:?}");
    assert_eq!(loser.0, StatusCode::CONFLICT);
    assert_eq!(loser.1["error"]["code"], "DRAFT_VERSION_CONFLICT");
    let version = accepted.1["buffer"]["version"].as_i64().unwrap();
    let (status, replayed) = request(&app, &f.actor, f.proposal, Some(winner), true, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replayed["replayed"], true);
    assert_eq!(replayed["buffer"]["version"], version);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_events")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    let mut changed = winner.clone();
    changed["content_utf8"] = json!("changed same save identity");
    let (_, error) = request(&app, &f.actor, f.proposal, Some(&changed), true, None).await;
    assert_eq!(error["error"]["code"], "DRAFT_SAVE_ID_MISMATCH");
    let next = input("DOC-MEDIA", version, "explicit next version");
    let (status, next_saved) = request(&app, &f.actor, f.proposal, Some(&next), true, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(next_saved["buffer"]["version"].as_i64().unwrap() > version);
    assert_eq!(
        request(&app, &f.actor, f.proposal, Some(winner), true, None)
            .await
            .0,
        StatusCode::CONFLICT
    );
    let (_, current) = request(&app, &f.actor, f.proposal, None, false, Some("DOC-MEDIA")).await;
    assert_eq!(
        current["buffers"][0]["content_utf8"],
        "explicit next version"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn recovery_survives_changed_heads_and_final_states_but_obeys_current_assignments(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    let editor = actor(&pool, f.organization, "CONTRIBUTOR").await;
    let reader = actor(&pool, f.organization, "READER").await;
    let foreign = fixture(&pool).await;
    sqlx::query("INSERT INTO proposal_collaborators(organization_id,proposal_id,user_id,access) VALUES($1,$2,$3,'EDIT')")
        .bind(f.organization).bind(f.proposal).bind(editor.id).execute(&pool).await.unwrap();
    let app = app(&pool, Some(30));
    let body = input("DOC-MEDIA", 0, "stale local edit kept for comparison");
    sqlx::query("UPDATE proposals SET head_sha=$2,version=version+1 WHERE id=$1")
        .bind(f.proposal)
        .bind(NEW)
        .execute(&pool)
        .await
        .unwrap();
    let (status, saved) = request(&app, &editor, f.proposal, Some(&body), true, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(saved["buffer"]["base_is_current"], false);
    assert_eq!(saved["context"]["current_head_sha"], NEW);
    let mut version = saved["buffer"]["version"].as_i64().unwrap();
    for state in ["CLOSED", "MERGED"] {
        sqlx::query("UPDATE proposals SET state=$2 WHERE id=$1")
            .bind(f.proposal)
            .bind(state)
            .execute(&pool)
            .await
            .unwrap();
        let body = input("DOC-MEDIA", version, "recovery after final state");
        let (status, saved) = request(&app, &editor, f.proposal, Some(&body), true, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(saved["context"]["can_edit"], false);
        assert_eq!(saved["context"]["can_store_recovery"], true);
        version = saved["buffer"]["version"].as_i64().unwrap();
    }
    assert_eq!(
        request(
            &app,
            &reader,
            f.proposal,
            Some(&input("DOC-MEDIA", 0, "reader must not store")),
            true,
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(&app, &foreign.actor, f.proposal, None, false, None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(&app, &foreign.actor, f.proposal, Some(&body), true, None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    sqlx::query("DELETE FROM proposal_collaborators WHERE proposal_id=$1 AND user_id=$2")
        .bind(f.proposal)
        .bind(editor.id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        request(
            &app,
            &editor,
            f.proposal,
            Some(&input("DOC-MEDIA", version, "revoked edit")),
            true,
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (_, own) = request(&app, &editor, f.proposal, None, false, Some("DOC-MEDIA")).await;
    assert_eq!(
        own["buffers"][0]["content_utf8"],
        "recovery after final state"
    );
    assert_eq!(own["context"]["can_store_recovery"], false);
    sqlx::query("UPDATE users SET status='DISABLED' WHERE id=$1")
        .bind(editor.id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        request(&app, &editor, f.proposal, None, false, None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn recovery_expiry_policy_and_cleanup_never_reuse_versions(pool: PgPool) {
    let f = fixture(&pool).await;
    let disabled = app(&pool, None);
    let body = input("DOC-MEDIA", 0, "retained temporary source");
    let (status, error) = request(&disabled, &f.actor, f.proposal, Some(&body), true, None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(error["error"]["code"], "RECOVERY_POLICY_NOT_CONFIGURED");
    let (_, context) = request(&disabled, &f.actor, f.proposal, None, false, None).await;
    assert!(context["context"]["retention_days"].is_null());
    assert_eq!(context["context"]["can_store_recovery"], false);
    let app = app(&pool, Some(30));
    let (_, saved) = request(&app, &f.actor, f.proposal, Some(&body), true, None).await;
    let old = saved["buffer"]["version"].as_i64().unwrap();
    sqlx::query("UPDATE draft_buffers SET updated_at=now()-interval '20 days',expires_at=now()+interval '10 days'").execute(&pool).await.unwrap();
    let shortened = PgDrafts::new(pool.clone(), Some(DraftRetention::new(7).unwrap()));
    assert!(
        shortened
            .read(f.actor.id, f.proposal, Some("DOC-MEDIA"))
            .await
            .unwrap()
            .buffers
            .is_empty()
    );
    assert_eq!(shortened.purge_expired().await.unwrap(), 1);
    let (_, saved) = request(
        &app,
        &f.actor,
        f.proposal,
        Some(&input("DOC-MEDIA", 0, "new buffer after deletion")),
        true,
        None,
    )
    .await;
    assert!(saved["buffer"]["version"].as_i64().unwrap() > old);
    assert_eq!(
        request(
            &app,
            &f.actor,
            f.proposal,
            Some(&input("DOC-MEDIA", old, "old tab must conflict")),
            true,
            None
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    sqlx::query("UPDATE draft_buffers SET expires_at=now()-interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        request(&app, &f.actor, f.proposal, None, false, Some("DOC-MEDIA"))
            .await
            .1["buffers"],
        json!([])
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn recovery_enforces_byte_quotas_and_csrf_without_creating_git_state(pool: PgPool) {
    let f = fixture(&pool).await;
    let app = app(&pool, Some(30));
    let body = input("DOC-MEDIA", 0, "private input");
    assert_eq!(
        request(&app, &f.actor, f.proposal, Some(&body), false, None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let mut invalid = body.clone();
    invalid["organization_id"] = json!(f.organization);
    assert_eq!(
        request(&app, &f.actor, f.proposal, Some(&invalid), true, None)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(
            &app,
            &f.actor,
            f.proposal,
            Some(&input("DOC-MEDIA", 0, &"x".repeat(MAX_DRAFT_BYTES + 1))),
            true,
            None
        )
        .await
        .0,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    let content = "x".repeat(MAX_DRAFT_BYTES);
    let mut first_version = 0;
    for index in 0..10 {
        let (status, saved) = request(
            &app,
            &f.actor,
            f.proposal,
            Some(&input(&format!("DOC-{index}"), 0, &content)),
            true,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{saved}");
        if index == 0 {
            first_version = saved["buffer"]["version"].as_i64().unwrap();
        }
    }
    let (status, error) = request(
        &app,
        &f.actor,
        f.proposal,
        Some(&input("DOC-EXCESS", 0, "one byte too many")),
        true,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(error["error"]["code"], "DRAFT_QUOTA_EXCEEDED");
    assert_eq!(
        request(
            &app,
            &f.actor,
            f.proposal,
            Some(&input("DOC-0", first_version, "reduced")),
            true,
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &app,
            &f.actor,
            f.proposal,
            Some(&input("DOC-EXCESS", 0, "space now available")),
            true,
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM git_operations")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn recovery_document_count_is_user_scoped_and_expired_buffers_release_capacity(pool: PgPool) {
    let f = fixture(&pool).await;
    let other = actor(&pool, f.organization, "ADMIN").await;
    sqlx::query("INSERT INTO draft_buffers(organization_id,proposal_id,user_id,document_id,base_head_sha,content,expires_at) SELECT $1,$2,$3,'DOC-'||n,$4,'small unfinished source',now()+interval '30 days' FROM generate_series(1,1000) n")
        .bind(f.organization).bind(f.proposal).bind(f.actor.id).bind(HEAD).execute(&pool).await.unwrap();
    let app = app(&pool, Some(30));
    let body = input("DOC-EXCESS", 0, "new document");
    assert_eq!(
        request(&app, &f.actor, f.proposal, Some(&body), true, None)
            .await
            .0,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert_eq!(
        request(&app, &other, f.proposal, Some(&body), true, None)
            .await
            .0,
        StatusCode::OK
    );
    sqlx::query("UPDATE draft_buffers SET expires_at=now()-interval '1 second' WHERE proposal_id=$1 AND user_id=$2 AND document_id='DOC-1'")
        .bind(f.proposal).bind(f.actor.id).execute(&pool).await.unwrap();
    assert_eq!(
        request(&app, &f.actor, f.proposal, Some(&body), true, None)
            .await
            .0,
        StatusCode::OK
    );
    let (_, list) = request(&app, &f.actor, f.proposal, None, false, None).await;
    assert_eq!(list["buffers"].as_array().unwrap().len(), 1000);
    assert!(
        list["buffers"]
            .as_array()
            .unwrap()
            .iter()
            .all(|buffer| buffer["content_utf8"].is_null())
    );
}

async fn discard(
    app: &Router,
    actor: &Actor,
    proposal: Uuid,
    version: i64,
    csrf: bool,
) -> StatusCode {
    let mut builder=Request::builder().method("DELETE")
        .uri(format!("/api/v1/proposals/{proposal}/draft?document_id=DOC-MEDIA&expected_draft_version={version}"))
        .header("Cookie",format!("specspace-session={}",actor.token));
    if csrf {
        builder = builder
            .header("Origin", "http://localhost:4200")
            .header("X-XSRF-TOKEN", &actor.csrf);
    }
    app.clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap()
        .status()
}

#[sqlx::test(migrations = "../../migrations")]
async fn recovery_discard_is_private_versioned_and_retry_safe_after_recreation(pool: PgPool) {
    let f = fixture(&pool).await;
    let other = actor(&pool, f.organization, "ADMIN").await;
    let app = app(&pool, Some(30));
    let body = input("DOC-MEDIA", 0, "source to discard explicitly");
    let (_, saved) = request(&app, &f.actor, f.proposal, Some(&body), true, None).await;
    let version = saved["buffer"]["version"].as_i64().unwrap();
    assert_eq!(
        discard(&app, &other, f.proposal, version, true).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        request(&app, &f.actor, f.proposal, None, false, Some("DOC-MEDIA"))
            .await
            .1["buffers"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        discard(&app, &f.actor, f.proposal, version, false).await,
        StatusCode::FORBIDDEN
    );
    sqlx::query("UPDATE memberships SET role='READER' WHERE organization_id=$1 AND user_id=$2")
        .bind(f.organization)
        .bind(f.actor.id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        discard(&app, &f.actor, f.proposal, version, true).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        discard(&app, &f.actor, f.proposal, version, true).await,
        StatusCode::NO_CONTENT
    );
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE action='DRAFT_RECOVERY_DISCARDED'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 1);
    sqlx::query(
        "UPDATE memberships SET role='CONTRIBUTOR' WHERE organization_id=$1 AND user_id=$2",
    )
    .bind(f.organization)
    .bind(f.actor.id)
    .execute(&pool)
    .await
    .unwrap();
    let (_, recreated) = request(
        &app,
        &f.actor,
        f.proposal,
        Some(&input("DOC-MEDIA", 0, "new unsaved source")),
        true,
        None,
    )
    .await;
    assert!(recreated["buffer"]["version"].as_i64().unwrap() > version);
    assert_eq!(
        discard(&app, &f.actor, f.proposal, version, true).await,
        StatusCode::CONFLICT
    );
    assert_eq!(
        request(&app, &f.actor, f.proposal, None, false, Some("DOC-MEDIA"))
            .await
            .1["buffers"][0]["content_utf8"],
        "new unsaved source"
    );
}
