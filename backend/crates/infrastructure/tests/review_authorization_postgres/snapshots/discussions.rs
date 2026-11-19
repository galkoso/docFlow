use super::*;
#[path = "discussions/service.rs"]
mod service;

struct Revision {
    thread: Uuid,
    version: i64,
    actor: Uuid,
    kind: &'static str,
    blocking: bool,
    resolved: bool,
    independent: bool,
    comment: Option<Uuid>,
    comment_version: Option<i64>,
    comment_author: Option<Uuid>,
    body: Option<String>,
}
impl Revision {
    fn opening(actor: Uuid, independent: bool) -> Self {
        Self {
            thread: Uuid::new_v4(),
            version: 1,
            actor,
            kind: "OPEN",
            blocking: true,
            resolved: false,
            independent,
            comment: Some(Uuid::new_v4()),
            comment_version: Some(1),
            comment_author: Some(actor),
            body: Some("Original discussion text".into()),
        }
    }
    fn state(&self, version: i64, actor: Uuid, kind: &'static str, resolved: bool) -> Self {
        Self {
            thread: self.thread,
            version,
            actor,
            kind,
            blocking: self.blocking,
            resolved,
            independent: self.independent,
            comment: None,
            comment_version: None,
            comment_author: None,
            body: None,
        }
    }
}

async fn write(
    pool: &PgPool,
    f: &Fixture,
    row: &Revision,
    complete: bool,
) -> Result<Uuid, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT id FROM proposals WHERE id=$1 FOR UPDATE")
        .bind(f.proposal)
        .execute(&mut *tx)
        .await?;
    let (id, attempt, request, audit, event) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    sqlx::query("INSERT INTO discussion_attempts(id,organization_id,proposal_id,actor_id,request_id,request_hash) VALUES($1,$2,$3,$4,$5,$6)")
        .bind(attempt).bind(f.org).bind(f.proposal).bind(row.actor).bind(request).bind("a".repeat(64)).execute(&mut *tx).await?;
    if row.version == 1 {
        sqlx::query("INSERT INTO discussion_threads(id,organization_id,proposal_id,creator_id,repository_id,remote_id,installation_id,branch,head_sha,target_sha,proposal_version,anchor,opening_revision) SELECT $1,p.organization_id,p.id,$2,p.repository_id,r.remote_id,r.installation_id,p.branch,p.head_sha,p.target_sha,p.version,'{\"kind\":\"proposal\"}',$3 FROM proposals p JOIN repositories r ON r.id=p.repository_id WHERE p.id=$4")
            .bind(row.thread).bind(row.actor).bind(id).bind(f.proposal).execute(&mut *tx).await?;
    }
    sqlx::query("INSERT INTO discussion_revisions(id,organization_id,proposal_id,thread_id,version,actor_id,request_id,request_hash,attempt_id,kind,blocking,resolved,independent_requirement,comment_id,comment_version,comment_author_id,body,audit_id,event_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19)")
        .bind(id).bind(f.org).bind(f.proposal).bind(row.thread).bind(row.version).bind(row.actor).bind(request).bind("a".repeat(64)).bind(attempt)
        .bind(row.kind).bind(row.blocking).bind(row.resolved).bind(row.independent).bind(row.comment).bind(row.comment_version).bind(row.comment_author).bind(&row.body).bind(audit).bind(event).execute(&mut *tx).await?;
    if complete {
        sqlx::query("INSERT INTO audit_events(id,organization_id,actor_id,action,entity_id,after_ref,result) VALUES($1,$2,$3,$4,$5,$6,'RECORDED')")
            .bind(audit).bind(f.org).bind(row.actor).bind(format!("DISCUSSION_{}",row.kind)).bind(row.thread).bind(id.to_string()).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO outbox(id,organization_id,event_type,entity_id,payload) VALUES($1,$2,'DISCUSSION_CHANGED',$3,$4)")
            .bind(event).bind(f.org).bind(f.proposal).bind(serde_json::json!({"thread_id":row.thread,"revision_id":id,"version":row.version,"kind":row.kind})).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(id)
}

#[sqlx::test(migrations = "../../migrations")]
async fn independent_blockers_survive_tombstones_role_changes_and_reopening(pool: PgPool) {
    let f = fixture(&pool).await;
    let open = Revision::opening(f.product, true);
    write(&pool, &f, &open, true).await.unwrap();
    sqlx::query("UPDATE memberships SET role='ADMIN' WHERE user_id=$1")
        .bind(f.owner)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        write(&pool, &f, &open.state(2, f.owner, "RESOLVE", true), true)
            .await
            .is_err()
    );
    let mut unmark = open.state(2, f.owner, "SET_BLOCKING", false);
    unmark.blocking = false;
    assert!(write(&pool, &f, &unmark, true).await.is_err());
    let tombstone = Revision {
        version: 2,
        kind: "TOMBSTONE",
        comment_version: Some(2),
        body: None,
        ..open
    };
    write(&pool, &f, &tombstone, true).await.unwrap();
    let blocked: bool = sqlx::query_scalar("SELECT blocking AND NOT resolved FROM discussion_revisions WHERE thread_id=$1 ORDER BY version DESC LIMIT 1").bind(open.thread).fetch_one(&pool).await.unwrap();
    assert!(blocked);
    sqlx::query("UPDATE memberships SET role='CONTRIBUTOR' WHERE user_id=$1")
        .bind(f.product)
        .execute(&pool)
        .await
        .unwrap();
    write(&pool, &f, &open.state(3, f.product, "RESOLVE", true), true)
        .await
        .unwrap();
    write(&pool, &f, &open.state(4, f.product, "REOPEN", false), true)
        .await
        .unwrap();
    let rows: Vec<(String, bool, bool, Option<String>)> = sqlx::query_as("SELECT kind,resolved,independent_requirement,body FROM discussion_revisions WHERE thread_id=$1 ORDER BY version").bind(open.thread).fetch_all(&pool).await.unwrap();
    assert_eq!(rows.len(), 4);
    assert!(rows.iter().all(|r| r.2));
    assert_eq!(rows[0].3.as_deref(), Some("Original discussion text"));
    assert!(rows[1].3.is_none());
    assert_eq!(rows[3].0, "REOPEN");
    assert!(!rows[3].1);
    let decisions: i64 = sqlx::query_scalar("SELECT count(*) FROM official_reviews")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(decisions, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn discussion_revisions_require_atomic_audit_and_event_and_cannot_be_rewritten(pool: PgPool) {
    let f = fixture(&pool).await;
    let open = Revision::opening(f.owner, false);
    assert!(write(&pool, &f, &open, false).await.is_err());
    let counts: (i64,i64,i64) = sqlx::query_as("SELECT (SELECT count(*) FROM discussion_threads),(SELECT count(*) FROM discussion_revisions),(SELECT count(*) FROM discussion_attempts)").fetch_one(&pool).await.unwrap();
    assert_eq!(counts, (0, 0, 0));
    write(&pool, &f, &open, true).await.unwrap();
    for statement in [
        "UPDATE discussion_revisions SET blocking=false",
        "DELETE FROM discussion_revisions",
        "TRUNCATE discussion_revisions CASCADE",
        "UPDATE discussion_threads SET anchor='{}'",
        "DELETE FROM discussion_threads",
        "TRUNCATE discussion_threads CASCADE",
        "UPDATE discussion_attempts SET request_hash=repeat('b',64)",
        "DELETE FROM discussion_attempts",
        "TRUNCATE discussion_attempts CASCADE",
        "UPDATE outbox SET payload='{}' WHERE event_type='DISCUSSION_CHANGED'",
        "DELETE FROM outbox WHERE event_type='DISCUSSION_CHANGED'",
    ] {
        assert!(
            sqlx::query(statement).execute(&pool).await.is_err(),
            "{statement}"
        );
    }
    sqlx::query(
        "UPDATE outbox SET sent_at=clock_timestamp() WHERE event_type='DISCUSSION_CHANGED'",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert!(
        sqlx::query("UPDATE outbox SET sent_at=NULL WHERE event_type='DISCUSSION_CHANGED'")
            .execute(&pool)
            .await
            .is_err()
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn message_author_versions_and_tombstones_are_enforced_independently_of_role(pool: PgPool) {
    let f = fixture(&pool).await;
    let open = Revision::opening(f.owner, false);
    write(&pool, &f, &open, true).await.unwrap();
    let mut edit = Revision {
        version: 2,
        actor: f.product,
        kind: "EDIT",
        blocking: true,
        resolved: false,
        independent: false,
        thread: open.thread,
        comment: open.comment,
        comment_version: Some(2),
        comment_author: Some(f.owner),
        body: Some("Forged rewrite".into()),
    };
    assert!(write(&pool, &f, &edit, true).await.is_err());
    edit.actor = f.owner;
    edit.comment_version = None;
    assert!(write(&pool, &f, &edit, true).await.is_err());
    edit.comment_version = Some(2);
    write(&pool, &f, &edit, true).await.unwrap();
    edit.version = 3;
    assert!(write(&pool, &f, &edit, true).await.is_err());
    edit.comment_version = Some(3);
    edit.kind = "TOMBSTONE";
    edit.body = None;
    write(&pool, &f, &edit, true).await.unwrap();
    edit.version = 4;
    edit.comment_version = Some(4);
    edit.kind = "EDIT";
    edit.body = Some("Resurrect deleted message".into());
    assert!(write(&pool, &f, &edit, true).await.is_err());
    let mut reply = Revision {
        version: 4,
        kind: "REPLY",
        actor: f.product,
        comment: Some(Uuid::new_v4()),
        comment_version: Some(1),
        comment_author: Some(f.product),
        body: Some("Reply without editor assignment".into()),
        ..edit
    };
    write(&pool, &f, &reply, true).await.unwrap();
    reply.version = 5;
    reply.comment = Some(Uuid::new_v4());
    reply.body = Some("🦀".repeat(10001));
    assert!(write(&pool, &f, &reply, true).await.is_err());
    sqlx::query("UPDATE memberships SET role='READER' WHERE user_id=$1")
        .bind(f.product)
        .execute(&pool)
        .await
        .unwrap();
    reply.body = Some("Revoked permission".into());
    assert!(write(&pool, &f, &reply, true).await.is_err());
}

#[sqlx::test(migrations = "../../migrations")]
async fn discussion_attempts_bind_actor_scope_and_body_and_charge_failed_source_admissions(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    let request = Uuid::new_v4();
    for i in 0..60 {
        sqlx::query("INSERT INTO discussion_attempts(id,organization_id,proposal_id,actor_id,request_id,request_hash) VALUES($1,$2,$3,$4,$5,$6)")
            .bind(Uuid::new_v4()).bind(f.org).bind(f.proposal).bind(f.owner).bind(request).bind("a".repeat(64)).execute(&pool).await.unwrap();
        if i == 0 {
            for (actor, hash) in [(f.product, "a".repeat(64)), (f.owner, "b".repeat(64))] {
                assert!(sqlx::query("INSERT INTO discussion_attempts(id,organization_id,proposal_id,actor_id,request_id,request_hash) VALUES($1,$2,$3,$4,$5,$6)")
                    .bind(Uuid::new_v4()).bind(f.org).bind(f.proposal).bind(actor).bind(request).bind(hash).execute(&pool).await.is_err());
            }
        }
    }
    assert!(sqlx::query("INSERT INTO discussion_attempts(id,organization_id,proposal_id,actor_id,request_id,request_hash) VALUES($1,$2,$3,$4,$5,$6)")
        .bind(Uuid::new_v4()).bind(f.org).bind(f.proposal).bind(f.owner).bind(request).bind("a".repeat(64)).execute(&pool).await.is_err());
    let attempts: i64 = sqlx::query_scalar("SELECT count(*) FROM discussion_attempts")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(attempts, 60);
}

#[sqlx::test(migrations = "../../migrations")]
async fn discussion_origins_match_frozen_snapshot_and_manifest_without_browser_locators(
    pool: PgPool,
) {
    let (f, source, preparer) = setup(&pool).await;
    let (lease, verified) = prepared(&pool, &f, &preparer, 2).await;
    let file = verified
        .candidate()
        .plan()
        .files()
        .iter()
        .find(|file| file.kind == specspace_application::comparison::Kind::Document)
        .unwrap()
        .clone();
    let published = PgReviewSnapshots::new(pool.clone())
        .publish(&lease, &verified, &preparer)
        .await
        .unwrap();
    let side = file.after.as_ref().unwrap();
    let text = std::str::from_utf8(&source.trees[sha(1).as_str()][&side.path]).unwrap();
    let evidence = specspace_document_engine::discussion_anchor::TextAnchor::document(
        text,
        side.document_id.as_deref().unwrap(),
    )
    .unwrap();
    let valid = serde_json::json!({"kind":"file","file_id":file.id,"side":"after","source":side,"text":evidence});
    for variant in 0..11 {
        let mut anchor = valid.clone();
        match variant {
            3 => {
                anchor["text"]
                    .as_object_mut()
                    .unwrap()
                    .remove("quote_truncated");
            }
            4 => anchor["source"]["path"] = serde_json::json!("docs/private.md"),
            5 => anchor["side"] = serde_json::json!("before"),
            6 => {
                anchor.as_object_mut().unwrap().remove("kind");
            }
            8 => anchor["text"]["end_byte"] = serde_json::json!(side.bytes + 1),
            9 => {
                anchor.as_object_mut().unwrap().remove("source");
            }
            10 => anchor["text"]["quote"] = serde_json::json!({"forged":"object"}),
            _ => {}
        }
        let mut tx = pool.begin().await.unwrap();
        let result = sqlx::query("INSERT INTO discussion_threads(id,organization_id,proposal_id,creator_id,snapshot_id,fingerprint,repository_id,remote_id,installation_id,branch,head_sha,target_sha,merge_base_sha,proposal_version,anchor,opening_revision) SELECT $1,p.organization_id,p.id,$2,h.id,CASE WHEN $6=1 THEN repeat('f',64) ELSE h.fingerprint END,p.repository_id,CASE WHEN $6=2 THEN '99' ELSE h.remote_id END,h.installation_id,h.branch,h.head_sha,h.target_sha,h.merge_base_sha,p.version+CASE WHEN $6=7 THEN 1 ELSE 0 END,$4,$5 FROM proposals p JOIN review_snapshots h ON h.proposal_id=p.id WHERE h.id=$3")
            .bind(Uuid::new_v4()).bind(f.product).bind(published.snapshot_id).bind(anchor).bind(Uuid::new_v4()).bind(variant).execute(&mut *tx).await;
        assert_eq!(
            result.is_ok(),
            variant == 0,
            "origin variant {variant}: {result:?}"
        );
        tx.rollback().await.unwrap();
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn concurrent_discussion_transitions_serialize_on_the_proposal_without_lost_updates(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    let open = Revision::opening(f.owner, false);
    write(&pool, &f, &open, true).await.unwrap();
    let product = open.state(2, f.product, "RESOLVE", true);
    let development = open.state(2, f.development, "RESOLVE", true);
    let (a, b) = tokio::join!(
        write(&pool, &f, &product, true),
        write(&pool, &f, &development, true)
    );
    assert_ne!(a.is_ok(), b.is_ok());
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM discussion_revisions WHERE thread_id=$1")
            .bind(open.thread)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 2);
}
