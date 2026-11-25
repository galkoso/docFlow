use super::*;
use specspace_application::product_question::*;
use specspace_infrastructure::product_questions::{
    PgProductQuestions, ProductQuestionError as Error,
};
fn create() -> CreateProductQuestion {
    CreateProductQuestion {
        request_id: Uuid::new_v4(),
        expected_proposal_version: 2,
        text: "שאלה מקורית\r\n🦀".into(),
        blocking: true,
        proposed_answer: Some("Suggested answer, not a decision".into()),
    }
}
fn change(version: i64, action: ProductQuestionAction) -> ChangeProductQuestion {
    ChangeProductQuestion {
        request_id: Uuid::new_v4(),
        expected_version: version,
        action,
    }
}
#[sqlx::test(migrations = "../../migrations")]
async fn exact_receipts_survive_later_state_while_changed_intents_and_lost_product_authority_do_not_replay(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    let store = PgProductQuestions::new(pool.clone());
    let request = create();
    let first = store
        .create(f.product, f.proposal, request.clone())
        .await
        .unwrap();
    let replay = store
        .create(f.product, f.proposal, request.clone())
        .await
        .unwrap();
    assert!(replay.replayed && first.id == replay.id);
    let current = store.list(f.owner, f.proposal, None).await.unwrap();
    assert_eq!(current.unresolved_blockers, 1);
    assert_eq!(current.questions[0].text, request.text);
    assert!(matches!(
        current.questions[0].current.state,
        ProductQuestionPhase::Proposed
    ));
    assert!(current.questions[0].current.decision.is_none());
    let decide = change(
        1,
        ProductQuestionAction::Decide {
            answer: "Explicit human answer".into(),
        },
    );
    assert_eq!(
        store
            .change(f.development, f.proposal, first.question_id, decide.clone())
            .await
            .err(),
        Some(Error::ProductRequired)
    );
    let decision = store
        .change(f.product, f.proposal, first.question_id, decide.clone())
        .await
        .unwrap();
    assert!(
        store
            .change(f.product, f.proposal, first.question_id, decide.clone())
            .await
            .unwrap()
            .replayed
    );
    let mut altered = decide.clone();
    altered.expected_version = 2;
    assert_eq!(
        store
            .change(f.product, f.proposal, first.question_id, altered)
            .await
            .err(),
        Some(Error::IdempotencyMismatch)
    );
    let mut altered = request.clone();
    altered.text.push('!');
    assert_eq!(
        store.create(f.product, f.proposal, altered).await.err(),
        Some(Error::IdempotencyMismatch)
    );
    assert_eq!(
        store
            .list(f.owner, f.proposal, None)
            .await
            .unwrap()
            .unresolved_blockers,
        0
    );
    sqlx::query("UPDATE memberships SET review_domains='{}' WHERE user_id=$1")
        .bind(f.product)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        store
            .change(f.product, f.proposal, first.question_id, decide)
            .await
            .err(),
        Some(Error::ProductRequired)
    );
    let history = store
        .history(f.owner, f.proposal, first.question_id, None)
        .await
        .unwrap();
    assert_eq!(history.revisions.len(), 2);
    assert_eq!(history.revisions[1].id, decision.id);
    assert_eq!(
        history.question.current.decision.unwrap().actor.id,
        f.product
    );
    sqlx::query("UPDATE proposals SET state='CLOSED' WHERE id=$1")
        .bind(f.proposal)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        store
            .create(f.product, f.proposal, request)
            .await
            .unwrap()
            .replayed
    );
    assert_eq!(
        store.create(f.owner, f.proposal, create()).await.err(),
        Some(Error::NotEditable)
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn concurrent_retries_share_one_receipt_and_atomic_failure_leaves_the_nonce_available(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    let store = PgProductQuestions::new(pool.clone());
    let request = create();
    let (a, b) = tokio::join!(
        store.create(f.owner, f.proposal, request.clone()),
        store.create(f.owner, f.proposal, request)
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(a.id, b.id);
    assert_ne!(a.replayed, b.replayed);
    let x = change(
        1,
        ProductQuestionAction::Propose {
            answer: "One answer".into(),
        },
    );
    let y = change(
        1,
        ProductQuestionAction::Propose {
            answer: "Other answer".into(),
        },
    );
    let (x, y) = tokio::join!(
        store.change(f.owner, f.proposal, a.question_id, x),
        store.change(f.development, f.proposal, a.question_id, y)
    );
    assert_ne!(x.is_ok(), y.is_ok());
    assert_eq!(x.err().or(y.err()), Some(Error::Stale));
    sqlx::raw_sql("CREATE FUNCTION fail_question_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='PRODUCT_QUESTION_CHANGED' THEN RAISE EXCEPTION 'fixture audit failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER fail_question_audit BEFORE INSERT ON audit_events FOR EACH ROW EXECUTE FUNCTION fail_question_audit();").execute(&pool).await.unwrap();
    let failed = create();
    assert_eq!(
        store
            .create(f.owner, f.proposal, failed.clone())
            .await
            .err(),
        Some(Error::Unavailable)
    );
    assert_eq!(
        store
            .list(f.owner, f.proposal, None)
            .await
            .unwrap()
            .questions
            .len(),
        1
    );
    sqlx::query("DROP TRIGGER fail_question_audit ON audit_events")
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        !store
            .create(f.owner, f.proposal, failed)
            .await
            .unwrap()
            .replayed
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn shared_pages_count_all_blockers_and_history_cursors_remain_stable_after_appends(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    let store = PgProductQuestions::new(pool.clone());
    let oldest = store.create(f.owner, f.proposal, create()).await.unwrap();
    for _ in 0..20 {
        let mut r = create();
        r.blocking = false;
        store.create(f.owner, f.proposal, r).await.unwrap();
    }
    let reader = user(&pool, f.org, "READER", vec![]).await;
    let first = store.list(reader, f.proposal, None).await.unwrap();
    assert_eq!(first.questions.len(), 20);
    assert_eq!(first.unresolved_blockers, 1);
    assert!(!first.can_create);
    assert!(
        first.questions.iter().all(|q| !q.current.blocking
            && !q.permissions.can_decide
            && !q.permissions.can_propose)
    );
    let second = store
        .list(reader, f.proposal, first.next_after)
        .await
        .unwrap();
    assert_eq!(second.questions[0].id, oldest.question_id);
    assert!(second.next_after.is_none());
    for v in 1..25 {
        store
            .change(
                f.owner,
                f.proposal,
                oldest.question_id,
                change(
                    v,
                    ProductQuestionAction::Propose {
                        answer: format!("answer {v}"),
                    },
                ),
            )
            .await
            .unwrap();
    }
    let page = store
        .history(reader, f.proposal, oldest.question_id, None)
        .await
        .unwrap();
    assert_eq!(page.revisions.len(), 20);
    assert_eq!(page.next_after, Some(20));
    store
        .change(
            f.owner,
            f.proposal,
            oldest.question_id,
            change(
                25,
                ProductQuestionAction::Propose {
                    answer: "new append".into(),
                },
            ),
        )
        .await
        .unwrap();
    let rest = store
        .history(reader, f.proposal, oldest.question_id, page.next_after)
        .await
        .unwrap();
    assert_eq!(rest.revisions.len(), 6);
    assert_eq!(rest.revisions.last().unwrap().version, 26);
    assert_eq!(
        store
            .history(reader, f.proposal, oldest.question_id, Some(999))
            .await
            .err(),
        Some(Error::Invalid)
    );
    assert_eq!(
        store.create(reader, f.proposal, create()).await.err(),
        Some(Error::Forbidden)
    );
    let other = fixture(&pool).await;
    assert_eq!(
        store.list(other.owner, f.proposal, None).await.err(),
        Some(Error::NotFound)
    );
    assert_eq!(
        store
            .history(other.owner, f.proposal, oldest.question_id, None)
            .await
            .err(),
        Some(Error::NotFound)
    );
    assert_eq!(
        store
            .list(reader, f.proposal, Some(Uuid::new_v4()))
            .await
            .err(),
        Some(Error::Invalid)
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn imported_answers_outdated_origins_and_missing_state_never_hide_a_blocker(pool: PgPool) {
    let f = fixture(&pool).await;
    let q = Question::new(f.owner, true);
    let r = Revision::open(f.owner, Some("Imported suggestion"));
    write(&pool, &f, &q, &r, true).await.unwrap();
    let store = PgProductQuestions::new(pool.clone());
    let before = store.list(f.product, f.proposal, None).await.unwrap();
    assert!(matches!(
        before.questions[0].source,
        ProductQuestionSource::Imported
    ));
    assert!(before.questions[0].source_reference.is_some());
    sqlx::query("UPDATE proposals SET head_sha=$2,version=version+1 WHERE id=$1")
        .bind(f.proposal)
        .bind("3".repeat(40))
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE users SET status='DISABLED' WHERE id=$1")
        .bind(f.owner)
        .execute(&pool)
        .await
        .unwrap();
    let page = store.list(f.product, f.proposal, None).await.unwrap();
    assert_eq!(page.unresolved_blockers, 1);
    assert!(page.questions[0].outdated_context);
    assert_eq!(
        store.list(f.owner, f.proposal, None).await.err(),
        Some(Error::NotFound)
    );
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("ALTER TABLE product_question_revisions DISABLE TRIGGER ALL")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("DELETE FROM product_question_revisions WHERE question_id=$1")
        .bind(q.id)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("ALTER TABLE product_question_revisions ENABLE TRIGGER ALL")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        store.list(f.product, f.proposal, None).await.err(),
        Some(Error::Unavailable)
    );
    assert_eq!(
        store.history(f.product, f.proposal, q.id, None).await.err(),
        Some(Error::Unavailable)
    );
}
