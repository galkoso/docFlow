use super::*;

fn general(version: i64) -> Value {
    json!({"request_id":Uuid::new_v4(),"origin":{"kind":"proposal","expected_proposal_version":version},"blocking":true,"body":"Exact original message\r\nשלום 🦀"})
}
fn action(version: i64, mutation: Value) -> Value {
    json!({"request_id":Uuid::new_v4(),"expected_thread_version":version,"mutation":mutation})
}
async fn write(
    f: &Fixture,
    user: &User,
    path: &str,
    body: Value,
) -> (StatusCode, Value, Option<String>) {
    let response = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("Content-Type", "application/json")
                .header("Cookie", format!("specspace-session={}", user.token))
                .header("X-Expected-Actor", user.id.to_string())
                .header("Origin", "http://localhost:4200")
                .header("X-XSRF-TOKEN", &user.csrf)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.headers()["cache-control"], "no-store");
    let status = response.status();
    let retry = response
        .headers()
        .get("Retry-After")
        .map(|v| v.to_str().unwrap().to_owned());
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap(), retry)
}
fn list(f: &Fixture) -> String {
    format!("/api/v1/proposals/{}/threads", f.proposal)
}
async fn anchored(pool: &PgPool) -> (Fixture, User, String, Value) {
    let (f, product, _) = configured(pool, false).await;
    let (result, snapshot) = publish(&f, 2).await;
    let files = get(
        &f.app,
        &f.actor,
        &format!("{}/{snapshot}/files", history_path(&f)),
        true,
    )
    .await;
    let file = files.1["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["kind"] == "document")
        .unwrap();
    let body = json!({"request_id":Uuid::new_v4(),"origin":{"kind":"snapshot","snapshot_id":snapshot,"expected_fingerprint":result["review"]["fingerprint"],"anchor":{"kind":"file","file_id":file["id"],"side":"after","selection":{"kind":"lines","start_line":1,"end_line":1}}},"blocking":true,"body":"Discuss the saved after side"});
    (f, product, snapshot, body)
}

#[sqlx::test(migrations = "../../migrations")]
async fn book_document_discussion_http_binds_intent_and_returns_original_source_without_git_writes(
    pool: PgPool,
) {
    let (f, product, _) = configured(&pool, false).await;
    let (result, snapshot) = publish(&f, 2).await;
    let snapshot_id = Uuid::parse_str(&snapshot).unwrap();
    let entries = specspace_infrastructure::review_snapshots::PgReviewSnapshots::new(pool.clone())
        .book_documents(f.actor.id, f.proposal, snapshot_id)
        .await
        .unwrap()
        .unwrap();
    let body = json!({"request_id":Uuid::new_v4(),"origin":{"kind":"snapshot","snapshot_id":snapshot,"expected_fingerprint":result["review"]["fingerprint"],
        "anchor":{"kind":"book_document","entry_id":entries[0].id,"selection":{"kind":"document"}}},"blocking":true,"body":"Discuss the sealed book document"});
    let path = list(&f);
    let reads = f.provider.reads.load(Ordering::SeqCst);
    assert_eq!(
        write(&f, &f.actor, &path, body.clone()).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(f.provider.reads.load(Ordering::SeqCst), reads);
    let created = write(&f, &product, &path, body.clone()).await;
    assert_eq!(created.0, StatusCode::CREATED, "{}", created.1);
    let reads = f.provider.reads.load(Ordering::SeqCst);
    let replay = write(&f, &product, &path, body.clone()).await;
    assert_eq!(replay.0, StatusCode::OK);
    assert_eq!(replay.1["id"], created.1["id"]);
    assert_eq!(f.provider.reads.load(Ordering::SeqCst), reads);
    let detail = get(
        &f.app,
        &f.actor,
        &format!("{path}/{}", created.1["thread_id"].as_str().unwrap()),
        true,
    )
    .await;
    assert_eq!(detail.0, StatusCode::OK);
    let anchor = &detail.1["thread"]["origin"]["anchor"];
    assert_eq!(anchor["kind"], "book_document");
    assert_eq!(anchor["entry_id"], entries[0].id);
    assert_eq!(
        anchor["source"]["sha256"],
        entries[0].source.sha256.as_deref().unwrap()
    );
    assert!(
        anchor["text"]["quote"]
            .as_str()
            .unwrap()
            .contains("document_id:")
    );
    assert_eq!(f.provider.reads.load(Ordering::SeqCst), reads);
    let mut forged = body.clone();
    forged["request_id"] = json!(Uuid::new_v4());
    forged["origin"]["anchor"]["path"] = json!("docs/private.md");
    assert_eq!(
        write(&f, &product, &path, forged).await.0,
        StatusCode::BAD_REQUEST
    );
    let mut mismatched = body;
    mismatched["body"] = json!("Another intent");
    assert_eq!(
        write(&f, &product, &path, mismatched).await.0,
        StatusCode::CONFLICT
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn discussion_writes_thread_messages_state_and_exact_receipts_without_git_changes(
    pool: PgPool,
) {
    let (f, product, _) = configured(&pool, false).await;
    let path = list(&f);
    let body = general(2);
    let created = write(&f, &f.owner, &path, body.clone()).await;
    assert_eq!(created.0, StatusCode::CREATED, "{}", created.1);
    assert_eq!(created.1["actor_id"], f.owner.id.to_string());
    let replay = write(&f, &f.owner, &path, body.clone()).await;
    assert_eq!(replay.0, StatusCode::OK);
    assert_eq!(replay.1["id"], created.1["id"]);
    assert_eq!(replay.1["replayed"], true);
    let mut changed = body;
    changed["body"] = json!("Changed intent");
    assert_eq!(
        write(&f, &f.owner, &path, changed).await.0,
        StatusCode::CONFLICT
    );
    let detail = format!("{path}/{}", created.1["thread_id"].as_str().unwrap());
    let actions = format!("{detail}/actions");
    let reply = action(
        1,
        json!({"kind":"reply","body":"Reply without an editor assignment","reply_to_id":created.1["comment_id"]}),
    );
    let replied = write(&f, &product, &actions, reply.clone()).await;
    assert_eq!(replied.0, StatusCode::CREATED);
    assert_eq!(replied.1["thread_version"], 2);
    assert_eq!(
        write(&f, &product, &actions, reply).await.1["id"],
        replied.1["id"]
    );
    let edited=write(&f,&f.owner,&actions,action(2,json!({"kind":"edit","comment_id":created.1["comment_id"],"expected_comment_version":1,"body":"Edited original message"}))).await;
    assert_eq!(edited.0, StatusCode::CREATED);
    assert_eq!(edited.1["comment_version"], 2);
    let forbidden=write(&f,&f.owner,&actions,action(3,json!({"kind":"tombstone","comment_id":replied.1["comment_id"],"expected_comment_version":1}))).await;
    assert_eq!(forbidden.0, StatusCode::FORBIDDEN);
    assert_eq!(write(&f,&product,&actions,action(3,json!({"kind":"tombstone","comment_id":replied.1["comment_id"],"expected_comment_version":1}))).await.0,StatusCode::CREATED);
    assert_eq!(
        write(&f, &product, &actions, action(4, json!({"kind":"resolve"})))
            .await
            .0,
        StatusCode::CREATED
    );
    assert_eq!(
        write(&f, &f.owner, &actions, action(5, json!({"kind":"reopen"})))
            .await
            .0,
        StatusCode::CREATED
    );
    assert_eq!(
        write(
            &f,
            &f.owner,
            &actions,
            action(6, json!({"kind":"set_blocking","blocking":false}))
        )
        .await
        .0,
        StatusCode::CREATED
    );
    let current = get(&f.app, &f.actor, &detail, true).await;
    assert_eq!(current.1["thread"]["version"], 7);
    assert_eq!(current.1["comments"][0]["body"], "Edited original message");
    assert!(current.1["comments"][1]["body"].is_null());
    assert_eq!(current.1["thread"]["blocking"], false);
    assert_eq!(
        write(&f, &f.owner, &actions, action(1, json!({"kind":"resolve"})))
            .await
            .0,
        StatusCode::CONFLICT
    );
    let version: i64 = sqlx::query_scalar("SELECT version FROM proposals WHERE id=$1")
        .bind(f.proposal)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(version, 2);
    assert_eq!(f.provider.reads.load(Ordering::SeqCst), 0);
    assert_eq!(f.provider.ref_reads.load(Ordering::SeqCst), 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn discussion_write_identity_csrf_role_and_payload_checks_precede_source_io(pool: PgPool) {
    let (f, _, _) = configured(&pool, false).await;
    let path = list(&f);
    for mode in 0..10 {
        let mut who = Some(&f.owner);
        let mut options = PostOptions::new(&f.owner, "unused");
        let mut uri = path.clone();
        let mut body = general(2);
        let expected = match mode {
            0 => {
                who = None;
                StatusCode::UNAUTHORIZED
            }
            1 => {
                options.expected = None;
                StatusCode::CONFLICT
            }
            2 => {
                options.origin = None;
                StatusCode::FORBIDDEN
            }
            3 => {
                options.csrf = None;
                StatusCode::FORBIDDEN
            }
            4 => {
                who = Some(&f.actor);
                options = PostOptions::new(&f.actor, "unused");
                StatusCode::FORBIDDEN
            }
            5 => {
                uri.push_str("?unexpected=1");
                StatusCode::BAD_REQUEST
            }
            6 => {
                body["actor_id"] = json!(f.owner.id);
                StatusCode::BAD_REQUEST
            }
            7 => {
                body["body"] = json!("🦀".repeat(10001));
                StatusCode::BAD_REQUEST
            }
            8 => {
                body["body"] = json!("x".repeat(129 * 1024));
                StatusCode::PAYLOAD_TOO_LARGE
            }
            _ => {
                body["request_id"] = json!(Uuid::nil());
                StatusCode::BAD_REQUEST
            }
        };
        assert_eq!(
            post_raw(&f.app, who, &uri, body.to_string(), options)
                .await
                .0,
            expected,
            "mode {mode}"
        );
    }
    let attempts: i64 = sqlx::query_scalar("SELECT count(*) FROM discussion_attempts")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(attempts, 0);
    assert_eq!(f.provider.reads.load(Ordering::SeqCst), 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn anchored_http_creation_binds_source_and_independent_blockers_and_replay_avoids_backoff(
    pool: PgPool,
) {
    let (f, product, snapshot, body) = anchored(&pool).await;
    let repository: Uuid = sqlx::query_scalar("SELECT repository_id FROM proposals WHERE id=$1")
        .bind(f.proposal)
        .fetch_one(&pool)
        .await
        .unwrap();
    let path = list(&f);
    let created = write(&f, &product, &path, body.clone()).await;
    assert_eq!(created.0, StatusCode::CREATED, "{}", created.1);
    let detail = format!("{path}/{}", created.1["thread_id"].as_str().unwrap());
    let actions = format!("{detail}/actions");
    let page = get(&f.app, &f.actor, &detail, true).await;
    assert_eq!(page.1["thread"]["origin"]["snapshot_id"], snapshot);
    assert_eq!(page.1["thread"]["origin"]["head_sha"], HEAD);
    assert_eq!(
        page.1["thread"]["origin"]["anchor"]["text"]["quote"],
        "---\n"
    );
    assert_eq!(page.1["thread"]["independent_requirement"], true);
    for mutation in [
        json!({"kind":"resolve"}),
        json!({"kind":"set_blocking","blocking":false}),
    ] {
        let rejected = write(&f, &f.owner, &actions, action(1, mutation)).await;
        assert_eq!(rejected.0, StatusCode::FORBIDDEN);
        assert_eq!(rejected.1["error"]["code"], "INDEPENDENT_REVIEWER_REQUIRED");
    }
    sqlx::query("INSERT INTO official_review_backoff(organization_id,repository_id,remote_id,installation_id,until_at) VALUES($1,$2,'42','7',clock_timestamp()+interval '60 seconds')").bind(f.organization).bind(repository).execute(&pool).await.unwrap();
    let reads = f.provider.reads.load(Ordering::SeqCst);
    let replay = write(&f, &product, &path, body).await;
    assert_eq!(replay.0, StatusCode::OK);
    assert_eq!(replay.1["id"], created.1["id"]);
    assert_eq!(f.provider.reads.load(Ordering::SeqCst), reads);
    assert_eq!(write(&f,&product,&actions,action(1,json!({"kind":"reply","body":"Still available during source cooldown","reply_to_id":null}))).await.0,StatusCode::CREATED);
}

#[sqlx::test(migrations = "../../migrations")]
async fn source_retry_after_is_durable_shared_with_official_reviews_and_never_retargets_intent(
    pool: PgPool,
) {
    let (f, product, snapshot, body) = anchored(&pool).await;
    let repository: Uuid = sqlx::query_scalar("SELECT repository_id FROM proposals WHERE id=$1")
        .bind(f.proposal)
        .fetch_one(&pool)
        .await
        .unwrap();
    let path = list(&f);
    f.provider.backoff.store(2, Ordering::SeqCst);
    let first = write(&f, &f.owner, &path, body.clone()).await;
    assert_eq!(first.0, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(first.2.as_deref(), Some("13"));
    let until: sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc> =
        sqlx::query_scalar("SELECT until_at FROM official_review_backoff WHERE repository_id=$1")
            .bind(repository)
            .fetch_one(&pool)
            .await
            .unwrap();
    let reads = f.provider.reads.load(Ordering::SeqCst);
    let mut another = body.clone();
    another["request_id"] = json!(Uuid::new_v4());
    let stopped = write(&f, &product, &path, another).await;
    assert_eq!(stopped.0, StatusCode::TOO_MANY_REQUESTS);
    assert!(stopped.2.unwrap().parse::<u32>().unwrap() <= 13);
    assert_eq!(f.provider.reads.load(Ordering::SeqCst), reads);
    let mut changed = body.clone();
    changed["body"] = json!("Different body under the same nonce");
    assert_eq!(
        write(&f, &f.owner, &path, changed).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        write(&f, &f.owner, &path, general(3)).await.0,
        StatusCode::CREATED
    );
    let official = json!({"request_id":Uuid::new_v4(),"expected_fingerprint":body["origin"]["expected_fingerprint"],"domain":"PRODUCT","decision":"COMMENT","explanation":"Still uses the same provider delay"});
    let refs = f.provider.ref_reads.load(Ordering::SeqCst);
    let blocked = write(
        &f,
        &product,
        &format!("{}/{snapshot}/decisions", history_path(&f)),
        official,
    )
    .await;
    assert_eq!(blocked.0, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(f.provider.ref_reads.load(Ordering::SeqCst), refs);
    let preserved: sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc> =
        sqlx::query_scalar("SELECT until_at FROM official_review_backoff WHERE repository_id=$1")
            .bind(repository)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(until, preserved);
    let results:Vec<String>=sqlx::query_scalar("SELECT result FROM audit_events WHERE action='DISCUSSION_SOURCE_STOPPED' ORDER BY timestamp,id").fetch_all(&pool).await.unwrap();
    assert!(
        results.contains(&"PROVIDER_RATE_LIMIT".into())
            && results.contains(&"PROVIDER_BACKOFF".into())
    );
    sqlx::query("UPDATE official_review_backoff SET until_at=clock_timestamp()-interval '1 second' WHERE repository_id=$1").bind(repository).execute(&pool).await.unwrap();
    f.provider.backoff.store(0, Ordering::SeqCst);
    assert_eq!(
        write(&f, &f.owner, &path, body).await.0,
        StatusCode::CREATED
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn invalid_source_and_mid_read_revocation_never_commit_a_thread_or_expose_source(
    pool: PgPool,
) {
    let (f, product, _, body) = anchored(&pool).await;
    let path = list(&f);
    f.provider.backoff.store(3, Ordering::SeqCst);
    let forged = write(&f, &f.owner, &path, body.clone()).await;
    assert_eq!(forged.0, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(forged.1["error"]["code"], "DISCUSSION_SOURCE_INVALID");
    f.provider.backoff.store(0, Ordering::SeqCst);
    *f.provider.hook.lock().unwrap() = Some((pool.clone(), product.id));
    let mut second = body;
    second["request_id"] = json!(Uuid::new_v4());
    let revoked = write(&f, &product, &path, second).await;
    assert_eq!(revoked.0, StatusCode::NOT_FOUND);
    assert!(revoked.1.get("thread").is_none());
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM discussion_threads")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    let results: Vec<String> = sqlx::query_scalar(
        "SELECT result FROM audit_events WHERE action='DISCUSSION_SOURCE_STOPPED'",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(
        results.contains(&"INVALID_EVIDENCE".into()) && results.contains(&"ACCESS_LOST".into())
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn provider_cooldown_and_failure_audit_commit_atomically_before_retry_after_is_returned(
    pool: PgPool,
) {
    let (f, _, _, body) = anchored(&pool).await;
    let path = list(&f);
    sqlx::query("CREATE FUNCTION reject_source_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='DISCUSSION_SOURCE_STOPPED' THEN RAISE EXCEPTION 'synthetic audit failure'; END IF; RETURN NEW; END $$").execute(&pool).await.unwrap();
    sqlx::query("CREATE TRIGGER reject_source_audit BEFORE INSERT ON audit_events FOR EACH ROW EXECUTE FUNCTION reject_source_audit()").execute(&pool).await.unwrap();
    f.provider.backoff.store(2, Ordering::SeqCst);
    assert_eq!(
        write(&f, &f.owner, &path, body.clone()).await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM official_review_backoff")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    sqlx::query("DROP TRIGGER reject_source_audit ON audit_events")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        write(&f, &f.owner, &path, body).await.0,
        StatusCode::TOO_MANY_REQUESTS
    );
    let counts:(i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM discussion_attempts),(SELECT count(*) FROM audit_events WHERE action='DISCUSSION_SOURCE_STOPPED'),(SELECT count(*) FROM discussion_threads)").fetch_one(&pool).await.unwrap();
    assert_eq!(counts, (2, 1, 0));
}

#[sqlx::test(migrations = "../../migrations")]
async fn merging_proposals_allow_comments_and_replays_but_reject_new_blocking_requirements(
    pool: PgPool,
) {
    let (f, _, _) = configured(&pool, false).await;
    let path = list(&f);
    let initial = general(2);
    let first = write(&f, &f.owner, &path, initial.clone()).await;
    let first_path = format!("{path}/{}", first.1["thread_id"].as_str().unwrap());
    let first_actions = format!("{first_path}/actions");
    assert_eq!(
        write(
            &f,
            &f.owner,
            &first_actions,
            action(1, json!({"kind":"resolve"}))
        )
        .await
        .0,
        StatusCode::CREATED
    );
    let mut nonblocking = general(2);
    nonblocking["blocking"] = json!(false);
    let second = write(&f, &f.owner, &path, nonblocking.clone()).await;
    let second_path = format!("{path}/{}", second.1["thread_id"].as_str().unwrap());
    sqlx::query("UPDATE proposals SET state='MERGING' WHERE id=$1")
        .bind(f.proposal)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(write(&f, &f.owner, &path, initial).await.0, StatusCode::OK);
    assert_eq!(
        write(&f, &f.owner, &path, general(2)).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        write(
            &f,
            &f.owner,
            &first_actions,
            action(2, json!({"kind":"reopen"}))
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        write(
            &f,
            &f.owner,
            &format!("{second_path}/actions"),
            action(1, json!({"kind":"set_blocking","blocking":true}))
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    nonblocking["request_id"] = json!(Uuid::new_v4());
    assert_eq!(
        write(&f, &f.owner, &path, nonblocking).await.0,
        StatusCode::CREATED
    );
    assert_eq!(write(&f,&f.owner,&first_actions,action(2,json!({"kind":"reply","body":"The merge does not prevent discussion","reply_to_id":null}))).await.0,StatusCode::CREATED);
    let page = get(&f.app, &f.owner, &path, true).await;
    assert_eq!(page.1["can_comment"], true);
    assert_eq!(page.1["can_open_blocking_thread"], false);
    assert_eq!(
        get(&f.app, &f.owner, &first_path, true).await.1["thread"]["permissions"]["can_reopen"],
        false
    );
    assert_eq!(
        get(&f.app, &f.owner, &second_path, true).await.1["thread"]["permissions"]["can_toggle_blocking"],
        false
    );
    assert_eq!(f.provider.reads.load(Ordering::SeqCst), 0);
}
