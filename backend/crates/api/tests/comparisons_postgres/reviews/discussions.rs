use super::*;
use specspace_application::discussion::{
    DiscussionRecorded, OpenThreadRequest, ThreadMutation, ThreadMutationRequest, ThreadOrigin,
};
use specspace_infrastructure::discussions::{OpenAdmissionResult, PgDiscussions};

async fn saved(pool: &PgPool, f: &Fixture) -> DiscussionRecorded {
    let store = PgDiscussions::new(pool.clone());
    let admitted = store
        .admit_open(
            f.owner.id,
            f.proposal,
            OpenThreadRequest {
                request_id: Uuid::new_v4(),
                origin: ThreadOrigin::Proposal {
                    expected_proposal_version: 2,
                },
                blocking: true,
                body: "Plain text <script>example</script>\r\nשָׁלוֹם 🦀".into(),
            },
        )
        .await
        .unwrap();
    let OpenAdmissionResult::Ready(admitted) = admitted else {
        panic!("new discussion");
    };
    store.complete_open(&admitted, None).await.unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
async fn discussion_http_reads_are_member_scoped_plain_current_and_provider_free(pool: PgPool) {
    let (f, _, _) = configured(&pool, false).await;
    let thread = saved(&pool, &f).await;
    let list = format!("/api/v1/proposals/{}/threads", f.proposal);
    let detail = format!("{list}/{}", thread.thread_id);
    let reads = (
        f.provider.reads.load(Ordering::SeqCst),
        f.provider.ref_reads.load(Ordering::SeqCst),
    );
    let page = get(&f.app, &f.actor, &list, true).await;
    assert_eq!(page.0, StatusCode::OK);
    assert_eq!(page.1["actor_id"], f.actor.id.to_string());
    assert_eq!(page.1["unresolved_blockers"], 1);
    assert_eq!(page.1["can_comment"], false);
    let comments = get(&f.app, &f.actor, &detail, true).await;
    assert_eq!(comments.0, StatusCode::OK);
    assert_eq!(
        comments.1["comments"][0]["body"],
        "Plain text <script>example</script>\r\nשָׁלוֹם 🦀"
    );
    assert_eq!(comments.1["comments"][0]["can_edit"], false);
    PgDiscussions::new(pool.clone())
        .mutate(
            f.owner.id,
            f.proposal,
            thread.thread_id,
            ThreadMutationRequest {
                request_id: Uuid::new_v4(),
                expected_thread_version: 1,
                mutation: ThreadMutation::Tombstone {
                    comment_id: thread.comment_id.unwrap(),
                    expected_comment_version: 1,
                },
            },
        )
        .await
        .unwrap();
    let deleted = get(&f.app, &f.actor, &detail, true).await;
    assert!(deleted.1["comments"][0]["body"].is_null());
    assert!(deleted.1["comments"][0]["tombstoned_at"].is_string());
    assert_eq!(deleted.1["thread"]["blocking"], true);
    assert_eq!(deleted.1["thread"]["resolved"], false);
    assert!(!deleted.1.to_string().contains("<script>"));
    assert_eq!(
        (
            f.provider.reads.load(Ordering::SeqCst),
            f.provider.ref_reads.load(Ordering::SeqCst)
        ),
        reads
    );
    sqlx::query("DELETE FROM memberships WHERE organization_id=$1 AND user_id=$2")
        .bind(f.organization)
        .bind(f.actor.id)
        .execute(&pool)
        .await
        .unwrap();
    let revoked = get(&f.app, &f.actor, &detail, true).await;
    assert_eq!(revoked.0, StatusCode::UNAUTHORIZED);
    assert!(revoked.1.get("comments").is_none());
}

#[sqlx::test(migrations = "../../migrations")]
async fn discussion_http_auth_actor_queries_cursors_and_scope_are_strict(pool: PgPool) {
    let (f, _, _) = configured(&pool, false).await;
    let thread = saved(&pool, &f).await;
    let list = format!("/api/v1/proposals/{}/threads", f.proposal);
    let detail = format!("{list}/{}", thread.thread_id);
    let response = f
        .app
        .clone()
        .oneshot(Request::builder().uri(&list).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(
        get(&f.app, &f.actor, &list, false).await.0,
        StatusCode::CONFLICT
    );
    for query in [
        "?unknown=1".to_owned(),
        "?after=not-a-uuid".into(),
        format!("?after={}", Uuid::new_v4()),
    ] {
        assert_eq!(
            get(&f.app, &f.actor, &format!("{list}{query}"), true)
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        get(
            &f.app,
            &f.actor,
            &format!("{detail}?after={}", thread.comment_id.unwrap()),
            true
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let stale = get(
        &f.app,
        &f.actor,
        &format!("{detail}?expected_thread_version=2"),
        true,
    )
    .await;
    assert_eq!(stale.0, StatusCode::CONFLICT);
    assert_eq!(stale.1["error"]["code"], "DISCUSSION_CONTEXT_CHANGED");
    let other = fixture(&pool, true).await;
    assert_eq!(
        get(&f.app, &other.owner, &detail, true).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        get(
            &f.app,
            &f.actor,
            &format!("{list}/{}", Uuid::new_v4()),
            true
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}
