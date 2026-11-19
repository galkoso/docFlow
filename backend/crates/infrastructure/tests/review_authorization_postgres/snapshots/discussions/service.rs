use super::*;
#[path = "history.rs"]
mod history;
#[path = "orchestration.rs"]
mod orchestration;
use serde_json::{Value, json};
use specspace_application::{
    comparison_file::{
        ComparisonFileReader,
        discussion::{AnchorSide, TextSelection},
    },
    discussion::{
        DiscussionRecorded, OpenThreadRequest, SnapshotAnchor, ThreadMutation,
        ThreadMutationRequest, ThreadOrigin,
    },
};
use specspace_infrastructure::{
    discussions::{
        DiscussionStoreError as Error, OpenAdmission, OpenAdmissionResult, PgDiscussions,
    },
    review_files::PgReviewFileAuthorization,
};

fn proposal_request() -> OpenThreadRequest {
    OpenThreadRequest {
        request_id: Uuid::new_v4(),
        origin: ThreadOrigin::Proposal {
            expected_proposal_version: 2,
        },
        blocking: false,
        body: "Exact discussion text\r\nשָׁלוֹם 🦀".into(),
    }
}
fn mutation(version: i64, mutation: ThreadMutation) -> ThreadMutationRequest {
    ThreadMutationRequest {
        request_id: Uuid::new_v4(),
        expected_thread_version: version,
        mutation,
    }
}
async fn admission(
    store: &PgDiscussions,
    actor: Uuid,
    proposal: Uuid,
    request: OpenThreadRequest,
) -> Box<OpenAdmission> {
    match store.admit_open(actor, proposal, request).await.unwrap() {
        OpenAdmissionResult::Ready(admission) => admission,
        OpenAdmissionResult::Recorded(_) => panic!("new intent unexpectedly replayed"),
    }
}
async fn open(
    store: &PgDiscussions,
    actor: Uuid,
    proposal: Uuid,
    request: OpenThreadRequest,
) -> DiscussionRecorded {
    let admission = admission(store, actor, proposal, request).await;
    store.complete_open(&admission, None).await.unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
async fn proposal_discussions_support_replies_edits_tombstones_and_state_without_content_changes(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    let store = PgDiscussions::new(pool.clone());
    let first = open(&store, f.owner, f.proposal, proposal_request()).await;
    let reply = store
        .mutate(
            f.product,
            f.proposal,
            first.thread_id,
            mutation(
                1,
                ThreadMutation::Reply {
                    body: "A reviewer can comment without editor assignment".into(),
                    reply_to_id: first.comment_id,
                },
            ),
        )
        .await
        .unwrap();
    let edit = store
        .mutate(
            f.owner,
            f.proposal,
            first.thread_id,
            mutation(
                2,
                ThreadMutation::Edit {
                    comment_id: first.comment_id.unwrap(),
                    expected_comment_version: 1,
                    body: "An exact revised message".into(),
                },
            ),
        )
        .await
        .unwrap();
    assert_eq!((edit.thread_version, edit.comment_version), (3, Some(2)));
    store
        .mutate(
            f.product,
            f.proposal,
            first.thread_id,
            mutation(
                3,
                ThreadMutation::Tombstone {
                    comment_id: reply.comment_id.unwrap(),
                    expected_comment_version: 1,
                },
            ),
        )
        .await
        .unwrap();
    let rows:Vec<(String,Option<String>,Option<Uuid>)>=sqlx::query_as("SELECT kind,body,reply_to_id FROM discussion_revisions WHERE thread_id=$1 ORDER BY version").bind(first.thread_id).fetch_all(&pool).await.unwrap();
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0].1.as_deref(), Some(proposal_request().body.as_str()));
    assert_eq!(rows[1].2, first.comment_id);
    assert!(rows[3].1.is_none());
    let unchanged: (i64, String, i64) = sqlx::query_as(
        "SELECT version,state,(SELECT count(*) FROM official_reviews) FROM proposals WHERE id=$1",
    )
    .bind(f.proposal)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(unchanged, (2, "DRAFT".into(), 0));
    let root: (Option<Uuid>, Value, String) =
        sqlx::query_as("SELECT snapshot_id,anchor,head_sha FROM discussion_threads WHERE id=$1")
            .bind(first.thread_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(root.0.is_none());
    assert_eq!(root.1, json!({"kind":"proposal"}));
    assert_eq!(root.2, sha(1).as_str());
    let events: Vec<Value> =
        sqlx::query_scalar("SELECT payload FROM outbox WHERE event_type='DISCUSSION_CHANGED'")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(events.len(), 4);
    assert!(
        events
            .iter()
            .all(|e| e.as_object().unwrap().len() == 4 && e.get("body").is_none())
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn successful_replay_is_exact_scoped_and_still_requires_current_write_permission(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    let store = PgDiscussions::new(pool.clone());
    let request = proposal_request();
    let first = open(&store, f.owner, f.proposal, request.clone()).await;
    let reply = mutation(
        1,
        ThreadMutation::Reply {
            body: "One reply".into(),
            reply_to_id: None,
        },
    );
    let recorded = store
        .mutate(f.product, f.proposal, first.thread_id, reply.clone())
        .await
        .unwrap();
    let replay = store
        .mutate(f.product, f.proposal, first.thread_id, reply)
        .await
        .unwrap();
    assert_eq!(recorded.id, replay.id);
    assert!(replay.replayed);
    sqlx::query("UPDATE proposals SET version=version+1,state='CLOSED' WHERE id=$1")
        .bind(f.proposal)
        .execute(&pool)
        .await
        .unwrap();
    let OpenAdmissionResult::Recorded(replayed) = store
        .admit_open(f.owner, f.proposal, request.clone())
        .await
        .unwrap()
    else {
        panic!("expected replay");
    };
    assert_eq!(first.id, replayed.id);
    assert!(replayed.replayed);
    let mut changed = request.clone();
    changed.body.push('!');
    assert_eq!(
        store.admit_open(f.owner, f.proposal, changed).await.err(),
        Some(Error::IdempotencyMismatch)
    );
    assert_eq!(
        store
            .admit_open(f.product, f.proposal, request.clone())
            .await
            .err(),
        Some(Error::IdempotencyMismatch)
    );
    sqlx::query("UPDATE memberships SET role='READER' WHERE user_id=$1")
        .bind(f.owner)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        store.admit_open(f.owner, f.proposal, request).await.err(),
        Some(Error::Forbidden)
    );
    let attempts: i64 = sqlx::query_scalar("SELECT count(*) FROM discussion_attempts")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(attempts, 2);
}

#[sqlx::test(migrations = "../../migrations")]
async fn policy_prevents_author_admin_bypasses_and_tombstones_cannot_withdraw_blockers(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    let store = PgDiscussions::new(pool.clone());
    sqlx::query("UPDATE memberships SET role='ADMIN' WHERE user_id=$1")
        .bind(f.owner)
        .execute(&pool)
        .await
        .unwrap();
    let mut request = proposal_request();
    request.blocking = true;
    let first = open(&store, f.product, f.proposal, request).await;
    for action in [
        ThreadMutation::Resolve {},
        ThreadMutation::SetBlocking { blocking: false },
    ] {
        assert_eq!(
            store
                .mutate(f.owner, f.proposal, first.thread_id, mutation(1, action))
                .await
                .err(),
            Some(Error::IndependentReviewerRequired)
        );
    }
    assert_eq!(
        store
            .mutate(
                f.owner,
                f.proposal,
                first.thread_id,
                mutation(
                    1,
                    ThreadMutation::Edit {
                        comment_id: first.comment_id.unwrap(),
                        expected_comment_version: 1,
                        body: "Admin rewrite".into()
                    }
                )
            )
            .await
            .err(),
        Some(Error::Forbidden)
    );
    store
        .mutate(
            f.product,
            f.proposal,
            first.thread_id,
            mutation(
                1,
                ThreadMutation::Tombstone {
                    comment_id: first.comment_id.unwrap(),
                    expected_comment_version: 1,
                },
            ),
        )
        .await
        .unwrap();
    let blocked:bool=sqlx::query_scalar("SELECT blocking AND NOT resolved FROM discussion_revisions WHERE thread_id=$1 ORDER BY version DESC LIMIT 1").bind(first.thread_id).fetch_one(&pool).await.unwrap();
    assert!(blocked);
    sqlx::query("UPDATE memberships SET role='CONTRIBUTOR' WHERE user_id=$1")
        .bind(f.product)
        .execute(&pool)
        .await
        .unwrap();
    store
        .mutate(
            f.product,
            f.proposal,
            first.thread_id,
            mutation(2, ThreadMutation::Resolve {}),
        )
        .await
        .unwrap();
    store
        .mutate(
            f.product,
            f.proposal,
            first.thread_id,
            mutation(3, ThreadMutation::Reopen {}),
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .mutate(
                f.owner,
                f.proposal,
                first.thread_id,
                mutation(4, ThreadMutation::Resolve {})
            )
            .await
            .err(),
        Some(Error::IndependentReviewerRequired)
    );
}

struct Review {
    f: Fixture,
    reader: ComparisonFileReader,
    request: OpenThreadRequest,
    snapshot: Uuid,
    file: String,
    source: Arc<Source>,
}
async fn review(pool: &PgPool) -> Review {
    let (f, source, preparer) = setup(pool).await;
    let (lease, verified) = prepared(pool, &f, &preparer, 2).await;
    let file = verified
        .candidate()
        .plan()
        .files()
        .iter()
        .find(|file| file.kind == specspace_application::comparison::Kind::Document)
        .unwrap()
        .id
        .clone();
    let published = PgReviewSnapshots::new(pool.clone())
        .publish(&lease, &verified, &preparer)
        .await
        .unwrap();
    let reader = ComparisonFileReader::new(
        Arc::new(PgReviewFileAuthorization::new(pool.clone())),
        Arc::new(Factory {
            source: source.clone(),
            repository: f.repository,
        }),
        Duration::from_secs(30),
    )
    .unwrap();
    let request = OpenThreadRequest {
        request_id: Uuid::new_v4(),
        origin: ThreadOrigin::Snapshot {
            snapshot_id: published.snapshot_id,
            expected_fingerprint: published.fingerprint,
            anchor: SnapshotAnchor::File {
                file_id: file.clone(),
                side: AnchorSide::After,
                selection: TextSelection::Document {},
            },
        },
        blocking: true,
        body: "Discuss this exact historical document".into(),
    };
    Review {
        f,
        reader,
        request,
        snapshot: published.snapshot_id,
        file,
        source,
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn anchored_open_requires_admission_and_exact_proof_and_replays_concurrently(pool: PgPool) {
    let r = review(&pool).await;
    let store = PgDiscussions::new(pool.clone());
    let reader = user(&pool, r.f.org, "READER", vec![]).await;
    let before = r.source.reads.load(Ordering::SeqCst);
    assert_eq!(
        store
            .admit_open(reader, r.f.proposal, r.request.clone())
            .await
            .err(),
        Some(Error::Forbidden)
    );
    assert_eq!(r.source.reads.load(Ordering::SeqCst), before);
    // A discussion on an old saved snapshot retains that source after recorded
    // content advances. Its origin is never substituted with the current head.
    sqlx::query("UPDATE proposals SET head_sha=$2,version=version+1 WHERE id=$1")
        .bind(r.f.proposal)
        .bind(sha(2).as_str())
        .execute(&pool)
        .await
        .unwrap();
    let admitted = admission(&store, r.f.product, r.f.proposal, r.request.clone()).await;
    assert_eq!(
        store.complete_open(&admitted, None).await.err(),
        Some(Error::Invalid)
    );
    let before = r
        .reader
        .load_discussion_anchor(
            r.f.product,
            r.f.proposal,
            r.snapshot,
            &r.file,
            AnchorSide::Before,
            TextSelection::Document {},
        )
        .await
        .unwrap();
    assert_eq!(
        store.complete_open(&admitted, Some(&before)).await.err(),
        Some(Error::Stale)
    );
    let lines = r
        .reader
        .load_discussion_anchor(
            r.f.product,
            r.f.proposal,
            r.snapshot,
            &r.file,
            AnchorSide::After,
            TextSelection::Lines {
                start_line: 1,
                end_line: 1,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        store.complete_open(&admitted, Some(&lines)).await.err(),
        Some(Error::Invalid)
    );
    let proof = r
        .reader
        .load_discussion_anchor(
            r.f.product,
            r.f.proposal,
            r.snapshot,
            &r.file,
            AnchorSide::After,
            TextSelection::Document {},
        )
        .await
        .unwrap();
    let (a, b) = tokio::join!(
        store.complete_open(&admitted, Some(&proof)),
        store.complete_open(&admitted, Some(&proof))
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(a.id, b.id);
    assert_ne!(a.replayed, b.replayed);
    let origin: (String, Uuid, Value) =
        sqlx::query_as("SELECT head_sha,snapshot_id,anchor FROM discussion_threads WHERE id=$1")
            .bind(a.thread_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(origin.0, sha(1).as_str());
    assert_eq!(origin.1, r.snapshot);
    assert_eq!(origin.2["source"]["path"], "docs/a.md");
    assert_eq!(
        origin.2["text"]["source_sha256"],
        proof.text().source_sha256()
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn final_authority_and_scope_rechecks_discard_prepared_anchor_after_revocation_or_drift(
    pool: PgPool,
) {
    let r = review(&pool).await;
    let store = PgDiscussions::new(pool.clone());
    let admitted = admission(&store, r.f.product, r.f.proposal, r.request.clone()).await;
    let proof = r
        .reader
        .load_discussion_anchor(
            r.f.product,
            r.f.proposal,
            r.snapshot,
            &r.file,
            AnchorSide::After,
            TextSelection::Document {},
        )
        .await
        .unwrap();
    sqlx::query("UPDATE memberships SET role='READER' WHERE user_id=$1")
        .bind(r.f.product)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        store.complete_open(&admitted, Some(&proof)).await.err(),
        Some(Error::Forbidden)
    );
    sqlx::query("UPDATE memberships SET role='REVIEWER' WHERE user_id=$1")
        .bind(r.f.product)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE proposals SET version=version+1 WHERE id=$1")
        .bind(r.f.proposal)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        store.complete_open(&admitted, Some(&proof)).await.err(),
        Some(Error::Stale)
    );
    let counts:(i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM discussion_attempts),(SELECT count(*) FROM discussion_threads)").fetch_one(&pool).await.unwrap();
    assert_eq!(counts, (1, 0));
    let mut changed = r.request.clone();
    changed.body.push('!');
    assert_eq!(
        store
            .admit_open(r.f.product, r.f.proposal, changed)
            .await
            .err(),
        Some(Error::IdempotencyMismatch)
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn actor_quota_charges_abandoned_admissions_but_successful_replay_needs_no_new_attempt(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    let store = PgDiscussions::new(pool.clone());
    let request = proposal_request();
    let first = open(&store, f.owner, f.proposal, request.clone()).await;
    for _ in 1..60 {
        admission(&store, f.owner, f.proposal, proposal_request()).await;
    }
    assert_eq!(
        store
            .admit_open(f.owner, f.proposal, proposal_request())
            .await
            .err(),
        Some(Error::Quota)
    );
    let OpenAdmissionResult::Recorded(replay) = store
        .admit_open(f.owner, f.proposal, request)
        .await
        .unwrap()
    else {
        panic!("expected replay at quota");
    };
    assert_eq!(replay.id, first.id);
}

#[sqlx::test(migrations = "../../migrations")]
async fn discussion_service_rolls_back_failed_audit_and_retries_the_same_admission(pool: PgPool) {
    let f = fixture(&pool).await;
    let store = PgDiscussions::new(pool.clone());
    let admitted = admission(&store, f.owner, f.proposal, proposal_request()).await;
    sqlx::query("CREATE FUNCTION fail_discussion_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action LIKE 'DISCUSSION_%' THEN RAISE EXCEPTION 'synthetic unavailable audit'; END IF; RETURN NEW; END $$").execute(&pool).await.unwrap();
    sqlx::query("CREATE TRIGGER fail_discussion_audit BEFORE INSERT ON audit_events FOR EACH ROW EXECUTE FUNCTION fail_discussion_audit()").execute(&pool).await.unwrap();
    assert_eq!(
        store.complete_open(&admitted, None).await.err(),
        Some(Error::Unavailable)
    );
    let counts:(i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM discussion_attempts),(SELECT count(*) FROM discussion_threads),(SELECT count(*) FROM discussion_revisions)").fetch_one(&pool).await.unwrap();
    assert_eq!(counts, (1, 0, 0));
    sqlx::query("DROP TRIGGER fail_discussion_audit ON audit_events")
        .execute(&pool)
        .await
        .unwrap();
    assert!(!store.complete_open(&admitted, None).await.unwrap().replayed);
}

#[sqlx::test(migrations = "../../migrations")]
async fn service_enforces_scoped_ids_message_versions_and_serialized_expected_versions(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    let store = PgDiscussions::new(pool.clone());
    let first = open(&store, f.owner, f.proposal, proposal_request()).await;
    let left = mutation(
        1,
        ThreadMutation::Reply {
            body: "Left".into(),
            reply_to_id: None,
        },
    );
    let right = mutation(
        1,
        ThreadMutation::Reply {
            body: "Right".into(),
            reply_to_id: None,
        },
    );
    let (a, b) = tokio::join!(
        store.mutate(f.product, f.proposal, first.thread_id, left),
        store.mutate(f.development, f.proposal, first.thread_id, right)
    );
    assert_ne!(a.is_ok(), b.is_ok());
    assert_eq!(a.err().or_else(|| b.err()), Some(Error::Stale));
    assert_eq!(
        store
            .mutate(
                f.owner,
                f.proposal,
                first.thread_id,
                mutation(
                    2,
                    ThreadMutation::Edit {
                        comment_id: first.comment_id.unwrap(),
                        expected_comment_version: 2,
                        body: "Stale message version".into()
                    }
                )
            )
            .await
            .err(),
        Some(Error::Stale)
    );
    let other = fixture(&pool).await;
    assert_eq!(
        store
            .mutate(
                other.owner,
                other.proposal,
                first.thread_id,
                mutation(2, ThreadMutation::Resolve {})
            )
            .await
            .err(),
        Some(Error::NotFound)
    );
    assert_eq!(
        store
            .admit_open(other.owner, f.proposal, proposal_request())
            .await
            .err(),
        Some(Error::NotFound)
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn unicode_comment_limits_match_postgres_and_invalid_text_never_creates_an_admission(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    let store = PgDiscussions::new(pool.clone());
    let mut request = proposal_request();
    request.body = "🦀".repeat(10000);
    let first = open(&store, f.owner, f.proposal, request.clone()).await;
    request.request_id = Uuid::new_v4();
    request.body.push('א');
    assert_eq!(
        store.admit_open(f.owner, f.proposal, request).await.err(),
        Some(Error::Invalid)
    );
    assert_eq!(
        store
            .mutate(
                f.owner,
                f.proposal,
                first.thread_id,
                mutation(
                    1,
                    ThreadMutation::Reply {
                        body: " \u{2003}\r\n".into(),
                        reply_to_id: None
                    }
                )
            )
            .await
            .err(),
        Some(Error::Invalid)
    );
    let lengths: (i32, i32) = sqlx::query_as(
        "SELECT length(body),octet_length(body) FROM discussion_revisions WHERE id=$1",
    )
    .bind(first.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(lengths, (10000, 40000));
    let attempts: i64 = sqlx::query_scalar("SELECT count(*) FROM discussion_attempts")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(attempts, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn organization_admission_quota_applies_across_actors_and_stays_organization_scoped(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    for _ in 0..10 {
        let actor = user(&pool, f.org, "CONTRIBUTOR", vec![]).await;
        sqlx::query("INSERT INTO discussion_attempts(id,organization_id,proposal_id,actor_id,request_id,request_hash) SELECT gen_random_uuid(),$1,$2,$3,gen_random_uuid(),repeat('a',64) FROM generate_series(1,60)")
            .bind(f.org).bind(f.proposal).bind(actor).execute(&pool).await.unwrap();
    }
    let store = PgDiscussions::new(pool.clone());
    assert_eq!(
        store
            .admit_open(f.owner, f.proposal, proposal_request())
            .await
            .err(),
        Some(Error::Quota)
    );
    let other = fixture(&pool).await;
    admission(&store, other.owner, other.proposal, proposal_request()).await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn deleting_a_discussion_message_preserves_the_existing_official_change_request(
    pool: PgPool,
) {
    use specspace_application::{
        official_review::{OfficialReviewRequest, ReviewDecision},
        review_configuration::ReviewArea,
    };
    use specspace_infrastructure::official_reviews::PgOfficialReviews;
    let r = review(&pool).await;
    let ThreadOrigin::Snapshot {
        expected_fingerprint,
        ..
    } = &r.request.origin
    else {
        unreachable!();
    };
    let official = PgOfficialReviews::new(pool.clone())
        .submit(
            r.f.product,
            r.f.proposal,
            r.snapshot,
            OfficialReviewRequest {
                request_id: Uuid::new_v4(),
                expected_fingerprint: expected_fingerprint.clone(),
                domain: ReviewArea::Product,
                decision: ReviewDecision::RequestChanges,
                explanation: "The independent change request remains official".into(),
            },
            &Factory {
                source: r.source.clone(),
                repository: r.f.repository,
            },
        )
        .await
        .unwrap();
    let store = PgDiscussions::new(pool.clone());
    let admitted = admission(&store, r.f.product, r.f.proposal, r.request.clone()).await;
    let proof = r
        .reader
        .load_discussion_anchor(
            r.f.product,
            r.f.proposal,
            r.snapshot,
            &r.file,
            AnchorSide::After,
            TextSelection::Document {},
        )
        .await
        .unwrap();
    let thread = store.complete_open(&admitted, Some(&proof)).await.unwrap();
    store
        .mutate(
            r.f.product,
            r.f.proposal,
            thread.thread_id,
            mutation(
                1,
                ThreadMutation::Tombstone {
                    comment_id: thread.comment_id.unwrap(),
                    expected_comment_version: 1,
                },
            ),
        )
        .await
        .unwrap();
    let retained:(Uuid,String,String)=sqlx::query_as("SELECT x.review_id,r.decision,p.state FROM official_review_effective x JOIN official_reviews r ON r.id=x.review_id JOIN proposals p ON p.id=x.proposal_id WHERE x.proposal_id=$1").bind(r.f.proposal).fetch_one(&pool).await.unwrap();
    assert_eq!(
        retained,
        (
            official.id,
            "REQUEST_CHANGES".into(),
            "CHANGES_REQUESTED".into()
        )
    );
    let blocked:bool=sqlx::query_scalar("SELECT blocking AND NOT resolved FROM discussion_revisions WHERE thread_id=$1 ORDER BY version DESC LIMIT 1").bind(thread.thread_id).fetch_one(&pool).await.unwrap();
    assert!(blocked);
}
