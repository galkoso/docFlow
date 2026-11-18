use super::*;
use specspace_infrastructure::pr_sync::{PrDispatchWork, PrSyncDispatcher};
fn dispatcher(pool: &PgPool) -> PrSyncDispatcher {
    PrSyncDispatcher::new(Arc::new(PgPrSync::new(pool.clone())), site())
}
async fn due(pool: &PgPool, event: Uuid) {
    sqlx::query("UPDATE pr_sync_dispatch SET next_at=clock_timestamp()-interval '1 second',attempts=attempts+1,updated_at=clock_timestamp() WHERE event_id=$1 AND state='WAITING'").bind(event).execute(pool).await.unwrap();
}
async fn dispatch_state(pool: &PgPool, event: Uuid) -> (String, String) {
    sqlx::query_as("SELECT state,reason FROM pr_sync_dispatch WHERE event_id=$1")
        .bind(event)
        .fetch_one(pool)
        .await
        .unwrap()
}
async fn delivered(pool: &PgPool, event: Uuid) -> bool {
    sqlx::query_scalar("SELECT sent_at IS NOT NULL FROM outbox WHERE id=$1")
        .bind(event)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
async fn valid_publication_is_delivered_once_into_the_exact_original_actor_intent(pool: PgPool) {
    let (f, source, published, event) = publication(&pool).await;
    let reads = source.reads.load(Ordering::SeqCst);
    let PrDispatchWork::Enqueued(returned, job) = dispatcher(&pool).run_once().await.unwrap()
    else {
        panic!("expected queued publication");
    };
    assert_eq!(returned, event);
    assert_eq!(
        dispatch_state(&pool, event).await,
        ("ENQUEUED".into(), "QUEUED".into())
    );
    let replay = PgPrSync::new(pool.clone())
        .reserve(f.owner, f.proposal, event, &site())
        .await
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.job_id, job);
    assert_eq!(replay.intent().source().actor, f.owner);
    assert_eq!(replay.intent().source().snapshot, published.snapshot_id);
    assert_eq!(replay.intent().source().fingerprint, published.fingerprint);
    assert_eq!(source.reads.load(Ordering::SeqCst), reads);
    assert_eq!(counts(&pool).await, (1, 1, 1, 1));
    assert_eq!(
        dispatcher(&pool).run_once().await.unwrap(),
        PrDispatchWork::Idle
    );
    let attempts: i32 =
        sqlx::query_scalar("SELECT attempts FROM pr_sync_dispatch WHERE event_id=$1")
            .bind(event)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(attempts, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn concurrent_dispatchers_cannot_duplicate_transport_or_operation(pool: PgPool) {
    let (_, _, _, event) = publication(&pool).await;
    let a = dispatcher(&pool);
    let b = dispatcher(&pool);
    let (left, right) = tokio::join!(a.run_once(), b.run_once());
    let results = [left.unwrap(), right.unwrap()];
    assert_eq!(
        results
            .iter()
            .filter(|r| matches!(r,PrDispatchWork::Enqueued(e,_) if *e==event))
            .count(),
        1
    );
    assert_eq!(counts(&pool).await, (1, 1, 1, 1));
}

#[sqlx::test(migrations = "../../migrations")]
async fn changed_closed_or_redirected_snapshots_are_audited_and_skipped_without_new_jobs(
    pool: PgPool,
) {
    for mode in 0..5 {
        let (f, _, _, event) = publication(&pool).await;
        match mode {
            0 => {
                sqlx::query(
                    "UPDATE proposals SET head_sha=repeat('9',40),version=version+1 WHERE id=$1",
                )
                .bind(f.proposal)
                .execute(&pool)
                .await
                .unwrap();
            }
            1 => {
                sqlx::query("UPDATE proposals SET state='CLOSED' WHERE id=$1")
                    .bind(f.proposal)
                    .execute(&pool)
                    .await
                    .unwrap();
            }
            2 => {
                sqlx::query("UPDATE organizations SET policy_version=policy_version+1 WHERE id=$1")
                    .bind(f.org)
                    .execute(&pool)
                    .await
                    .unwrap();
            }
            3 => {
                sqlx::query("UPDATE repositories SET installation_id='8' WHERE id=$1")
                    .bind(f.repository)
                    .execute(&pool)
                    .await
                    .unwrap();
            }
            _ => {
                sqlx::query("UPDATE proposals SET description='Changed saved metadata',version=version+1 WHERE id=$1").bind(f.proposal).execute(&pool).await.unwrap();
            }
        }
        let before: (String, i64, Option<Uuid>) = sqlx::query_as(
            "SELECT state,version,active_review_snapshot_id FROM proposals WHERE id=$1",
        )
        .bind(f.proposal)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            dispatcher(&pool).run_once().await.unwrap(),
            PrDispatchWork::Skipped(event)
        );
        let after: (String, i64, Option<Uuid>) = sqlx::query_as(
            "SELECT state,version,active_review_snapshot_id FROM proposals WHERE id=$1",
        )
        .bind(f.proposal)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(before, after);
        assert!(delivered(&pool, event).await);
    }
    assert_eq!(counts(&pool).await, (0, 0, 0, 5));
}

#[sqlx::test(migrations = "../../migrations")]
async fn malformed_noncanonical_or_oversized_publication_payloads_are_quarantined(pool: PgPool) {
    for mode in 0..6 {
        let (f, _, _, event) = publication(&pool).await;
        let mut body: serde_json::Value =
            sqlx::query_scalar("SELECT payload FROM outbox WHERE id=$1")
                .bind(event)
                .fetch_one(&pool)
                .await
                .unwrap();
        match mode {
            0 => body["approvals_granted"] = json!(true),
            1 => body["pr_sync_required"] = json!(false),
            2 => body["operation_id"] = json!(Uuid::new_v4()),
            3 => {
                body["operation_id"] =
                    json!(body["operation_id"].as_str().unwrap().replace('-', ""))
            }
            4 => body["unexpected"] = json!(true),
            _ => body["padding"] = json!("x".repeat(20000)),
        }
        sqlx::query("UPDATE outbox SET payload=$2 WHERE id=$1")
            .bind(event)
            .bind(body)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            dispatcher(&pool).run_once().await.unwrap(),
            PrDispatchWork::Rejected(event)
        );
        assert_eq!(
            dispatch_state(&pool, event).await,
            ("REJECTED".into(), "PUBLICATION_INVALID".into())
        );
        let snapshots: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM review_snapshots WHERE proposal_id=$1 AND state='READY'",
        )
        .bind(f.proposal)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(snapshots, 1);
    }
    assert_eq!(counts(&pool).await, (0, 0, 0, 6));
}

#[sqlx::test(migrations = "../../migrations")]
async fn unrelated_events_are_untouched_and_orphan_publications_do_not_block_later_work(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    let unrelated = Uuid::new_v4();
    let bad = Uuid::new_v4();
    sqlx::query("INSERT INTO outbox(id,organization_id,event_type,entity_id,created_at) VALUES($1,$2,'UNRELATED_EVENT',$3,clock_timestamp()-interval '1 hour')").bind(unrelated).bind(f.org).bind(f.proposal).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO outbox(id,organization_id,event_type,entity_id,created_at) VALUES($1,$2,'REVIEW_SNAPSHOT_PUBLISHED',$3,clock_timestamp()-interval '1 hour')").bind(bad).bind(f.org).bind(Uuid::new_v4()).execute(&pool).await.unwrap();
    let (_, _, _, valid) = publication(&pool).await;
    assert_eq!(
        dispatcher(&pool).run_once().await.unwrap(),
        PrDispatchWork::Rejected(bad)
    );
    assert!(
        matches!(dispatcher(&pool).run_once().await.unwrap(),PrDispatchWork::Enqueued(e,_) if e==valid)
    );
    assert!(!delivered(&pool, unrelated).await);
    assert_eq!(
        dispatcher(&pool).run_once().await.unwrap(),
        PrDispatchWork::Idle
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn revoked_original_actor_waits_durably_and_resumes_only_after_current_access_returns(
    pool: PgPool,
) {
    let (f, _, _, event) = publication(&pool).await;
    sqlx::query("UPDATE users SET status='DISABLED' WHERE id=$1")
        .bind(f.owner)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        dispatcher(&pool).run_once().await.unwrap(),
        PrDispatchWork::Deferred(event)
    );
    assert_eq!(
        dispatch_state(&pool, event).await,
        ("WAITING".into(), "AUTHORITY_REQUIRED".into())
    );
    assert_eq!(counts(&pool).await, (0, 0, 0, 0));
    assert_eq!(
        dispatcher(&pool).run_once().await.unwrap(),
        PrDispatchWork::Idle
    );
    sqlx::query("UPDATE users SET status='ACTIVE' WHERE id=$1")
        .bind(f.owner)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        dispatcher(&pool).run_once().await.unwrap(),
        PrDispatchWork::Idle
    );
    due(&pool, event).await;
    assert!(
        matches!(dispatcher(&pool).run_once().await.unwrap(),PrDispatchWork::Enqueued(e,_) if e==event)
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn manual_admission_and_dispatch_share_the_same_atomic_transport_record(pool: PgPool) {
    let (f, _, _, event) = publication(&pool).await;
    sqlx::query("UPDATE users SET status='DISABLED' WHERE id=$1")
        .bind(f.owner)
        .execute(&pool)
        .await
        .unwrap();
    dispatcher(&pool).run_once().await.unwrap();
    sqlx::query("UPDATE users SET status='ACTIVE' WHERE id=$1")
        .bind(f.owner)
        .execute(&pool)
        .await
        .unwrap();
    let receipt = PgPrSync::new(pool.clone())
        .reserve(f.owner, f.proposal, event, &site())
        .await
        .unwrap();
    assert!(!receipt.replayed);
    assert_eq!(dispatch_state(&pool, event).await.0, "ENQUEUED");
    assert_eq!(
        dispatcher(&pool).run_once().await.unwrap(),
        PrDispatchWork::Idle
    );
    assert_eq!(counts(&pool).await, (1, 1, 1, 1));
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_busy_pr_operation_defers_the_next_projection_and_retries_after_safe_release(
    pool: PgPool,
) {
    let (f, _, preparer) = setup(&pool).await;
    let (lease, verified) = prepared(&pool, &f, &preparer, 2).await;
    PgReviewSnapshots::new(pool.clone())
        .publish(&lease, &verified, &preparer)
        .await
        .unwrap();
    let first: Uuid = sqlx::query_scalar(
        "SELECT id FROM outbox WHERE entity_id=$1 AND event_type='REVIEW_SNAPSHOT_PUBLISHED'",
    )
    .bind(f.proposal)
    .fetch_one(&pool)
    .await
    .unwrap();
    let store = PgPrSync::new(pool.clone());
    store
        .reserve(f.owner, f.proposal, first, &site())
        .await
        .unwrap();
    let (lease, verified) = prepared(&pool, &f, &preparer, 3).await;
    PgReviewSnapshots::new(pool.clone())
        .publish(&lease, &verified, &preparer)
        .await
        .unwrap();
    let next:Uuid=sqlx::query_scalar("SELECT id FROM outbox WHERE entity_id=$1 AND event_type='REVIEW_SNAPSHOT_PUBLISHED' AND id<>$2").bind(f.proposal).bind(first).fetch_one(&pool).await.unwrap();
    assert_eq!(
        dispatcher(&pool).run_once().await.unwrap(),
        PrDispatchWork::Deferred(next)
    );
    assert_eq!(dispatch_state(&pool, next).await.1, "PR_SYNC_BUSY");
    let lease = store.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    store
        .defer(&lease, "PREFLIGHT_STOPPED", None)
        .await
        .unwrap();
    due(&pool, next).await;
    assert!(
        matches!(dispatcher(&pool).run_once().await.unwrap(),PrDispatchWork::Enqueued(e,_) if e==next)
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn quotas_schedule_delivery_without_losing_the_publication_or_blocking_another_organization(
    pool: PgPool,
) {
    let (f, source, _, event) = publication(&pool).await;
    let store = PgPrSync::new(pool.clone());
    store
        .reserve(f.owner, f.proposal, event, &site())
        .await
        .unwrap();
    let (second, event2) = sibling_publication(&pool, &f, source.clone()).await;
    store
        .reserve(second.owner, second.proposal, event2, &site())
        .await
        .unwrap();
    let (_, third) = sibling_publication(&pool, &f, source).await;
    assert_eq!(
        dispatcher(&pool).run_once().await.unwrap(),
        PrDispatchWork::Deferred(third)
    );
    assert_eq!(dispatch_state(&pool, third).await.1, "ADMISSION_QUOTA");
    let (_, _, _, other) = publication(&pool).await;
    assert!(
        matches!(dispatcher(&pool).run_once().await.unwrap(),PrDispatchWork::Enqueued(e,_) if e==other)
    );
    assert!(!delivered(&pool, third).await);
}

#[sqlx::test(migrations = "../../migrations")]
async fn duplicate_backed_publication_reuses_existing_delivery_even_after_actor_revocation(
    pool: PgPool,
) {
    let (f, _, _, event) = publication(&pool).await;
    let old = PgPrSync::new(pool.clone())
        .reserve(f.owner, f.proposal, event, &site())
        .await
        .unwrap();
    let duplicate = Uuid::new_v4();
    sqlx::query("INSERT INTO outbox(id,organization_id,event_type,entity_id,payload) SELECT $1,organization_id,event_type,entity_id,payload FROM outbox WHERE id=$2").bind(duplicate).bind(event).execute(&pool).await.unwrap();
    sqlx::query("UPDATE users SET status='DISABLED' WHERE id=$1")
        .bind(f.owner)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        dispatcher(&pool).run_once().await.unwrap(),
        PrDispatchWork::Enqueued(duplicate, old.job_id)
    );
    assert_eq!(counts(&pool).await, (1, 1, 1, 2));
}

#[sqlx::test(migrations = "../../migrations")]
async fn dispatch_audit_failure_rolls_back_job_intent_operation_and_delivery_together(
    pool: PgPool,
) {
    let (_, _, _, event) = publication(&pool).await;
    sqlx::query("CREATE FUNCTION reject_dispatch_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='PR_OUTBOX_DISPATCH' THEN RAISE EXCEPTION 'synthetic dispatch audit failure'; END IF; RETURN NEW; END $$").execute(&pool).await.unwrap();
    sqlx::query("CREATE TRIGGER reject_dispatch_audit BEFORE INSERT ON audit_events FOR EACH ROW EXECUTE FUNCTION reject_dispatch_audit()").execute(&pool).await.unwrap();
    assert!(matches!(
        dispatcher(&pool).run_once().await,
        Err(PrSyncStoreError::Unavailable)
    ));
    assert_eq!(counts(&pool).await, (0, 0, 0, 0));
    let dispatches: i64 = sqlx::query_scalar("SELECT count(*) FROM pr_sync_dispatch")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(dispatches, 0);
    sqlx::query("DROP TRIGGER reject_dispatch_audit ON audit_events")
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        matches!(dispatcher(&pool).run_once().await.unwrap(),PrDispatchWork::Enqueued(e,_) if e==event)
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn captured_events_and_terminal_dispatch_results_are_immutable_and_require_audit(
    pool: PgPool,
) {
    let (f, _, _, event) = publication(&pool).await;
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("INSERT INTO pr_sync_dispatch(event_id,organization_id,proposal_id,state,reason,attempts,next_at) VALUES($1,$2,$3,'WAITING','SYNTHETIC',1,clock_timestamp()+interval '1 minute')").bind(event).bind(f.org).bind(f.proposal).execute(&mut *tx).await.unwrap();
    assert!(tx.commit().await.is_err());
    sqlx::query("UPDATE proposals SET state='CLOSED' WHERE id=$1")
        .bind(f.proposal)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        dispatcher(&pool).run_once().await.unwrap(),
        PrDispatchWork::Skipped(event)
    );
    for sql in [
        "UPDATE pr_sync_dispatch SET reason='OTHER',attempts=attempts+1 WHERE event_id=$1",
        "DELETE FROM pr_sync_dispatch WHERE event_id=$1",
        "UPDATE outbox SET payload='{}' WHERE id=$1",
        "UPDATE outbox SET sent_at=NULL WHERE id=$1",
        "DELETE FROM outbox WHERE id=$1",
    ] {
        assert!(
            sqlx::query(sql).bind(event).execute(&pool).await.is_err(),
            "{sql}"
        );
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn revocation_while_waiting_on_current_user_lock_cannot_return_admission_authority(
    pool: PgPool,
) {
    let (f, _, _, event) = publication(&pool).await;
    let mut lock = pool.begin().await.unwrap();
    sqlx::query("UPDATE users SET status='DISABLED' WHERE id=$1")
        .bind(f.owner)
        .execute(&mut *lock)
        .await
        .unwrap();
    let worker = dispatcher(&pool);
    let task = tokio::spawn(async move { worker.run_once().await });
    tokio::time::timeout(Duration::from_secs(5),async {
        loop {let waiting:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND pid<>pg_backend_pid() AND wait_event_type='Lock')").fetch_one(&pool).await.unwrap();if waiting {break;}tokio::time::sleep(Duration::from_millis(10)).await;}
    }).await.unwrap();
    lock.commit().await.unwrap();
    assert_eq!(
        task.await.unwrap().unwrap(),
        PrDispatchWork::Deferred(event)
    );
    assert_eq!(counts(&pool).await, (0, 0, 0, 0));
}

#[sqlx::test(migrations = "../../migrations")]
async fn manual_admission_racing_dispatch_preserves_lock_order_and_one_delivery(pool: PgPool) {
    let (f, _, _, event) = publication(&pool).await;
    let store = PgPrSync::new(pool.clone());
    let dispatch = dispatcher(&pool);
    let origin = site();
    let (manual, automatic) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(
            store.reserve(f.owner, f.proposal, event, &origin),
            dispatch.run_once()
        )
    })
    .await
    .unwrap();
    let receipt = manual.unwrap();
    assert!(matches!(
        automatic.unwrap(),
        PrDispatchWork::Idle | PrDispatchWork::Enqueued(_, _)
    ));
    assert_eq!(counts(&pool).await, (1, 1, 1, 1));
    let op: Uuid =
        sqlx::query_scalar("SELECT operation_id FROM pr_sync_dispatch WHERE event_id=$1")
            .bind(event)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(op, receipt.operation_id);
}

#[sqlx::test(migrations = "../../migrations")]
async fn invalid_projection_is_quarantined_without_changing_the_saved_snapshot(pool: PgPool) {
    let (f, _, snapshot, event) = publication(&pool).await;
    sqlx::query("UPDATE proposals SET links=$2 WHERE id=$1")
        .bind(f.proposal)
        .bind(json!([{"kind":"ticket","url":"http://unsafe.example/ticket"}]))
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        dispatcher(&pool).run_once().await.unwrap(),
        PrDispatchWork::Rejected(event)
    );
    assert_eq!(
        dispatch_state(&pool, event).await,
        ("REJECTED".into(), "PROJECTION_INVALID".into())
    );
    assert_eq!(counts(&pool).await, (0, 0, 0, 1));
    let current: (String, Option<Uuid>) =
        sqlx::query_as("SELECT state,active_review_snapshot_id FROM proposals WHERE id=$1")
            .bind(f.proposal)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(current, ("IN_REVIEW".into(), Some(snapshot.snapshot_id)));
}

#[sqlx::test(migrations = "../../migrations")]
async fn upgrading_existing_unknown_intents_preserves_their_original_work_and_never_requeues_delivery(
    pool: PgPool,
) {
    let (f, _, _, event) = publication(&pool).await;
    let store = PgPrSync::new(pool.clone());
    let receipt = store
        .reserve(f.owner, f.proposal, event, &site())
        .await
        .unwrap();
    let lease = store.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    store.start_write(&lease, &[]).await.unwrap();
    store
        .defer(&lease, "RESULT_UNKNOWN", Some(60))
        .await
        .unwrap();
    sqlx::query("UPDATE users SET status='DISABLED' WHERE id=$1")
        .bind(f.owner)
        .execute(&pool)
        .await
        .unwrap();
    let query = "SELECT jsonb_build_object('intent',to_jsonb(i),'job',to_jsonb(j),'operation',to_jsonb(o),'write',to_jsonb(w)) FROM pr_sync_intents i JOIN jobs j ON j.id=i.job_id JOIN git_operations o ON o.id=i.operation_id JOIN pr_sync_writes w ON w.operation_id=i.operation_id WHERE i.operation_id=$1";
    let before: serde_json::Value = sqlx::query_scalar(query)
        .bind(receipt.operation_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    // Remove only migration-015 objects/metadata to recreate the pre-upgrade
    // journal schema. Existing intent/write/job/outbox/audit evidence is retained.
    sqlx::raw_sql(
        "DROP TRIGGER pr_dispatch_event_guard ON outbox;
      DROP TRIGGER pr_dispatch_event_complete ON outbox;
      DROP TABLE pr_sync_dispatch CASCADE;
      DROP FUNCTION pr_dispatch_guard(); DROP FUNCTION pr_dispatch_complete();
      DROP FUNCTION pr_dispatch_event_guard(); DROP FUNCTION pr_sync_publication_actor(uuid);
      DROP FUNCTION pr_sync_snapshot_current(uuid,uuid,uuid); DROP INDEX pr_dispatch_pending;
      DELETE FROM _sqlx_migrations WHERE version=202611180015;",
    )
    .execute(&pool)
    .await
    .unwrap();
    specspace_infrastructure::MIGRATOR.run(&pool).await.unwrap();
    let after: serde_json::Value = sqlx::query_scalar(query)
        .bind(receipt.operation_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(before, after);
    assert_eq!(
        dispatch_state(&pool, event).await,
        ("ENQUEUED".into(), "EXISTING_INTENT".into())
    );
    assert_eq!(
        dispatcher(&pool).run_once().await.unwrap(),
        PrDispatchWork::Idle
    );
    assert_eq!(counts(&pool).await, (1, 1, 1, 1));
}
