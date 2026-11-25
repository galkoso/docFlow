use super::*;
use serde_json::json;
#[path = "product_questions/service.rs"]
mod service;
#[derive(Clone)]
struct Question {
    id: Uuid,
    source: &'static str,
    reference: Option<Uuid>,
    creator: Uuid,
}
impl Question {
    fn new(creator: Uuid, imported: bool) -> Self {
        Self {
            id: Uuid::new_v4(),
            creator,
            source: if imported { "IMPORTED" } else { "HUMAN" },
            reference: imported.then(Uuid::new_v4),
        }
    }
}
#[derive(Clone)]
struct Revision {
    version: i64,
    actor: Uuid,
    request: Uuid,
    kind: &'static str,
    state: &'static str,
    blocking: bool,
    answer: Option<String>,
    decider: Option<Uuid>,
    text: String,
}
impl Revision {
    fn open(actor: Uuid, answer: Option<&str>) -> Self {
        Self {
            version: 1,
            actor,
            request: Uuid::new_v4(),
            kind: "OPEN",
            state: if answer.is_some() { "PROPOSED" } else { "OPEN" },
            blocking: true,
            answer: answer.map(str::to_owned),
            decider: None,
            text: "שאלה עם מקור\r\n🦀".into(),
        }
    }
    fn next(
        &self,
        actor: Uuid,
        kind: &'static str,
        state: &'static str,
        answer: Option<&str>,
        decider: Option<Uuid>,
    ) -> Self {
        Self {
            version: self.version + 1,
            actor,
            request: Uuid::new_v4(),
            kind,
            state,
            answer: answer.map(str::to_owned),
            decider,
            ..self.clone()
        }
    }
}
async fn write(
    pool: &PgPool,
    f: &Fixture,
    q: &Question,
    r: &Revision,
    complete: bool,
) -> Result<Uuid, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT id FROM proposals WHERE id=$1 FOR UPDATE")
        .bind(f.proposal)
        .execute(&mut *tx)
        .await?;
    let (id, audit, event) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    if r.version == 1 {
        sqlx::query("INSERT INTO product_questions(id,organization_id,proposal_id,creator_id,source,source_reference,repository_id,branch,head_sha,target_sha,proposal_version,opening_revision) SELECT $1,p.organization_id,p.id,$2,$3,$4,p.repository_id,p.branch,p.head_sha,p.target_sha,p.version,$5 FROM proposals p WHERE p.id=$6")
            .bind(q.id).bind(q.creator).bind(q.source).bind(q.reference).bind(id).bind(f.proposal).execute(&mut *tx).await?;
    }
    let decided_at: Option<sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>> = if r.kind
        == "SET_BLOCKING"
    {
        sqlx::query_scalar("SELECT decided_at FROM product_question_revisions WHERE question_id=$1 ORDER BY version DESC LIMIT 1").bind(q.id).fetch_one(&mut *tx).await?
    } else if r.state == "DECIDED" {
        Some(sqlx::types::chrono::Utc::now())
    } else {
        None
    };
    sqlx::query("INSERT INTO product_question_revisions(id,organization_id,proposal_id,question_id,version,actor_id,request_id,request_hash,kind,question_text,blocking,state,answer,decided_by,decided_at,audit_id,event_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17)")
        .bind(id).bind(f.org).bind(f.proposal).bind(q.id).bind(r.version).bind(r.actor).bind(r.request).bind("a".repeat(64)).bind(r.kind).bind(&r.text)
        .bind(r.blocking).bind(r.state).bind(&r.answer).bind(r.decider).bind(decided_at).bind(audit).bind(event).execute(&mut *tx).await?;
    if complete {
        sqlx::query("INSERT INTO audit_events(id,organization_id,actor_id,action,entity_id,after_ref,result) VALUES($1,$2,$3,'PRODUCT_QUESTION_CHANGED',$4,$5,$6)")
            .bind(audit).bind(f.org).bind(r.actor).bind(q.id).bind(id.to_string()).bind(r.kind).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO outbox(id,organization_id,event_type,entity_id,payload) VALUES($1,$2,'PRODUCT_QUESTION_CHANGED',$3,$4)")
            .bind(event).bind(f.org).bind(f.proposal).bind(json!({"question_id":q.id,"revision_id":id,"version":r.version,"kind":r.kind})).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(id)
}

#[sqlx::test(migrations = "../../migrations")]
async fn imported_answers_remain_proposed_even_when_opened_by_product_authority(pool: PgPool) {
    let f = fixture(&pool).await;
    for (creator, imported) in [(f.owner, true), (f.product, true), (f.product, false)] {
        let q = Question::new(creator, imported);
        let proposed = Revision::open(creator, Some("Suggested answer from outside the system"));
        let mut forged = proposed.clone();
        forged.state = "DECIDED";
        forged.decider = Some(creator);
        assert!(write(&pool, &f, &q, &forged, true).await.is_err());
        write(&pool, &f, &q, &proposed, true).await.unwrap();
        let saved: (String, Option<Uuid>, bool) = sqlx::query_as(
            "SELECT state,decided_by,blocking FROM product_question_revisions WHERE question_id=$1",
        )
        .bind(q.id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(saved, ("PROPOSED".into(), None, true));
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn question_decisions_need_current_product_qualification_and_cannot_forge_the_decider(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    let q = Question::new(f.owner, true);
    let first = Revision::open(f.owner, Some("Draft answer"));
    write(&pool, &f, &q, &first, true).await.unwrap();
    for actor in [
        f.owner,
        f.development,
        user(&pool, f.org, "ADMIN", vec![]).await,
        user(&pool, f.org, "MAINTAINER", vec!["DEVELOPMENT"]).await,
        user(&pool, f.org, "CONTRIBUTOR", vec!["PRODUCT"]).await,
    ] {
        let decide = first.next(
            actor,
            "DECIDE",
            "DECIDED",
            Some("Forged approval"),
            Some(actor),
        );
        assert!(write(&pool, &f, &q, &decide, true).await.is_err());
    }
    let mut decide = first.next(
        f.product,
        "DECIDE",
        "DECIDED",
        Some("Human product decision"),
        Some(f.owner),
    );
    assert!(write(&pool, &f, &q, &decide, true).await.is_err());
    decide.decider = Some(f.product);
    sqlx::query("UPDATE memberships SET review_domains='{}' WHERE user_id=$1")
        .bind(f.product)
        .execute(&pool)
        .await
        .unwrap();
    assert!(write(&pool, &f, &q, &decide, true).await.is_err());
    sqlx::query("UPDATE memberships SET review_domains=ARRAY['PRODUCT'] WHERE user_id=$1")
        .bind(f.product)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE users SET status='DISABLED' WHERE id=$1")
        .bind(f.product)
        .execute(&pool)
        .await
        .unwrap();
    assert!(write(&pool, &f, &q, &decide, true).await.is_err());
    sqlx::query("UPDATE users SET status='ACTIVE' WHERE id=$1")
        .bind(f.product)
        .execute(&pool)
        .await
        .unwrap();
    write(&pool, &f, &q, &decide, true).await.unwrap();
    let recorded: (Uuid,bool) = sqlx::query_as("SELECT decided_by,decided_at=created_at FROM product_question_revisions WHERE question_id=$1 AND version=2").bind(q.id).fetch_one(&pool).await.unwrap();
    assert_eq!(recorded, (f.product, true));
}

#[sqlx::test(migrations = "../../migrations")]
async fn question_revisions_preserve_history_and_product_control_of_blocking(pool: PgPool) {
    let f = fixture(&pool).await;
    let q = Question::new(f.owner, false);
    let first = Revision::open(f.owner, None);
    write(&pool, &f, &q, &first, true).await.unwrap();
    let mut rewritten = first.next(
        f.owner,
        "PROPOSE",
        "PROPOSED",
        Some("Candidate answer"),
        None,
    );
    rewritten.text = "Silently changed the question".into();
    assert!(write(&pool, &f, &q, &rewritten, true).await.is_err());
    let proposed = first.next(
        f.development,
        "PROPOSE",
        "PROPOSED",
        Some("Candidate answer"),
        None,
    );
    write(&pool, &f, &q, &proposed, true).await.unwrap();
    let decided = proposed.next(
        f.product,
        "DECIDE",
        "DECIDED",
        Some("Approved business rule"),
        Some(f.product),
    );
    write(&pool, &f, &q, &decided, true).await.unwrap();
    assert!(
        write(
            &pool,
            &f,
            &q,
            &decided.next(
                f.owner,
                "PROPOSE",
                "PROPOSED",
                Some("Silent replacement"),
                None
            ),
            true
        )
        .await
        .is_err()
    );
    let mut unmark = decided.next(
        f.owner,
        "SET_BLOCKING",
        "DECIDED",
        Some("Approved business rule"),
        Some(f.product),
    );
    unmark.blocking = false;
    assert!(write(&pool, &f, &q, &unmark, true).await.is_err());
    unmark.actor = f.product;
    write(&pool, &f, &q, &unmark, true).await.unwrap();
    let reopened = unmark.next(f.owner, "REOPEN", "OPEN", None, None);
    write(&pool, &f, &q, &reopened, true).await.unwrap();
    let history: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT state,answer FROM product_question_revisions WHERE question_id=$1 ORDER BY version",
    )
    .bind(q.id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(history.len(), 5);
    assert_eq!(
        history[2],
        ("DECIDED".into(), Some("Approved business rule".into()))
    );
    assert_eq!(history[4], ("OPEN".into(), None));
    for sql in [
        "UPDATE product_question_revisions SET state='OPEN'",
        "DELETE FROM product_question_revisions",
        "TRUNCATE product_question_revisions",
        "UPDATE product_questions SET source='HUMAN'",
        "DELETE FROM product_questions",
        "TRUNCATE product_questions",
        "UPDATE outbox SET payload='{}' WHERE event_type='PRODUCT_QUESTION_CHANGED'",
        "DELETE FROM outbox WHERE event_type='PRODUCT_QUESTION_CHANGED'",
    ] {
        assert!(sqlx::query(sql).execute(&pool).await.is_err(), "{sql}");
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn question_merge_barrier_forbids_new_blockers_and_concurrent_versions_have_one_winner(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    let q = Question::new(f.owner, false);
    let first = Revision::open(f.owner, None);
    write(&pool, &f, &q, &first, true).await.unwrap();
    let a = first.next(
        f.product,
        "DECIDE",
        "DECIDED",
        Some("Product choice"),
        Some(f.product),
    );
    let b = first.next(
        f.development,
        "PROPOSE",
        "PROPOSED",
        Some("Another choice"),
        None,
    );
    let (a, b) = tokio::join!(
        write(&pool, &f, &q, &a, true),
        write(&pool, &f, &q, &b, true)
    );
    assert_ne!(a.is_ok(), b.is_ok());
    let (state,answer,decider):(String,Option<String>,Option<Uuid>) = sqlx::query_as("SELECT state,answer,decided_by FROM product_question_revisions WHERE question_id=$1 ORDER BY version DESC LIMIT 1").bind(q.id).fetch_one(&pool).await.unwrap();
    let mut last = first.clone();
    last.version = 2;
    last.state = if state == "DECIDED" {
        "DECIDED"
    } else {
        "PROPOSED"
    };
    last.answer = answer;
    last.decider = decider;
    let decided = last.next(
        f.product,
        "DECIDE",
        "DECIDED",
        Some("Final product choice"),
        Some(f.product),
    );
    write(&pool, &f, &q, &decided, true).await.unwrap();
    sqlx::query("UPDATE proposals SET state='MERGING' WHERE id=$1")
        .bind(f.proposal)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        write(
            &pool,
            &f,
            &q,
            &decided.next(f.owner, "REOPEN", "OPEN", None, None),
            true
        )
        .await
        .is_err()
    );
    let new = Question::new(f.owner, false);
    let mut blocked = first.clone();
    blocked.request = Uuid::new_v4();
    assert!(write(&pool, &f, &new, &blocked, true).await.is_err());
    let mut nonblocking = first.clone();
    nonblocking.blocking = false;
    nonblocking.request = Uuid::new_v4();
    write(&pool, &f, &new, &nonblocking, true).await.unwrap();
    let mut mark = nonblocking.next(f.owner, "SET_BLOCKING", "OPEN", None, None);
    mark.blocking = true;
    assert!(write(&pool, &f, &new, &mark, true).await.is_err());
}

#[sqlx::test(migrations = "../../migrations")]
async fn question_opening_is_atomic_scoped_and_unicode_bounded(pool: PgPool) {
    let f = fixture(&pool).await;
    let q = Question::new(f.owner, false);
    let first = Revision::open(f.owner, None);
    assert!(write(&pool, &f, &q, &first, false).await.is_err());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM product_questions")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    for invalid in [
        "".into(),
        " \t\r\n".into(),
        "\u{2003}".into(),
        "🦀".repeat(10001),
    ] {
        let mut r = first.clone();
        r.text = invalid;
        assert!(write(&pool, &f, &q, &r, true).await.is_err());
    }
    let mut valid = first.clone();
    valid.text = "v".into();
    write(&pool, &f, &q, &valid, true).await.unwrap();
    let other = fixture(&pool).await;
    assert!(
        write(
            &pool,
            &other,
            &q,
            &valid.next(
                other.product,
                "DECIDE",
                "DECIDED",
                Some("Wrong proposal"),
                Some(other.product)
            ),
            true
        )
        .await
        .is_err()
    );
    let duplicate = Question::new(f.owner, false);
    assert!(write(&pool, &f, &duplicate, &valid, true).await.is_err());
    let mut long = Revision::open(f.owner, Some(&"🦀".repeat(10000)));
    long.request = Uuid::new_v4();
    write(&pool, &f, &duplicate, &long, true).await.unwrap();
    long.version = 2;
    long.kind = "PROPOSE";
    long.request = Uuid::new_v4();
    long.answer = Some("\u{00a0}\u{2003}".into());
    assert!(write(&pool, &f, &duplicate, &long, true).await.is_err());
}

#[sqlx::test(migrations = "../../migrations")]
async fn question_quota_and_closed_proposal_reject_writes_without_partial_records(pool: PgPool) {
    let f = fixture(&pool).await;
    for _ in 0..60 {
        write(
            &pool,
            &f,
            &Question::new(f.owner, false),
            &Revision::open(f.owner, None),
            true,
        )
        .await
        .unwrap();
    }
    assert!(
        write(
            &pool,
            &f,
            &Question::new(f.owner, false),
            &Revision::open(f.owner, None),
            true
        )
        .await
        .is_err()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM product_questions")
            .fetch_one(&pool)
            .await
            .unwrap(),
        60
    );
    sqlx::query("UPDATE proposals SET state='CLOSED' WHERE id=$1")
        .bind(f.proposal)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        write(
            &pool,
            &f,
            &Question::new(f.product, false),
            &Revision::open(f.product, None),
            true
        )
        .await
        .is_err()
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn question_organization_quota_is_shared_across_actors_and_does_not_block_another_organization(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    for _ in 0..10 {
        let actor = user(&pool, f.org, "CONTRIBUTOR", vec![]).await;
        for _ in 0..60 {
            write(
                &pool,
                &f,
                &Question::new(actor, false),
                &Revision::open(actor, None),
                true,
            )
            .await
            .unwrap();
        }
    }
    let next_actor = user(&pool, f.org, "CONTRIBUTOR", vec![]).await;
    assert!(
        write(
            &pool,
            &f,
            &Question::new(next_actor, false),
            &Revision::open(next_actor, None),
            true
        )
        .await
        .is_err()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM product_questions WHERE organization_id=$1"
        )
        .bind(f.org)
        .fetch_one(&pool)
        .await
        .unwrap(),
        600
    );
    let other = fixture(&pool).await;
    write(
        &pool,
        &other,
        &Question::new(other.owner, false),
        &Revision::open(other.owner, None),
        true,
    )
    .await
    .unwrap();
}
