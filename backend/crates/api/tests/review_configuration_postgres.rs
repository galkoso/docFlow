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
    org: Uuid,
    proposal: Uuid,
    owner: Actor,
    product: Actor,
    development: Actor,
    app: Router,
}
async fn actor(pool: &PgPool, org: Uuid, role: &str, domains: Vec<&str>) -> Actor {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO users(id,issuer,external_subject,display_name) VALUES($1,'fixture',$1::text,'Synthetic reviewer')").bind(id).execute(pool).await.unwrap();
    sqlx::query(
        "INSERT INTO memberships(organization_id,user_id,role,review_domains) VALUES($1,$2,$3,$4)",
    )
    .bind(org)
    .bind(id)
    .bind(role)
    .bind(domains)
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
    let org = Uuid::new_v4();
    let repository = Uuid::new_v4();
    let team = Uuid::new_v4();
    let proposal = Uuid::new_v4();
    sqlx::query("INSERT INTO organizations(id,name) VALUES($1,'Synthetic review organization')")
        .bind(org)
        .execute(pool)
        .await
        .unwrap();
    let owner = actor(pool, org, "CONTRIBUTOR", vec![]).await;
    let product = actor(pool, org, "REVIEWER", vec!["PRODUCT"]).await;
    let development = actor(pool, org, "REVIEWER", vec!["DEVELOPMENT"]).await;
    sqlx::query("INSERT INTO teams(id,organization_id,name) VALUES($1,$2,'Synthetic review team')")
        .bind(team)
        .bind(org)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO repositories(id,organization_id,provider,remote_id,installation_id) VALUES($1,$2,'GITHUB','42','7')").bind(repository).bind(org).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO proposals(id,organization_id,repository_id,title,description,branch,owner_id,author_id,team_id,state,ui_impact,base_sha,head_sha,target_sha) VALUES($1,$2,$3,'Review configuration','Synthetic review configuration proposal','change/review',$4,$4,$5,'DRAFT','UNDECIDED',$6,$6,$6)")
        .bind(proposal).bind(org).bind(repository).bind(owner.id).bind(team).bind(HEAD).execute(pool).await.unwrap();
    let mut state = AppState::new(pool.clone());
    state.cookies = CookiePolicy::new("http://localhost:4200", true).unwrap();
    Fixture {
        org,
        proposal,
        owner,
        product,
        development,
        app: router(state),
    }
}
fn path(f: &Fixture) -> String {
    format!("/api/v1/proposals/{}/review/configuration", f.proposal)
}
fn settings(f: &Fixture) -> Value {
    json!({"ui_impact":"no","ui_impact_reason":"No UI change","assignments":[
    {"reviewer_id":f.product.id,"domain":"PRODUCT","required":true},{"reviewer_id":f.development.id,"domain":"DEVELOPMENT","required":true}]})
}
fn input(f: &Fixture) -> Value {
    json!({"request_id":Uuid::new_v4(),"expected_head_sha":HEAD,"expected_proposal_version":1,"expected_configuration_version":1,"settings":settings(f)})
}
async fn request(
    app: &Router,
    user: &Actor,
    uri: &str,
    body: Option<&Value>,
    bound: bool,
    csrf: bool,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .uri(uri)
        .method(if body.is_some() { "PUT" } else { "GET" })
        .header("Cookie", format!("specspace-session={}", user.token));
    if bound {
        builder = builder.header("X-Expected-Actor", user.id.to_string());
    }
    if csrf {
        builder = builder
            .header("Origin", "http://localhost:4200")
            .header("X-XSRF-TOKEN", &user.csrf);
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
async fn configuration_saves_immutable_revisions_audit_and_outbox_without_submitting_a_review(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    let (status, initial) = request(&f.app, &f.owner, &path(&f), None, true, false).await;
    assert_eq!(status, StatusCode::OK, "{initial}");
    assert_eq!(initial["configuration_version"], 1);
    assert_eq!(initial["can_edit"], true);
    let (status, saved) = request(&f.app, &f.owner, &path(&f), Some(&input(&f)), true, true).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["current"]["configuration_version"], 2);
    assert_eq!(saved["current"]["proposal_version"], 2);
    assert_eq!(saved["current"]["state"], "DRAFT");
    assert_eq!(saved["changed"], true);
    assert_eq!(saved["replayed"], false);
    assert!(
        saved["current"]["reviewers"]
            .as_array()
            .unwrap()
            .iter()
            .all(|reviewer| reviewer["active"] == true && reviewer["qualified"] == true)
    );
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM proposal_review_configurations WHERE proposal_id=$1",
    )
    .bind(f.proposal)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 2);
    let event:Value=sqlx::query_scalar("SELECT payload FROM outbox WHERE entity_id=$1 AND event_type='PROPOSAL_REVIEW_CONFIGURATION_CHANGED'").bind(f.proposal).fetch_one(&pool).await.unwrap();
    assert_eq!(event["invalidate_review"], true);
    let reader = actor(&pool, f.org, "READER", vec![]).await;
    let (_, visible) = request(&f.app, &reader, &path(&f), None, true, false).await;
    assert_eq!(visible["can_edit"], false);
    assert_eq!(
        visible["settings"]["assignments"].as_array().unwrap().len(),
        2
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn exact_replays_survive_later_versions_and_changed_keys_never_replace_an_intent(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    let body = input(&f);
    let (_, saved) = request(&f.app, &f.owner, &path(&f), Some(&body), true, true).await;
    assert_eq!(saved["current"]["configuration_version"], 2, "{saved}");
    let (status, replayed) = request(&f.app, &f.owner, &path(&f), Some(&body), true, true).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replayed["replayed"], true);
    assert_eq!(replayed["applied_configuration_version"], 2);
    let mut conflict = body.clone();
    conflict["settings"]["ui_impact_reason"] = "Different reason".into();
    let (status, response) =
        request(&f.app, &f.owner, &path(&f), Some(&conflict), true, true).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(response["error"]["code"], "IDEMPOTENCY_KEY_REUSED");
    let mut next = input(&f);
    next["expected_proposal_version"] = 2.into();
    next["expected_configuration_version"] = 2.into();
    next["settings"]["ui_impact"] = "yes".into();
    assert_eq!(
        request(&f.app, &f.owner, &path(&f), Some(&next), true, true)
            .await
            .0,
        StatusCode::OK
    );
    let (_, replayed) = request(&f.app, &f.owner, &path(&f), Some(&body), true, true).await;
    assert_eq!(replayed["applied_configuration_version"], 2);
    assert_eq!(replayed["current"]["configuration_version"], 3);
    let count:i64=sqlx::query_scalar("SELECT count(*) FROM outbox WHERE entity_id=$1 AND event_type='PROPOSAL_REVIEW_CONFIGURATION_CHANGED'").bind(f.proposal).fetch_one(&pool).await.unwrap();
    assert_eq!(count, 2);
}

#[sqlx::test(migrations = "../../migrations")]
async fn no_op_preserves_approval_but_a_changed_configuration_invalidates_it(pool: PgPool) {
    let f = fixture(&pool).await;
    sqlx::query("UPDATE proposals SET state='APPROVED' WHERE id=$1")
        .bind(f.proposal)
        .execute(&pool)
        .await
        .unwrap();
    let mut body = input(&f);
    body["settings"] = json!({"ui_impact":"undecided","ui_impact_reason":null,"assignments":[]});
    let (status, saved) = request(&f.app, &f.owner, &path(&f), Some(&body), true, true).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["changed"], false);
    assert_eq!(saved["current"]["state"], "APPROVED");
    assert_eq!(saved["applied_proposal_version"], 1);
    let events: i64 = sqlx::query_scalar("SELECT count(*) FROM outbox WHERE entity_id=$1")
        .bind(f.proposal)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(events, 0);
    let (_, changed) = request(&f.app, &f.owner, &path(&f), Some(&input(&f)), true, true).await;
    assert_eq!(changed["current"]["state"], "IN_REVIEW", "{changed}");
    assert_eq!(changed["applied_configuration_version"], 2);
    let (_, replayed) = request(&f.app, &f.owner, &path(&f), Some(&body), true, true).await;
    assert_eq!(replayed["changed"], false);
    assert_eq!(replayed["replayed"], true);
    assert_eq!(replayed["applied_configuration_version"], 1);
    assert_eq!(replayed["current"]["configuration_version"], 2);
}

#[sqlx::test(migrations = "../../migrations")]
async fn concurrent_intents_have_one_winner_and_identical_retries_have_one_receipt(pool: PgPool) {
    let f = fixture(&pool).await;
    let uri = path(&f);
    let first = input(&f);
    let second = input(&f);
    let (a, b) = tokio::join!(
        request(&f.app, &f.owner, &uri, Some(&first), true, true),
        request(&f.app, &f.owner, &uri, Some(&second), true, true)
    );
    assert!(
        (a.0 == StatusCode::OK && b.0 == StatusCode::CONFLICT)
            || (b.0 == StatusCode::OK && a.0 == StatusCode::CONFLICT),
        "{a:?} {b:?}"
    );
    let stale = if a.0 == StatusCode::CONFLICT {
        a.1
    } else {
        b.1
    };
    assert_eq!(stale["error"]["code"], "REVIEW_CONFIGURATION_STALE");
    let mut next = input(&f);
    next["expected_proposal_version"] = 2.into();
    next["expected_configuration_version"] = 2.into();
    next["settings"]["ui_impact_reason"] = "Updated rationale".into();
    let (a, b) = tokio::join!(
        request(&f.app, &f.owner, &uri, Some(&next), true, true),
        request(&f.app, &f.owner, &uri, Some(&next), true, true)
    );
    assert_eq!(a.0, StatusCode::OK, "{a:?}");
    assert_eq!(b.0, StatusCode::OK, "{b:?}");
    assert_ne!(a.1["replayed"], b.1["replayed"]);
    let (revisions, receipts, events): (i64, i64, i64) = sqlx::query_as("SELECT (SELECT count(*) FROM proposal_review_configurations WHERE proposal_id=$1),(SELECT count(*) FROM review_configuration_operations WHERE proposal_id=$1),(SELECT count(*) FROM outbox WHERE entity_id=$1)").bind(f.proposal).fetch_one(&pool).await.unwrap();
    assert_eq!((revisions, receipts, events), (3, 2, 2));
}

#[sqlx::test(migrations = "../../migrations")]
async fn actor_binding_csrf_scope_and_current_edit_permissions_are_enforced(pool: PgPool) {
    let f = fixture(&pool).await;
    let body = input(&f);
    assert_eq!(
        request(&f.app, &f.owner, &path(&f), Some(&body), false, true)
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        request(&f.app, &f.owner, &path(&f), Some(&body), true, false)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let reader = actor(&pool, f.org, "READER", vec![]).await;
    let contributor = actor(&pool, f.org, "CONTRIBUTOR", vec![]).await;
    for user in [&reader, &contributor, &f.product] {
        assert_eq!(
            request(&f.app, user, &path(&f), Some(&body), true, true)
                .await
                .0,
            StatusCode::FORBIDDEN
        );
    }
    sqlx::query("INSERT INTO proposal_collaborators(organization_id,proposal_id,user_id,access) VALUES($1,$2,$3,'READ')").bind(f.org).bind(f.proposal).bind(contributor.id).execute(&pool).await.unwrap();
    assert_eq!(
        request(&f.app, &contributor, &path(&f), Some(&body), true, true)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    sqlx::query(
        "UPDATE proposal_collaborators SET access='EDIT' WHERE proposal_id=$1 AND user_id=$2",
    )
    .bind(f.proposal)
    .bind(contributor.id)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        request(&f.app, &contributor, &path(&f), Some(&body), true, true)
            .await
            .0,
        StatusCode::OK
    );
    sqlx::query("DELETE FROM proposal_collaborators WHERE proposal_id=$1 AND user_id=$2")
        .bind(f.proposal)
        .bind(contributor.id)
        .execute(&pool)
        .await
        .unwrap();
    // A receipt does not restore a revoked actor's authority.
    assert_eq!(
        request(&f.app, &contributor, &path(&f), Some(&body), true, true)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let foreign = Uuid::new_v4();
    sqlx::query("INSERT INTO organizations(id,name) VALUES($1,'Foreign organization')")
        .bind(foreign)
        .execute(&pool)
        .await
        .unwrap();
    let outsider = actor(&pool, foreign, "ADMIN", vec![]).await;
    assert_eq!(
        request(&f.app, &outsider, &path(&f), None, true, false)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(&f.app, &outsider, &path(&f), Some(&body), true, true)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let admin = actor(&pool, f.org, "ADMIN", vec![]).await;
    let mut next = input(&f);
    next["expected_proposal_version"] = 2.into();
    next["expected_configuration_version"] = 2.into();
    next["settings"]["ui_impact_reason"] = "Administrator revision".into();
    assert_eq!(
        request(&f.app, &admin, &path(&f), Some(&next), true, true)
            .await
            .0,
        StatusCode::OK
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn reviewer_revocation_remains_visible_without_erasing_history(pool: PgPool) {
    let f = fixture(&pool).await;
    let body = input(&f);
    assert_eq!(
        request(&f.app, &f.owner, &path(&f), Some(&body), true, true)
            .await
            .0,
        StatusCode::OK
    );
    sqlx::query("DELETE FROM memberships WHERE organization_id=$1 AND user_id=$2")
        .bind(f.org)
        .bind(f.product.id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE users SET status='DISABLED' WHERE id=$1")
        .bind(f.development.id)
        .execute(&pool)
        .await
        .unwrap();
    let (_, read) = request(&f.app, &f.owner, &path(&f), None, true, false).await;
    assert_eq!(read["reviewers"].as_array().unwrap().len(), 2);
    assert!(
        read["reviewers"]
            .as_array()
            .unwrap()
            .iter()
            .all(|reviewer| reviewer["active"] == false && reviewer["qualified"] == false)
    );
    // Existing receipts remain observable; a new intent must use eligible reviewers.
    assert_eq!(
        request(&f.app, &f.owner, &path(&f), Some(&body), true, true)
            .await
            .0,
        StatusCode::OK
    );
    let mut next = input(&f);
    next["expected_proposal_version"] = 2.into();
    next["expected_configuration_version"] = 2.into();
    let (status, result) = request(&f.app, &f.owner, &path(&f), Some(&next), true, true).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(result["error"]["code"], "REVIEWER_NOT_ELIGIBLE");
    next["settings"]["assignments"] = json!([]);
    assert_eq!(
        request(&f.app, &f.owner, &path(&f), Some(&next), true, true)
            .await
            .0,
        StatusCode::OK
    );
    let old_count: i64 = sqlx::query_scalar("SELECT count(*) FROM proposal_review_assignments WHERE proposal_id=$1 AND configuration_version=2").bind(f.proposal).fetch_one(&pool).await.unwrap();
    assert_eq!(old_count, 2);
}

#[sqlx::test(migrations = "../../migrations")]
async fn pending_git_mutations_and_final_state_block_new_intents_but_allow_observation(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    let body = input(&f);
    assert_eq!(
        request(&f.app, &f.owner, &path(&f), Some(&body), true, true)
            .await
            .0,
        StatusCode::OK
    );
    let mut next = input(&f);
    next["expected_proposal_version"] = 2.into();
    next["expected_configuration_version"] = 2.into();
    sqlx::query("INSERT INTO git_operations(id,organization_id,proposal_id,actor_id,kind,idempotency_key,request_hash,state) VALUES($1,$2,$3,$4,'COMMIT','synthetic-save','synthetic','UNKNOWN')").bind(Uuid::new_v4()).bind(f.org).bind(f.proposal).bind(f.owner.id).execute(&pool).await.unwrap();
    let (status, blocked) = request(&f.app, &f.owner, &path(&f), Some(&next), true, true).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(blocked["error"]["code"], "PROPOSAL_BUSY");
    let (_, replay) = request(&f.app, &f.owner, &path(&f), Some(&body), true, true).await;
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["current"]["can_edit"], false);
    sqlx::query("UPDATE git_operations SET state='SUCCEEDED' WHERE proposal_id=$1")
        .bind(f.proposal)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE proposals SET state='CLOSED',version=version+1 WHERE id=$1")
        .bind(f.proposal)
        .execute(&pool)
        .await
        .unwrap();
    let (status, blocked) = request(&f.app, &f.owner, &path(&f), Some(&next), true, true).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(blocked["error"]["code"], "PROPOSAL_NOT_EDITABLE");
    assert_eq!(
        request(&f.app, &f.owner, &path(&f), Some(&body), true, true)
            .await
            .0,
        StatusCode::OK
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn history_cannot_be_rewritten_appended_late_or_published_incompletely(pool: PgPool) {
    let f = fixture(&pool).await;
    assert_eq!(
        request(&f.app, &f.owner, &path(&f), Some(&input(&f)), true, true)
            .await
            .0,
        StatusCode::OK
    );
    for statement in [
        "UPDATE proposal_review_configurations SET ui_impact='YES' WHERE proposal_id=$1",
        "DELETE FROM proposal_review_configurations WHERE proposal_id=$1",
        "UPDATE proposal_review_assignments SET required=false WHERE proposal_id=$1",
        "DELETE FROM proposal_review_assignments WHERE proposal_id=$1",
        "UPDATE review_configuration_operations SET changed=false WHERE proposal_id=$1",
        "DELETE FROM review_configuration_operations WHERE proposal_id=$1",
        "UPDATE proposals SET ui_impact='YES' WHERE id=$1",
        "UPDATE proposals SET review_configuration_version=1 WHERE id=$1",
    ] {
        assert!(
            sqlx::query(statement)
                .bind(f.proposal)
                .execute(&pool)
                .await
                .is_err(),
            "{statement}"
        );
    }
    for table in [
        "proposal_review_configurations",
        "proposal_review_assignments",
        "review_configuration_operations",
    ] {
        assert!(
            sqlx::query(&format!("TRUNCATE {table} CASCADE"))
                .execute(&pool)
                .await
                .is_err()
        );
    }
    assert!(sqlx::query("INSERT INTO proposal_review_assignments(organization_id,proposal_id,configuration_version,reviewer_id,domain,required) VALUES($1,$2,1,$3,'PRODUCT',true)").bind(f.org).bind(f.proposal).bind(f.product.id).execute(&pool).await.is_err());
    // Header + pointer without the promised relational assignments must roll back.
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("INSERT INTO proposal_review_configurations(organization_id,proposal_id,version,ui_impact,ui_impact_reason,assignments,created_by) VALUES($1,$2,3,'NO','No UI change',$3,$4)").bind(f.org).bind(f.proposal).bind(settings(&f)["assignments"].clone()).bind(f.owner.id).execute(&mut *tx).await.unwrap();
    sqlx::query("UPDATE proposals SET review_configuration_version=3,version=3 WHERE id=$1")
        .bind(f.proposal)
        .execute(&mut *tx)
        .await
        .unwrap();
    assert!(tx.commit().await.is_err());
    // An internally complete revision must also be selected by the proposal.
    assert!(sqlx::query("INSERT INTO proposal_review_configurations(organization_id,proposal_id,version,ui_impact,assignments,created_by) VALUES($1,$2,3,'YES','[]',$3)").bind(f.org).bind(f.proposal).bind(f.owner.id).execute(&pool).await.is_err());
    // Forcing the header check early cannot permit later unlisted assignments.
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("INSERT INTO proposal_review_configurations(organization_id,proposal_id,version,ui_impact,ui_impact_reason,assignments,created_by) VALUES($1,$2,3,'NO','No UI change','[]',$3)").bind(f.org).bind(f.proposal).bind(f.owner.id).execute(&mut *tx).await.unwrap();
    sqlx::query("UPDATE proposals SET review_configuration_version=3,version=3 WHERE id=$1")
        .bind(f.proposal)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("SET CONSTRAINTS ALL IMMEDIATE")
        .execute(&mut *tx)
        .await
        .unwrap();
    assert!(sqlx::query("INSERT INTO proposal_review_assignments(organization_id,proposal_id,configuration_version,reviewer_id,domain,required) VALUES($1,$2,3,$3,'PRODUCT',true)").bind(f.org).bind(f.proposal).bind(f.product.id).execute(&mut *tx).await.is_err());
    tx.rollback().await.unwrap();
    let (_, read) = request(&f.app, &f.owner, &path(&f), None, true, false).await;
    assert_eq!(read["configuration_version"], 2);
}

#[sqlx::test(migrations = "../../migrations")]
async fn malformed_and_ineligible_intents_never_create_history(pool: PgPool) {
    let f = fixture(&pool).await;
    let body = input(&f);
    assert_eq!(
        request(
            &f.app,
            &f.owner,
            &format!("{}?unexpected=true", path(&f)),
            Some(&body),
            true,
            true
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let mut malformed = body.clone();
    malformed["settings"]["unknown"] = true.into();
    assert_eq!(
        request(&f.app, &f.owner, &path(&f), Some(&malformed), true, true)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let mut missing_reason = body.clone();
    missing_reason["settings"]["ui_impact_reason"] = "  ".into();
    assert_eq!(
        request(
            &f.app,
            &f.owner,
            &path(&f),
            Some(&missing_reason),
            true,
            true
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let mut duplicate = body.clone();
    duplicate["settings"]["assignments"] = json!([
        body["settings"]["assignments"][0],
        body["settings"]["assignments"][0]
    ]);
    assert_eq!(
        request(&f.app, &f.owner, &path(&f), Some(&duplicate), true, true)
            .await
            .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    // Even a qualified proposal author cannot fill an independent reviewer slot.
    sqlx::query("UPDATE memberships SET role='REVIEWER',review_domains=ARRAY['PRODUCT'] WHERE organization_id=$1 AND user_id=$2").bind(f.org).bind(f.owner.id).execute(&pool).await.unwrap();
    let mut author = body.clone();
    author["settings"]["assignments"][0]["reviewer_id"] = f.owner.id.to_string().into();
    assert_eq!(
        request(&f.app, &f.owner, &path(&f), Some(&author), true, true)
            .await
            .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let mut stale = body;
    stale["expected_head_sha"] = "2".repeat(40).into();
    assert_eq!(
        request(&f.app, &f.owner, &path(&f), Some(&stale), true, true)
            .await
            .0,
        StatusCode::CONFLICT
    );
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM proposal_review_configurations WHERE proposal_id=$1",
    )
    .bind(f.proposal)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 1);
}
