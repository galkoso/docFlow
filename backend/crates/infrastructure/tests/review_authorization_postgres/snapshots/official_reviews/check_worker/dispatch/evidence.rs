use super::*;
use crate::snapshots::Factory as SourceFactory;
use specspace_application::{
    comparison_file::discussion::AnchorSide,
    official_review::{OfficialReviewRequest, ReviewDecision},
    review_configuration::ReviewArea,
    review_visits::{ReviewSourceRequest, ReviewVisitRequest},
    unit_approval::UnitApprovalRequest,
};
use specspace_infrastructure::{
    official_reviews::PgOfficialReviews, unit_approvals::PgUnitApprovals,
};

fn source(f: &Frame) -> SourceFactory {
    SourceFactory {
        source: f.git.clone(),
        repository: f.f.repository,
    }
}
async fn prior_runs(pool: &PgPool, f: &Frame) -> Arc<Provider> {
    let prior: Vec<(Uuid,i64,String)> = sqlx::query_as("SELECT r.operation_id,r.run_id,x.result FROM review_check_runs r JOIN review_check_results x ON x.operation_id=r.operation_id WHERE r.repository_id=$1 ORDER BY r.run_id")
        .bind(f.f.repository).fetch_all(pool).await.unwrap();
    let mut port = provider(pool, f);
    Arc::get_mut(&mut port).unwrap().run_id = prior.last().map_or(81, |(_, id, _)| *id as u64 + 1);
    *port.rows.lock().unwrap() = prior
        .into_iter()
        .map(|(operation, id, result)| CheckRun {
            id: CheckRunId::new(id as u64).unwrap(),
            scope: CheckScope::new(42, 7, sha(1)).unwrap(),
            operation,
            state: match result.as_str() {
                "PENDING" => CheckState::Pending,
                "FAILURE" => CheckState::Failure,
                "SUCCESS" => CheckState::Success,
                _ => panic!("unexpected test result"),
            },
        })
        .collect();
    port
}
async fn fingerprint(pool: &PgPool, f: &Frame) -> String {
    sqlx::query_scalar("SELECT fingerprint FROM review_snapshots WHERE id=$1")
        .bind(f.snapshot)
        .fetch_one(pool)
        .await
        .unwrap()
}
async fn decision(
    pool: &PgPool,
    f: &Frame,
    actor: Uuid,
    domain: ReviewArea,
    value: ReviewDecision,
) {
    PgOfficialReviews::new(pool.clone())
        .submit(
            actor,
            f.f.proposal,
            f.snapshot,
            OfficialReviewRequest {
                request_id: Uuid::new_v4(),
                expected_fingerprint: fingerprint(pool, f).await,
                domain,
                decision: value,
                explanation: "Independent synthetic review".into(),
            },
            &source(f),
        )
        .await
        .unwrap();
}
async fn publish(
    pool: &PgPool,
    f: &Frame,
    dispatcher: &ReviewCheckDispatcher,
    actor: Uuid,
    expected: &str,
) -> Uuid {
    let D::Enqueued(event, job) = dispatcher.run_once().await.unwrap() else {
        panic!("expected admitted evidence event")
    };
    let (stored_actor, result): (Uuid, String) =
        sqlx::query_as("SELECT actor_id,result FROM review_check_intents WHERE operation_id=$1")
            .bind(event)
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(stored_actor, actor);
    assert_eq!(result, expected);
    let port = prior_runs(pool, f).await;
    let worker = ReviewCheckWorker::new(
        f.store.clone(),
        Arc::new(Factory(port.clone())),
        Duration::from_secs(60),
    )
    .unwrap();
    assert_eq!(worker.run_once().await.unwrap(), CheckWork::Completed(job));
    assert_eq!(port.creates.load(Ordering::SeqCst), 1);
    assert_eq!(port.updates.load(Ordering::SeqCst), 1);
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT result FROM review_check_results WHERE operation_id=$1"
        )
        .bind(event)
        .fetch_one(pool)
        .await
        .unwrap(),
        expected
    );
    event
}

#[sqlx::test(migrations = "../../migrations")]
async fn independent_votes_automatically_advance_pending_to_success_and_request_changes_to_failure(
    pool: PgPool,
) {
    let f = prepared(&pool, false).await;
    let dispatcher = ReviewCheckDispatcher::new(f.store.clone());
    publish(&pool, &f, &dispatcher, f.f.owner, "PENDING").await;
    decision(
        &pool,
        &f,
        f.f.product,
        ReviewArea::Product,
        ReviewDecision::ApproveAll,
    )
    .await;
    let product = publish(&pool, &f, &dispatcher, f.f.product, "PENDING").await;
    decision(
        &pool,
        &f,
        f.f.development,
        ReviewArea::Development,
        ReviewDecision::ApproveAll,
    )
    .await;
    let success = publish(&pool, &f, &dispatcher, f.f.development, "SUCCESS").await;
    decision(
        &pool,
        &f,
        f.f.product,
        ReviewArea::Product,
        ReviewDecision::RequestChanges,
    )
    .await;
    let failure = publish(&pool, &f, &dispatcher, f.f.product, "FAILURE").await;
    assert_ne!(product, success);
    assert_ne!(success, failure);
    assert_eq!(dispatcher.run_once().await.unwrap(), D::Idle);
    assert_eq!(count(&pool, "review_check_results").await, 4);
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM proposals WHERE id=$1")
            .bind(f.f.proposal)
            .fetch_one(&pool)
            .await
            .unwrap(),
        "CHANGES_REQUESTED"
    );
}

async fn coverage(pool: &PgPool, f: &Frame) -> String {
    let fp = fingerprint(pool, f).await;
    let units = PgReviewSnapshots::new(pool.clone())
        .units(f.f.product, f.f.proposal, f.snapshot, 0, 100)
        .await
        .unwrap();
    let unit = units
        .units
        .iter()
        .find(|u| {
            u.before.as_ref().is_none_or(|s| s.range.is_some())
                && u.after.as_ref().is_none_or(|s| s.range.is_some())
        })
        .expect("fixture includes a readable source unit");
    let visits = crate::snapshots::visits::store(pool, Arc::new(source(f)));
    for (side, present) in [
        (AnchorSide::Before, unit.before.is_some()),
        (AnchorSide::After, unit.after.is_some()),
    ] {
        if !present {
            continue;
        }
        let window = visits
            .deliver(
                f.f.product,
                f.f.proposal,
                f.snapshot,
                ReviewSourceRequest {
                    expected_fingerprint: fp.clone(),
                    domain: ReviewArea::Product,
                    unit_id: unit.id.clone(),
                    side,
                    offset: 0,
                },
            )
            .await
            .unwrap();
        visits
            .acknowledge(
                f.f.product,
                f.f.proposal,
                f.snapshot,
                ReviewVisitRequest {
                    request_id: Uuid::new_v4(),
                    delivery_id: window.delivery_id,
                    expected_fingerprint: fp.clone(),
                    domain: ReviewArea::Product,
                },
            )
            .await
            .unwrap();
    }
    PgUnitApprovals::new(pool.clone())
        .approve(
            f.f.product,
            f.f.proposal,
            f.snapshot,
            UnitApprovalRequest {
                request_id: Uuid::new_v4(),
                expected_fingerprint: fp,
                domain: ReviewArea::Product,
                unit_id: unit.id.clone(),
            },
        )
        .await
        .unwrap();
    unit.id.clone()
}

#[sqlx::test(migrations = "../../migrations")]
async fn explicit_visits_and_individual_approvals_each_dispatch_without_granting_proposal_approval(
    pool: PgPool,
) {
    let f = prepared(&pool, false).await;
    let dispatcher = ReviewCheckDispatcher::new(f.store.clone());
    publish(&pool, &f, &dispatcher, f.f.owner, "PENDING").await;
    coverage(&pool, &f).await;
    let events:i64=sqlx::query_scalar("SELECT count(*) FROM outbox WHERE event_type IN ('REVIEW_SOURCE_ACKNOWLEDGED','REVIEW_UNIT_APPROVED')").fetch_one(&pool).await.unwrap();
    assert!(events >= 2);
    for _ in 0..events {
        publish(&pool, &f, &dispatcher, f.f.product, "PENDING").await;
    }
    assert_eq!(dispatcher.run_once().await.unwrap(), D::Idle);
    assert_eq!(count(&pool, "official_reviews").await, 0);
    assert_eq!(count(&pool, "review_check_results").await, events + 1);
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM proposals WHERE id=$1")
            .bind(f.f.proposal)
            .fetch_one(&pool)
            .await
            .unwrap(),
        "IN_REVIEW"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn copied_or_forged_evidence_envelopes_are_rejected_before_provider_reads(pool: PgPool) {
    let f = prepared(&pool, false).await;
    let dispatcher = ReviewCheckDispatcher::new(f.store.clone());
    publish(&pool, &f, &dispatcher, f.f.owner, "PENDING").await;
    decision(
        &pool,
        &f,
        f.f.product,
        ReviewArea::Product,
        ReviewDecision::Comment,
    )
    .await;
    coverage(&pool, &f).await;
    loop {
        match dispatcher.run_once().await.unwrap() {
            D::Enqueued(_, job) => {
                let port = prior_runs(&pool, &f).await;
                assert_eq!(
                    ReviewCheckWorker::new(
                        f.store.clone(),
                        Arc::new(Factory(port)),
                        Duration::from_secs(60)
                    )
                    .unwrap()
                    .run_once()
                    .await
                    .unwrap(),
                    CheckWork::Completed(job)
                );
            }
            D::Idle => break,
            other => panic!("{other:?}"),
        }
    }
    let originals:Vec<(String,serde_json::Value)>=sqlx::query_as("SELECT event_type,payload FROM outbox WHERE event_type IN ('REVIEW_DECISION_RECORDED','REVIEW_SOURCE_ACKNOWLEDGED','REVIEW_UNIT_APPROVED')").fetch_all(&pool).await.unwrap();
    let reads = f.git.reads.load(Ordering::SeqCst);
    for (kind, payload) in originals {
        for body in [
            payload,
            json!({"snapshot_id":f.snapshot,"actor_id":f.f.owner,"result":"SUCCESS"}),
            json!({"snapshot_id":"invalid","private":"PRIVATE BODY SENTINEL"}),
        ] {
            let event = Uuid::new_v4();
            sqlx::query("INSERT INTO outbox(id,organization_id,entity_id,event_type,payload) VALUES($1,$2,$3,$4,$5)").bind(event).bind(f.f.org).bind(f.f.proposal).bind(&kind).bind(body).execute(&pool).await.unwrap();
            assert_eq!(dispatcher.run_once().await.unwrap(), D::Rejected(event));
            assert_eq!(dispatch_state(&pool, event).await.1, "PUBLICATION_INVALID");
        }
    }
    assert_eq!(f.git.reads.load(Ordering::SeqCst), reads);
}

#[sqlx::test(migrations = "../../migrations")]
async fn old_or_revoked_review_events_cannot_borrow_the_snapshot_publisher_authority(pool: PgPool) {
    for closed in [false, true] {
        let f = prepared(&pool, false).await;
        let dispatcher = ReviewCheckDispatcher::new(f.store.clone());
        publish(&pool, &f, &dispatcher, f.f.owner, "PENDING").await;
        decision(
            &pool,
            &f,
            f.f.product,
            ReviewArea::Product,
            ReviewDecision::Comment,
        )
        .await;
        let event: Uuid =
            sqlx::query_scalar("SELECT event_id FROM official_reviews WHERE proposal_id=$1")
                .bind(f.f.proposal)
                .fetch_one(&pool)
                .await
                .unwrap();
        if closed {
            sqlx::query("UPDATE proposals SET state='CLOSED' WHERE id=$1")
                .bind(f.f.proposal)
                .execute(&pool)
                .await
                .unwrap();
        } else {
            sqlx::query("UPDATE memberships SET role='READER' WHERE user_id=$1")
                .bind(f.f.product)
                .execute(&pool)
                .await
                .unwrap();
        }
        let reads = f.git.reads.load(Ordering::SeqCst);
        assert_eq!(
            dispatcher.run_once().await.unwrap(),
            if closed {
                D::Skipped(event)
            } else {
                D::Deferred(event)
            }
        );
        assert_eq!(f.git.reads.load(Ordering::SeqCst), reads);
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM review_check_intents WHERE source_event=$1"
            )
            .bind(event)
            .fetch_one(&pool)
            .await
            .unwrap(),
            0
        );
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_live_review_delivery_fence_cannot_substitute_the_proposal_owner_for_the_reviewer(
    pool: PgPool,
) {
    let mut f = prepared(&pool, false).await;
    let dispatcher = ReviewCheckDispatcher::new(f.store.clone());
    publish(&pool, &f, &dispatcher, f.f.owner, "PENDING").await;
    decision(
        &pool,
        &f,
        f.f.product,
        ReviewArea::Product,
        ReviewDecision::Comment,
    )
    .await;
    f.event = sqlx::query_scalar("SELECT event_id FROM official_reviews WHERE proposal_id=$1")
        .bind(f.f.proposal)
        .fetch_one(&pool)
        .await
        .unwrap();
    let _ = tokio::time::timeout(Duration::from_millis(5), f.git.started.notified()).await;
    f.git.blocked.store(true, Ordering::SeqCst);
    let running = tokio::spawn(async move { dispatcher.run_once().await });
    tokio::time::timeout(Duration::from_secs(10), f.git.started.notified())
        .await
        .unwrap();
    let (worker, attempt): (Uuid, i32) =
        sqlx::query_as("SELECT lease_owner,attempts FROM review_check_dispatch WHERE event_id=$1")
            .bind(f.event)
            .fetch_one(&pool)
            .await
            .unwrap();
    stale_insert(&pool, &f, worker, attempt).await;
    f.git.blocked.store(false, Ordering::SeqCst);
    f.git.release.notify_one();
    assert!(matches!(running.await.unwrap().unwrap(),D::Enqueued(event,_) if event==f.event));
    assert_eq!(
        sqlx::query_scalar::<_, Uuid>(
            "SELECT actor_id FROM review_check_intents WHERE operation_id=$1"
        )
        .bind(f.event)
        .fetch_one(&pool)
        .await
        .unwrap(),
        f.f.product
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn an_evidence_event_waits_for_the_lane_then_retries_after_unstarted_stale_work_stops(
    pool: PgPool,
) {
    let f = prepared(&pool, false).await;
    let dispatcher = ReviewCheckDispatcher::new(f.store.clone());
    let D::Enqueued(_, old_job) = dispatcher.run_once().await.unwrap() else {
        panic!("snapshot admission")
    };
    decision(
        &pool,
        &f,
        f.f.product,
        ReviewArea::Product,
        ReviewDecision::ApproveAll,
    )
    .await;
    let event: Uuid =
        sqlx::query_scalar("SELECT event_id FROM official_reviews WHERE proposal_id=$1")
            .bind(f.f.proposal)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(dispatcher.run_once().await.unwrap(), D::Deferred(event));
    assert_eq!(dispatch_state(&pool, event).await.1, "CHECK_LANE_BUSY");
    assert_eq!(dispatcher.run_once().await.unwrap(), D::Idle);
    let port = provider(&pool, &f);
    assert_eq!(
        ReviewCheckWorker::new(
            f.store.clone(),
            Arc::new(Factory(port.clone())),
            Duration::from_secs(60)
        )
        .unwrap()
        .run_once()
        .await
        .unwrap(),
        CheckWork::Stopped(old_job)
    );
    assert_eq!(port.creates.load(Ordering::SeqCst), 0);
    tokio::time::sleep(Duration::from_millis(30100)).await;
    assert_eq!(
        publish(&pool, &f, &dispatcher, f.f.product, "PENDING").await,
        event
    );
    assert_eq!(dispatch_state(&pool, event).await.2, 2);
    assert_eq!(count(&pool, "review_check_results").await, 1);
}
