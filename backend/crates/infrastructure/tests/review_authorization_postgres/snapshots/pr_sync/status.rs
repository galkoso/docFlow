use super::*;

#[sqlx::test(migrations = "../../migrations")]
async fn transport_wait_and_stale_skip_do_not_claim_provider_work(pool: PgPool) {
    let (f, _, published, _) = publication(&pool).await;
    let reader = user(&pool, f.org, "READER", vec![]).await;
    let store = Arc::new(PgPrSync::new(pool.clone()));
    let dispatcher =
        specspace_infrastructure::pr_sync::PrSyncDispatcher::new(store.clone(), site());
    sqlx::query("UPDATE users SET status='DISABLED' WHERE id=$1")
        .bind(f.owner)
        .execute(&pool)
        .await
        .unwrap();
    dispatcher.run_once().await.unwrap();
    let waiting = store
        .status(reader, f.proposal, published.snapshot_id)
        .await
        .unwrap();
    assert_eq!(waiting.dispatch.phase, D::Waiting);
    assert_eq!(waiting.dispatch.reason, Some(R::AuthorityRequired));
    assert!(
        waiting.dispatch.next_attempt_at.is_some()
            && waiting.sync.is_none()
            && waiting.pr.is_none()
    );
    sqlx::query("UPDATE users SET status='ACTIVE' WHERE id=$1")
        .bind(f.owner)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE proposals SET state='CLOSED' WHERE id=$1")
        .bind(f.proposal)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE pr_sync_dispatch SET next_at=clock_timestamp()-interval '1 second',attempts=attempts+1,updated_at=clock_timestamp() WHERE proposal_id=$1").bind(f.proposal).execute(&pool).await.unwrap();
    dispatcher.run_once().await.unwrap();
    let skipped = store
        .status(reader, f.proposal, published.snapshot_id)
        .await
        .unwrap();
    assert_eq!(skipped.dispatch.phase, D::Skipped);
    assert_eq!(skipped.dispatch.reason, Some(R::SourceChanged));
    assert!(
        skipped.dispatch.next_attempt_at.is_none()
            && skipped.sync.is_none()
            && skipped.pr.is_none()
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn historical_snapshot_exposes_latest_proposal_observation_without_claiming_current_sync(
    pool: PgPool,
) {
    let (f, _, preparer) = setup(&pool).await;
    let snapshots = PgReviewSnapshots::new(pool.clone());
    let store = PgPrSync::new(pool.clone());
    let (lease, verified) = prepared(&pool, &f, &preparer, 2).await;
    let first = snapshots
        .publish(&lease, &verified, &preparer)
        .await
        .unwrap();
    let event: Uuid = sqlx::query_scalar(
        "SELECT id FROM outbox WHERE entity_id=$1 AND event_type='REVIEW_SNAPSHOT_PUBLISHED'",
    )
    .bind(f.proposal)
    .fetch_one(&pool)
    .await
    .unwrap();
    store
        .reserve(f.owner, f.proposal, event, &site())
        .await
        .unwrap();
    let first_lease = store.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    store.start_write(&first_lease, &[]).await.unwrap();
    let old_pr = observed(&first_lease);
    store.complete(&first_lease, &old_pr).await.unwrap();
    sqlx::query("UPDATE proposals SET description='Another reviewed description',version=version+1 WHERE id=$1").bind(f.proposal).execute(&pool).await.unwrap();
    let (lease, verified) = prepared(&pool, &f, &preparer, 4).await;
    let second = snapshots
        .publish(&lease, &verified, &preparer)
        .await
        .unwrap();
    assert_ne!(first.snapshot_id, second.snapshot_id);
    let event2 = sqlx::query_scalar("SELECT id FROM outbox WHERE entity_id=$1 AND event_type='REVIEW_SNAPSHOT_PUBLISHED' AND id<>$2").bind(f.proposal).bind(event).fetch_one(&pool).await.unwrap();
    store
        .reserve(f.owner, f.proposal, event2, &site())
        .await
        .unwrap();
    let second_lease = store.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    store.start_write(&second_lease, &[old_pr]).await.unwrap();
    store
        .defer(&second_lease, "PR_RESULT_UNKNOWN", Some(1))
        .await
        .unwrap();
    let uncertain = store
        .status(f.owner, f.proposal, first.snapshot_id)
        .await
        .unwrap();
    assert!(uncertain.uncertain_write);
    assert_eq!(uncertain.sync.unwrap().phase, S::Superseded);
    due(&pool, &second_lease).await;
    let resumed = store.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    store.complete(&resumed, &observed(&resumed)).await.unwrap();
    let historical = store
        .status(f.owner, f.proposal, first.snapshot_id)
        .await
        .unwrap();
    assert_eq!(historical.sync.unwrap().phase, S::Superseded);
    assert_eq!(historical.pr.unwrap().snapshot_id, second.snapshot_id);
    let current = store
        .status(f.owner, f.proposal, second.snapshot_id)
        .await
        .unwrap();
    assert_eq!(current.sync.unwrap().phase, S::ObservedSynced);
    assert!(!current.uncertain_write);
    let pr = current.pr.unwrap();
    assert_eq!(pr.number, "9");
    assert!(pr.recorded_at <= current.read_at);
}
use specspace_application::review_pr_status::{
    ReviewPrDispatchPhase as D, ReviewPrReason as R, ReviewPrSyncPhase as S,
};

#[sqlx::test(migrations = "../../migrations")]
async fn shared_status_is_read_only_and_separates_transport_work_and_observation(pool: PgPool) {
    let (f, source, published, event) = publication(&pool).await;
    let store = PgPrSync::new(pool.clone());
    let reader = user(&pool, f.org, "READER", vec![]).await;
    let reads = source.reads.load(Ordering::SeqCst);
    let first = store
        .status(reader, f.proposal, published.snapshot_id)
        .await
        .unwrap();
    assert_eq!(first.dispatch.phase, D::Awaiting);
    assert!(first.sync.is_none() && first.pr.is_none());
    assert!(first.current_for_sync && first.binding_current && !first.uncertain_write);
    assert_eq!(counts(&pool).await, (0, 0, 0, 0));
    store
        .reserve(f.owner, f.proposal, event, &site())
        .await
        .unwrap();
    let queued = store
        .status(reader, f.proposal, published.snapshot_id)
        .await
        .unwrap();
    assert_eq!(queued.dispatch.phase, D::Enqueued);
    assert!(queued.dispatch.reason.is_none());
    assert_eq!(queued.sync.as_ref().unwrap().phase, S::Queued);
    assert!(queued.sync.unwrap().automatic);
    assert!(queued.pr.is_none());
    let lease = store.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    assert_eq!(
        store
            .status(reader, f.proposal, published.snapshot_id)
            .await
            .unwrap()
            .sync
            .unwrap()
            .phase,
        S::Observing
    );
    store.start_write(&lease, &[]).await.unwrap();
    assert!(
        store
            .status(reader, f.proposal, published.snapshot_id)
            .await
            .unwrap()
            .uncertain_write
    );
    store.complete(&lease, &observed(&lease)).await.unwrap();
    let ready = store
        .status(reader, f.proposal, published.snapshot_id)
        .await
        .unwrap();
    assert_eq!(ready.sync.as_ref().unwrap().phase, S::ObservedSynced);
    assert!(!ready.sync.as_ref().unwrap().automatic && !ready.uncertain_write);
    let pr = ready.pr.as_ref().unwrap();
    assert_eq!(pr.number, "9");
    assert_eq!(pr.snapshot_id, published.snapshot_id);
    assert!(!pr.superseded);
    assert!(pr.recorded_at <= ready.read_at);
    let json = serde_json::to_string(&ready).unwrap();
    for secret in [
        lease.intent().content().body().to_owned(),
        lease.operation_id().to_string(),
        lease.job_id().to_string(),
    ] {
        assert!(!json.contains(&secret));
    }
    assert_eq!(source.reads.load(Ordering::SeqCst), reads);
}

#[sqlx::test(migrations = "../../migrations")]
async fn status_distinguishes_retry_terminal_failure_and_unresolved_started_writes(pool: PgPool) {
    for started in [false, true] {
        let (f, store, lease) = job(&pool).await;
        let snapshot = lease.intent().source().snapshot;
        if started {
            store.start_write(&lease, &[]).await.unwrap();
        }
        store
            .defer(&lease, "PR_PROVIDER_UNAVAILABLE", Some(60))
            .await
            .unwrap();
        let pending = store.status(f.owner, f.proposal, snapshot).await.unwrap();
        assert_eq!(pending.uncertain_write, started);
        let work = pending.sync.unwrap();
        assert_eq!(
            work.phase,
            if started {
                S::Reconciling
            } else {
                S::RetryWait
            }
        );
        assert_eq!(work.reason, Some(R::ProviderUnavailable));
        assert!(work.automatic && work.next_attempt_at.is_some());
        due(&pool, &lease).await;
        let next = store.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
        store.defer(&next, "PRIVATE_SENTINEL", None).await.unwrap();
        let stopped = store.status(f.owner, f.proposal, snapshot).await.unwrap();
        assert_eq!(stopped.uncertain_write, started);
        let work = stopped.sync.as_ref().unwrap();
        assert_eq!(work.phase, if started { S::Reconciling } else { S::Failed });
        assert!(!work.automatic && work.next_attempt_at.is_none());
        assert_eq!(work.reason, Some(R::Other));
        assert!(
            !serde_json::to_string(&stopped)
                .unwrap()
                .contains("PRIVATE_SENTINEL")
        );
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn stored_success_is_superseded_after_closure_and_hidden_after_binding_changes(pool: PgPool) {
    let (f, store, lease) = job(&pool).await;
    let snapshot = lease.intent().source().snapshot;
    store.start_write(&lease, &[]).await.unwrap();
    store.complete(&lease, &observed(&lease)).await.unwrap();
    sqlx::query("UPDATE proposals SET state='CLOSED' WHERE id=$1")
        .bind(f.proposal)
        .execute(&pool)
        .await
        .unwrap();
    let closed = store.status(f.owner, f.proposal, snapshot).await.unwrap();
    assert!(!closed.current_for_sync && closed.binding_current && closed.pr.is_some());
    assert_eq!(closed.sync.unwrap().phase, S::Superseded);
    sqlx::query("UPDATE repositories SET installation_id='8' WHERE id=$1")
        .bind(f.repository)
        .execute(&pool)
        .await
        .unwrap();
    let redirected = store.status(f.owner, f.proposal, snapshot).await.unwrap();
    assert!(!redirected.current_for_sync && !redirected.binding_current && redirected.pr.is_none());
    assert_eq!(redirected.sync.unwrap().reason, Some(R::BindingChanged));
}

#[sqlx::test(migrations = "../../migrations")]
async fn status_requires_current_organization_access_and_exact_snapshot_scope(pool: PgPool) {
    let (f, store, lease) = job(&pool).await;
    let snapshot = lease.intent().source().snapshot;
    let foreign = fixture(&pool).await;
    for (actor, proposal, selected) in [
        (foreign.owner, f.proposal, snapshot),
        (f.owner, f.proposal, Uuid::new_v4()),
        (f.owner, foreign.proposal, snapshot),
    ] {
        assert!(matches!(
            store.status(actor, proposal, selected).await,
            Err(PrSyncStoreError::Access(SubmissionError::NotFound))
        ));
    }
    let reader = user(&pool, f.org, "READER", vec![]).await;
    store.status(reader, f.proposal, snapshot).await.unwrap();
    sqlx::query("DELETE FROM memberships WHERE organization_id=$1 AND user_id=$2")
        .bind(f.org)
        .bind(reader)
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        store.status(reader, f.proposal, snapshot).await,
        Err(PrSyncStoreError::Access(SubmissionError::NotFound))
    ));
}

#[sqlx::test(migrations = "../../migrations")]
async fn resubmission_of_same_snapshot_reports_latest_publication_transport(pool: PgPool) {
    let (f, _, preparer) = setup(&pool).await;
    let (lease, verified) = prepared(&pool, &f, &preparer, 2).await;
    let snapshots = PgReviewSnapshots::new(pool.clone());
    let first = snapshots
        .publish(&lease, &verified, &preparer)
        .await
        .unwrap();
    let event = sqlx::query_scalar(
        "SELECT id FROM outbox WHERE entity_id=$1 AND event_type='REVIEW_SNAPSHOT_PUBLISHED'",
    )
    .bind(f.proposal)
    .fetch_one(&pool)
    .await
    .unwrap();
    let store = PgPrSync::new(pool.clone());
    store
        .reserve(f.owner, f.proposal, event, &site())
        .await
        .unwrap();
    let (lease, verified) = prepared(&pool, &f, &preparer, 3).await;
    let second = snapshots
        .publish(&lease, &verified, &preparer)
        .await
        .unwrap();
    assert_eq!(first.snapshot_id, second.snapshot_id);
    let latest = store
        .status(f.owner, f.proposal, second.snapshot_id)
        .await
        .unwrap();
    assert_eq!(latest.dispatch.phase, D::Awaiting);
    assert!(latest.sync.is_none());
}
