use super::*;
use specspace_application::discussion::read::DiscussionAnchor;

#[sqlx::test(migrations = "../../migrations")]
async fn readers_get_discussion_content_but_capabilities_use_current_thread_policy(pool: PgPool) {
    let f = fixture(&pool).await;
    let store = PgDiscussions::new(pool.clone());
    let mut request = proposal_request();
    request.blocking = true;
    let thread = open(&store, f.product, f.proposal, request).await;
    let reader = user(&pool, f.org, "READER", vec![]).await;
    let page = store.threads(reader, f.proposal, None).await.unwrap();
    assert_eq!(page.actor_id, reader);
    assert_eq!(page.unresolved_blockers, 1);
    assert!(!page.can_comment);
    let permissions = &page.threads[0].permissions;
    assert!(
        !permissions.can_comment
            && !permissions.can_resolve
            && !permissions.can_reopen
            && !permissions.can_toggle_blocking
    );
    let body = store
        .comments(reader, f.proposal, thread.thread_id, None, None)
        .await
        .unwrap();
    assert_eq!(
        body.comments[0].body.as_deref(),
        Some(proposal_request().body.as_str())
    );
    assert!(!body.comments[0].can_edit && !body.comments[0].can_tombstone);
    sqlx::query("UPDATE memberships SET role='ADMIN' WHERE user_id=$1")
        .bind(f.owner)
        .execute(&pool)
        .await
        .unwrap();
    let author = store
        .comments(f.owner, f.proposal, thread.thread_id, None, None)
        .await
        .unwrap();
    assert!(author.thread.permissions.can_comment);
    assert!(
        !author.thread.permissions.can_resolve && !author.thread.permissions.can_toggle_blocking
    );
    assert!(!author.comments[0].can_edit);
    let creator = store
        .comments(f.product, f.proposal, thread.thread_id, None, None)
        .await
        .unwrap();
    assert!(
        creator.thread.permissions.can_resolve && creator.thread.permissions.can_toggle_blocking
    );
    assert!(creator.comments[0].can_edit);
}

#[sqlx::test(migrations = "../../migrations")]
async fn current_comments_hide_old_text_and_tombstones_keep_authorship_and_resolution_identity(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    let store = PgDiscussions::new(pool.clone());
    let thread = open(&store, f.owner, f.proposal, proposal_request()).await;
    store
        .mutate(
            f.product,
            f.proposal,
            thread.thread_id,
            mutation(
                1,
                ThreadMutation::Reply {
                    body: "Independent reply".into(),
                    reply_to_id: thread.comment_id,
                },
            ),
        )
        .await
        .unwrap();
    store
        .mutate(
            f.owner,
            f.proposal,
            thread.thread_id,
            mutation(
                2,
                ThreadMutation::Edit {
                    comment_id: thread.comment_id.unwrap(),
                    expected_comment_version: 1,
                    body: "Replacement text before deletion".into(),
                },
            ),
        )
        .await
        .unwrap();
    let edited = store
        .comments(f.owner, f.proposal, thread.thread_id, None, None)
        .await
        .unwrap();
    assert_eq!(edited.comments[0].version, 2);
    assert!(edited.comments[0].edited_at.is_some());
    assert_eq!(
        edited.comments[0].body.as_deref(),
        Some("Replacement text before deletion")
    );
    assert!(
        !serde_json::to_string(&edited)
            .unwrap()
            .contains("Exact discussion text")
    );
    store
        .mutate(
            f.owner,
            f.proposal,
            thread.thread_id,
            mutation(
                3,
                ThreadMutation::Tombstone {
                    comment_id: thread.comment_id.unwrap(),
                    expected_comment_version: 2,
                },
            ),
        )
        .await
        .unwrap();
    let resolved = store
        .mutate(
            f.product,
            f.proposal,
            thread.thread_id,
            mutation(4, ThreadMutation::Resolve {}),
        )
        .await
        .unwrap();
    store
        .mutate(
            f.owner,
            f.proposal,
            thread.thread_id,
            mutation(
                5,
                ThreadMutation::Reply {
                    body: "A later reply does not become the resolver".into(),
                    reply_to_id: thread.comment_id,
                },
            ),
        )
        .await
        .unwrap();
    let page = store
        .comments(f.owner, f.proposal, thread.thread_id, None, None)
        .await
        .unwrap();
    assert_eq!(page.comments.len(), 3);
    let deleted = &page.comments[0];
    assert_eq!(deleted.author.id, f.owner);
    assert_eq!(deleted.version, 3);
    assert!(
        deleted.body.is_none()
            && deleted.tombstoned_at.is_some()
            && !deleted.can_edit
            && !deleted.can_tombstone
    );
    assert!(
        !serde_json::to_string(&page)
            .unwrap()
            .contains("Replacement text before deletion")
    );
    let resolution = page.thread.resolution.unwrap();
    assert_eq!(resolution.actor.id, f.product);
    assert_eq!(resolution.recorded_at, resolved.recorded_at);
    store
        .mutate(
            f.product,
            f.proposal,
            thread.thread_id,
            mutation(6, ThreadMutation::Reopen {}),
        )
        .await
        .unwrap();
    let reopened = store
        .comments(f.product, f.proposal, thread.thread_id, None, None)
        .await
        .unwrap();
    assert!(reopened.thread.resolution.is_none() && !reopened.thread.resolved);
}

#[sqlx::test(migrations = "../../migrations")]
async fn discussion_pages_use_scoped_stable_identities_and_reject_changed_comment_versions(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    let store = PgDiscussions::new(pool.clone());
    let mut threads = Vec::new();
    for n in 0..22 {
        let mut request = proposal_request();
        request.blocking = n < 2;
        threads.push(open(&store, f.owner, f.proposal, request).await);
    }
    let first = store.threads(f.owner, f.proposal, None).await.unwrap();
    assert_eq!(first.threads.len(), 20);
    assert_eq!(first.unresolved_blockers, 2);
    assert!(first.threads.iter().all(|t| !t.blocking));
    let second = store
        .threads(f.owner, f.proposal, first.next_after)
        .await
        .unwrap();
    assert_eq!(second.threads.len(), 2);
    assert!(second.next_after.is_none());
    assert!(
        second
            .threads
            .iter()
            .all(|t| first.threads.iter().all(|old| old.id != t.id))
    );
    let thread = threads.last().unwrap();
    for version in 1..22 {
        store
            .mutate(
                f.owner,
                f.proposal,
                thread.thread_id,
                mutation(
                    version,
                    ThreadMutation::Reply {
                        body: format!("Reply {version}"),
                        reply_to_id: None,
                    },
                ),
            )
            .await
            .unwrap();
    }
    let comments = store
        .comments(f.owner, f.proposal, thread.thread_id, None, None)
        .await
        .unwrap();
    assert_eq!(comments.comments.len(), 20);
    assert_eq!(comments.thread.version, 22);
    let rest = store
        .comments(
            f.owner,
            f.proposal,
            thread.thread_id,
            comments.next_after,
            Some(22),
        )
        .await
        .unwrap();
    assert_eq!(rest.comments.len(), 2);
    assert!(rest.next_after.is_none());
    assert!(
        rest.comments
            .iter()
            .all(|c| comments.comments.iter().all(|old| old.id != c.id))
    );
    assert_eq!(
        store
            .comments(
                f.owner,
                f.proposal,
                thread.thread_id,
                comments.next_after,
                None
            )
            .await
            .err(),
        Some(Error::Invalid)
    );
    assert_eq!(
        store
            .comments(
                f.owner,
                f.proposal,
                thread.thread_id,
                threads[0].comment_id,
                Some(22)
            )
            .await
            .err(),
        Some(Error::Invalid)
    );
    store
        .mutate(
            f.owner,
            f.proposal,
            thread.thread_id,
            mutation(
                22,
                ThreadMutation::Edit {
                    comment_id: thread.comment_id.unwrap(),
                    expected_comment_version: 1,
                    body: "Changed while paging".into(),
                },
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .comments(
                f.owner,
                f.proposal,
                thread.thread_id,
                comments.next_after,
                Some(22)
            )
            .await
            .err(),
        Some(Error::Stale)
    );
    let other = fixture(&pool).await;
    let foreign = open(&store, other.owner, other.proposal, proposal_request()).await;
    assert_eq!(
        store
            .threads(f.owner, f.proposal, Some(foreign.thread_id))
            .await
            .err(),
        Some(Error::Invalid)
    );
    assert_eq!(
        store
            .comments(other.owner, other.proposal, thread.thread_id, None, None)
            .await
            .err(),
        Some(Error::NotFound)
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn historical_anchor_reads_keep_old_context_and_blockers_without_provider_reads(
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
    let thread = store
        .complete_open(&admitted, Some(DiscussionSourceProof::File(&proof)))
        .await
        .unwrap();
    let reads = r.source.reads.load(Ordering::SeqCst);
    r.source.target.store(3, Ordering::SeqCst);
    let initial = store
        .comments(r.f.owner, r.f.proposal, thread.thread_id, None, None)
        .await
        .unwrap();
    assert!(!initial.thread.outdated_context);
    sqlx::query("UPDATE proposals SET title='Metadata-only edit',version=version+1 WHERE id=$1")
        .bind(r.f.proposal)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        !store
            .comments(r.f.owner, r.f.proposal, thread.thread_id, None, None)
            .await
            .unwrap()
            .thread
            .outdated_context
    );
    sqlx::query("UPDATE proposals SET base_sha=$2,version=version+1 WHERE id=$1")
        .bind(r.f.proposal)
        .bind(sha(2).as_str())
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        store
            .comments(r.f.owner, r.f.proposal, thread.thread_id, None, None)
            .await
            .unwrap()
            .thread
            .outdated_context
    );
    sqlx::query("UPDATE proposals SET head_sha=$2,version=version+1 WHERE id=$1")
        .bind(r.f.proposal)
        .bind(sha(2).as_str())
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE repositories SET remote_id='99' WHERE id=$1")
        .bind(r.f.repository)
        .execute(&pool)
        .await
        .unwrap();
    let page = store
        .comments(r.f.owner, r.f.proposal, thread.thread_id, None, None)
        .await
        .unwrap();
    assert!(page.thread.outdated_context && page.thread.repository_binding_changed);
    assert_eq!(page.thread.origin.head_sha, sha(1).as_str());
    assert_eq!(page.thread.origin.snapshot_id, Some(r.snapshot));
    let DiscussionAnchor::File { text, source, .. } = page.thread.origin.anchor else {
        panic!("stored file anchor");
    };
    assert_eq!(text.source_sha256, proof.text().source_sha256());
    assert_eq!(source.path, "docs/a.md");
    assert_eq!(
        store
            .threads(r.f.owner, r.f.proposal, None)
            .await
            .unwrap()
            .unresolved_blockers,
        1
    );
    assert_eq!(r.source.reads.load(Ordering::SeqCst), reads);
}

#[sqlx::test(migrations = "../../migrations")]
async fn current_membership_and_target_configuration_control_reads_and_action_hints(pool: PgPool) {
    let f = fixture(&pool).await;
    let store = PgDiscussions::new(pool.clone());
    let thread = open(&store, f.owner, f.proposal, proposal_request()).await;
    let other = fixture(&pool).await;
    assert_eq!(
        store.threads(other.owner, f.proposal, None).await.err(),
        Some(Error::NotFound)
    );
    sqlx::query("UPDATE repositories SET default_branch='trunk' WHERE id=$1")
        .bind(f.repository)
        .execute(&pool)
        .await
        .unwrap();
    let page = store
        .comments(f.owner, f.proposal, thread.thread_id, None, None)
        .await
        .unwrap();
    assert!(!page.thread.permissions.can_comment && !page.comments[0].can_edit);
    assert!(page.thread.repository_binding_changed && page.thread.outdated_context);
    sqlx::query("UPDATE users SET status='DISABLED' WHERE id=$1")
        .bind(f.owner)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        store
            .comments(f.owner, f.proposal, thread.thread_id, None, None)
            .await
            .err(),
        Some(Error::NotFound)
    );
}
