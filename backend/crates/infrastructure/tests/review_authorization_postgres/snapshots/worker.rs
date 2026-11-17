use super::*;
use specspace_infrastructure::review_worker::{ReviewWork, ReviewWorker};
fn worker(pool: &PgPool, preparer: ReviewPreparer) -> Arc<ReviewWorker> {
    Arc::new(
        ReviewWorker::new(
            Arc::new(PgReviewSubmissions::new(pool.clone())),
            Arc::new(PgReviewSnapshots::new(pool.clone())),
            Arc::new(preparer),
            Duration::from_secs(30),
        )
        .unwrap(),
    )
}
async fn started(source: &Source) {
    tokio::time::timeout(Duration::from_secs(5), source.started.notified())
        .await
        .unwrap();
}
async fn expire(pool: &PgPool, job: Uuid) {
    sqlx::query("UPDATE jobs SET leased_until=clock_timestamp()-interval '1 second' WHERE id=$1")
        .bind(job)
        .execute(pool)
        .await
        .unwrap();
}
async fn assert_unpublished(pool: &PgPool, f: &Fixture) {
    let (count,version,state):(i64,i64,String)=sqlx::query_as("SELECT (SELECT count(*) FROM review_snapshots WHERE proposal_id=p.id),version,state FROM proposals p WHERE id=$1").bind(f.proposal).fetch_one(pool).await.unwrap();
    assert_eq!((count, version, state.as_str()), (0, 2, "DRAFT"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn worker_completes_real_image_evidence_once_and_then_is_idle(pool: PgPool) {
    let (f, source, preparer) = setup(&pool).await;
    let ticket = reserve(&pool, &f, 2).await;
    let worker = worker(&pool, preparer);
    assert_eq!(
        worker.run_once().await.unwrap(),
        ReviewWork::Completed(ticket.job_id)
    );
    let submissions = PgReviewSubmissions::new(pool.clone());
    let status = submissions.status(f.owner, ticket.job_id).await.unwrap();
    assert_eq!(status.phase, "SNAPSHOT_READY");
    assert_eq!(status.attempts, 1);
    let publication = status.publication.unwrap();
    assert!(publication.activated);
    let shared = PgReviewSnapshots::new(pool.clone())
        .get(f.product, f.proposal, publication.snapshot_id)
        .await
        .unwrap();
    assert_eq!((shared.file_count, shared.media_count), (2, 1));
    let reads = source.reads.load(Ordering::SeqCst);
    assert_eq!(worker.run_once().await.unwrap(), ReviewWork::Idle);
    assert_eq!(source.reads.load(Ordering::SeqCst), reads);
}

#[sqlx::test(migrations = "../../migrations")]
async fn worker_revoked_or_stale_reservations_fail_before_provider_io(pool: PgPool) {
    for fault in 0..4 {
        let (f, source, preparer) = setup(&pool).await;
        let ticket = reserve(&pool, &f, 2).await;
        match fault {
            0 => {
                sqlx::query("UPDATE users SET status='DISABLED' WHERE id=$1")
                    .bind(f.owner)
                    .execute(&pool)
                    .await
                    .unwrap();
            }
            1 => {
                sqlx::query("DELETE FROM memberships WHERE organization_id=$1 AND user_id=$2")
                    .bind(f.org)
                    .bind(f.product)
                    .execute(&pool)
                    .await
                    .unwrap();
            }
            2 => {
                sqlx::query("UPDATE proposals SET version=3 WHERE id=$1")
                    .bind(f.proposal)
                    .execute(&pool)
                    .await
                    .unwrap();
            }
            _ => {
                sqlx::query("UPDATE organizations SET policy_version=2 WHERE id=$1")
                    .bind(f.org)
                    .execute(&pool)
                    .await
                    .unwrap();
            }
        }
        assert_eq!(
            worker(&pool, preparer).run_once().await.unwrap(),
            ReviewWork::Failed(ticket.job_id)
        );
        assert_eq!(source.reads.load(Ordering::SeqCst), 0);
        let (job,operation,code):(String,String,String)=sqlx::query_as("SELECT j.state,o.state,j.result->>'error_code' FROM jobs j JOIN git_operations o ON o.id=j.operation_id WHERE j.id=$1").bind(ticket.job_id).fetch_one(&pool).await.unwrap();
        assert_eq!((job.as_str(), operation.as_str()), ("FAILED", "FAILED"));
        assert_eq!(
            code,
            if fault == 0 {
                "REVIEW_FORBIDDEN"
            } else {
                "REVIEW_CONTEXT_CHANGED"
            }
        );
        let snapshots: i64 =
            sqlx::query_scalar("SELECT count(*) FROM review_snapshots WHERE proposal_id=$1")
                .bind(f.proposal)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(snapshots, 0);
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn worker_honors_read_backoff_and_retries_without_duplicate_publication(pool: PgPool) {
    for (error, delay, code) in [
        (
            GitError::RateLimited {
                retry_after_seconds: Some(7),
            },
            7,
            "REVIEW_PROVIDER_RATE_LIMIT",
        ),
        (
            GitError::ProviderUnavailable,
            2,
            "REVIEW_PROVIDER_UNAVAILABLE",
        ),
        (
            GitError::ResultUnknownWithRetryAfter {
                retry_after_seconds: 9,
            },
            9,
            "REVIEW_PROVIDER_UNAVAILABLE",
        ),
    ] {
        let (f, source, preparer) = setup_media(&pool, false).await;
        let ticket = reserve(&pool, &f, 2).await;
        *source.failure.lock().unwrap() = Some(error);
        let worker = worker(&pool, preparer);
        assert_eq!(
            worker.run_once().await.unwrap(),
            ReviewWork::Deferred(ticket.job_id)
        );
        let status = PgReviewSubmissions::new(pool.clone())
            .status(f.owner, ticket.job_id)
            .await
            .unwrap();
        assert_eq!(status.phase, "RETRY_WAIT");
        assert_eq!(status.error_code.as_deref(), Some(code));
        assert!(status.publication.is_none());
        let remaining: bool = sqlx::query_scalar(
            "SELECT available_at>=clock_timestamp()+make_interval(secs=>$2) FROM jobs WHERE id=$1",
        )
        .bind(ticket.job_id)
        .bind(f64::from(delay - 1))
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(remaining);
        assert_eq!(worker.run_once().await.unwrap(), ReviewWork::Idle);
        assert_unpublished(&pool, &f).await;
        *source.failure.lock().unwrap() = None;
        sqlx::query("UPDATE jobs SET available_at=clock_timestamp() WHERE id=$1")
            .bind(ticket.job_id)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            worker.run_once().await.unwrap(),
            ReviewWork::Completed(ticket.job_id)
        );
        let status = PgReviewSubmissions::new(pool.clone())
            .status(f.owner, ticket.job_id)
            .await
            .unwrap();
        assert_eq!(status.attempts, 2);
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM review_submission_results WHERE operation_id=$1",
        )
        .bind(ticket.operation_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 1);
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn invalid_content_media_and_provider_denial_fail_without_storing_diagnostics(pool: PgPool) {
    for fault in 0..4 {
        let (f, source, preparer) = if fault == 0 {
            setup_body(
                &pool,
                false,
                Some("[Private missing target](#SEC-NOT-FOUND)"),
            )
            .await
        } else {
            setup(&pool).await
        };
        let ticket = reserve(&pool, &f, 2).await;
        match fault {
            1 => source.invalid_media.store(true, Ordering::SeqCst),
            2 => *source.failure.lock().unwrap() = Some(GitError::Unauthorized),
            3 => {
                *source.failure.lock().unwrap() = Some(GitError::RateLimited {
                    retry_after_seconds: Some(86401),
                })
            }
            _ => {}
        }
        assert_eq!(
            worker(&pool, preparer).run_once().await.unwrap(),
            ReviewWork::Failed(ticket.job_id)
        );
        assert_unpublished(&pool, &f).await;
        let result: serde_json::Value = sqlx::query_scalar("SELECT result FROM jobs WHERE id=$1")
            .bind(ticket.job_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(result.as_object().unwrap().len(), 2);
        assert_eq!(result["phase"], "FAILED");
        let code = result["error_code"].as_str().unwrap();
        assert_eq!(
            code,
            match fault {
                0 => "REVIEW_VALIDATION_FAILED",
                1 => "REVIEW_MEDIA_BLOB_MISMATCH",
                2 => "REVIEW_PROVIDER_UNAUTHORIZED",
                _ => "REVIEW_PROVIDER_RATE_LIMIT",
            }
        );
        assert!(!result.to_string().contains("SEC-NOT-FOUND"));
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn shutdown_cancels_active_reads_and_a_new_worker_reclaims_only_expired_work(pool: PgPool) {
    let (f, source, preparer) = setup(&pool).await;
    let ticket = reserve(&pool, &f, 2).await;
    source.blocked.store(true, Ordering::SeqCst);
    let worker = worker(&pool, preparer);
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let running = worker.clone();
    let task = tokio::spawn(async move { running.run(stopped).await });
    started(&source).await;
    stop.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap();
    let (state, attempt): (String, i32) =
        sqlx::query_as("SELECT state,attempts FROM jobs WHERE id=$1")
            .bind(ticket.job_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((state.as_str(), attempt), ("RUNNING", 1));
    assert_unpublished(&pool, &f).await;
    assert_eq!(worker.run_once().await.unwrap(), ReviewWork::Idle);
    expire(&pool, ticket.job_id).await;
    source.blocked.store(false, Ordering::SeqCst);
    assert_eq!(
        worker.run_once().await.unwrap(),
        ReviewWork::Completed(ticket.job_id)
    );
    let status = PgReviewSubmissions::new(pool.clone())
        .status(f.owner, ticket.job_id)
        .await
        .unwrap();
    assert_eq!(status.attempts, 2);
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_reclaimed_worker_attempt_cannot_fail_or_publish_over_the_new_owner(pool: PgPool) {
    let (f, source, preparer) = setup(&pool).await;
    let ticket = reserve(&pool, &f, 2).await;
    source.blocked.store(true, Ordering::SeqCst);
    let worker = worker(&pool, preparer);
    let running = worker.clone();
    let task = tokio::spawn(async move { running.run_once().await });
    started(&source).await;
    expire(&pool, ticket.job_id).await;
    let fresh = PgReviewSubmissions::new(pool.clone())
        .claim(Uuid::new_v4(), 60)
        .await
        .unwrap()
        .unwrap();
    source.blocked.store(false, Ordering::SeqCst);
    source.release.notify_one();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        ReviewWork::LeaseLost(ticket.job_id)
    );
    let (state, attempt): (String, i32) =
        sqlx::query_as("SELECT state,attempts FROM jobs WHERE id=$1")
            .bind(ticket.job_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((state.as_str(), attempt), ("RUNNING", 2));
    assert_unpublished(&pool, &f).await;
    PgReviewSubmissions::new(pool.clone())
        .fail(&fresh, "TEST_CLEANUP", None)
        .await
        .unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn publication_database_failure_leaves_reclaimable_work_and_no_partial_snapshot(
    pool: PgPool,
) {
    let (f, _, preparer) = setup(&pool).await;
    let ticket = reserve(&pool, &f, 2).await;
    let worker = worker(&pool, preparer);
    sqlx::raw_sql("CREATE FUNCTION fail_worker_event() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.event_type='REVIEW_SNAPSHOT_PUBLISHED' THEN RAISE EXCEPTION 'injected failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER fail_worker_event BEFORE INSERT ON outbox FOR EACH ROW EXECUTE FUNCTION fail_worker_event()").execute(&pool).await.unwrap();
    assert!(matches!(
        worker.run_once().await,
        Err(SubmissionError::Unavailable)
    ));
    assert_unpublished(&pool, &f).await;
    assert_eq!(worker.run_once().await.unwrap(), ReviewWork::Idle);
    sqlx::query("DROP TRIGGER fail_worker_event ON outbox")
        .execute(&pool)
        .await
        .unwrap();
    expire(&pool, ticket.job_id).await;
    assert_eq!(
        worker.run_once().await.unwrap(),
        ReviewWork::Completed(ticket.job_id)
    );
    assert_eq!(
        PgReviewSubmissions::new(pool.clone())
            .status(f.owner, ticket.job_id)
            .await
            .unwrap()
            .attempts,
        2
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn long_reads_renew_the_lease_without_restarting_preparation(pool: PgPool) {
    let (f, source, preparer) = setup_media(&pool, false).await;
    let ticket = reserve(&pool, &f, 2).await;
    source.blocked.store(true, Ordering::SeqCst);
    let worker = worker(&pool, preparer);
    let task = tokio::spawn(async move { worker.run_once().await });
    started(&source).await;
    let initial: sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc> =
        sqlx::query_scalar("SELECT heartbeat_at FROM jobs WHERE id=$1")
            .bind(ticket.job_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    tokio::time::timeout(Duration::from_secs(20),async {
        loop {
            let renewed:bool=sqlx::query_scalar("SELECT heartbeat_at>$2 AND leased_until>clock_timestamp()+interval '50 seconds' AND attempts=1 FROM jobs WHERE id=$1").bind(ticket.job_id).bind(initial).fetch_one(&pool).await.unwrap();
            if renewed {break;}
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.unwrap();
    source.blocked.store(false, Ordering::SeqCst);
    source.release.notify_one();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        ReviewWork::Completed(ticket.job_id)
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn heartbeat_stops_a_blocked_read_when_the_actor_loses_access(pool: PgPool) {
    let (f, source, preparer) = setup(&pool).await;
    let ticket = reserve(&pool, &f, 2).await;
    source.blocked.store(true, Ordering::SeqCst);
    let worker = worker(&pool, preparer);
    let task = tokio::spawn(async move { worker.run_once().await });
    started(&source).await;
    sqlx::query("UPDATE users SET status='DISABLED' WHERE id=$1")
        .bind(f.owner)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(20), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        ReviewWork::Failed(ticket.job_id)
    );
    assert_unpublished(&pool, &f).await;
    let code: String = sqlx::query_scalar("SELECT result->>'error_code' FROM jobs WHERE id=$1")
        .bind(ticket.job_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(code, "REVIEW_FORBIDDEN");
}

#[sqlx::test(migrations = "../../migrations")]
async fn deadline_defers_a_blocked_read_without_a_detached_heartbeat_or_publication(pool: PgPool) {
    let (f, source, preparer) = setup(&pool).await;
    let ticket = reserve(&pool, &f, 2).await;
    source.blocked.store(true, Ordering::SeqCst);
    let worker = worker(&pool, preparer);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(40), worker.run_once())
            .await
            .unwrap()
            .unwrap(),
        ReviewWork::Deferred(ticket.job_id)
    );
    assert_unpublished(&pool, &f).await;
    let status = PgReviewSubmissions::new(pool.clone())
        .status(f.owner, ticket.job_id)
        .await
        .unwrap();
    assert_eq!(status.phase, "RETRY_WAIT");
    assert!(matches!(
        status.error_code.as_deref(),
        Some("REVIEW_PREPARATION_TIMEOUT" | "REVIEW_PROCESSING_DELAY")
    ));
    let reads = source.reads.load(Ordering::SeqCst);
    source.release.notify_one();
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    assert_eq!(source.reads.load(Ordering::SeqCst), reads);
    let owns_lease: bool = sqlx::query_scalar(
        "SELECT lease_owner IS NOT NULL OR leased_until IS NOT NULL FROM jobs WHERE id=$1",
    )
    .bind(ticket.job_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(!owns_lease);
}

#[sqlx::test(migrations = "../../migrations")]
async fn exhausted_read_retries_release_the_mutation_slot_for_a_new_request(pool: PgPool) {
    let (f, source, preparer) = setup_media(&pool, false).await;
    let ticket = reserve(&pool, &f, 2).await;
    *source.failure.lock().unwrap() = Some(GitError::ProviderUnavailable);
    let worker = worker(&pool, preparer);
    for attempt in 1..=8 {
        let expected = if attempt == 8 {
            ReviewWork::Failed(ticket.job_id)
        } else {
            ReviewWork::Deferred(ticket.job_id)
        };
        assert_eq!(worker.run_once().await.unwrap(), expected);
        let status = PgReviewSubmissions::new(pool.clone())
            .status(f.owner, ticket.job_id)
            .await
            .unwrap();
        assert_eq!(status.attempts, attempt);
        sqlx::query("UPDATE jobs SET available_at=clock_timestamp() WHERE id=$1")
            .bind(ticket.job_id)
            .execute(&pool)
            .await
            .unwrap();
    }
    assert_unpublished(&pool, &f).await;
    assert_eq!(worker.run_once().await.unwrap(), ReviewWork::Idle);
    let next = reserve(&pool, &f, 2).await;
    assert_ne!(next.operation_id, ticket.operation_id);
}
