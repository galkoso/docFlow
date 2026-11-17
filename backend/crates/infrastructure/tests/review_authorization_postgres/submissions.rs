use super::*;
use specspace_application::{
    proposal::IdempotencyKey,
    review_submission::{ReviewSubmissionRequest, ValidatedReviewSubmission},
};
use specspace_infrastructure::{
    complete_job, lease_job,
    review_submissions::{PgReviewSubmissions, SubmissionError},
};
fn input(f: &Fixture) -> ValidatedReviewSubmission {
    ValidatedReviewSubmission::new(
        f.owner,
        f.proposal,
        ReviewSubmissionRequest {
            expected_head_sha: "1".repeat(40),
            expected_target_sha: "2".repeat(40),
            expected_proposal_version: 2,
            expected_configuration_version: 2,
            expected_policy_version: 1,
        },
    )
    .unwrap()
}
fn key() -> IdempotencyKey {
    IdempotencyKey::parse(Uuid::new_v4().to_string()).unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
async fn concurrent_replays_reserve_one_private_metadata_job_and_only_its_lease_bypasses_own_busy_gate(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    let store = PgReviewSubmissions::new(pool.clone());
    let input = input(&f);
    let key = key();
    let (a, b) = tokio::join!(store.reserve(&input, &key), store.reserve(&input, &key));
    let ticket = a.unwrap();
    assert_eq!(ticket, b.unwrap());
    let (jobs,ops,requests):(i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM jobs),(SELECT count(*) FROM git_operations),(SELECT count(*) FROM review_submissions)").fetch_one(&pool).await.unwrap();
    assert_eq!((jobs, ops, requests), (1, 1, 1));
    let payload: serde_json::Value = sqlx::query_scalar("SELECT payload FROM jobs WHERE id=$1")
        .bind(ticket.job_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        payload,
        serde_json::json!({"operation_id":ticket.operation_id})
    );
    let status = store.status(f.owner, ticket.job_id).await.unwrap();
    assert_eq!(status.state, "QUEUED");
    assert_eq!(status.attempts, 0);
    assert!(matches!(
        PgReviewAuthorization::new(pool.clone())
            .resolve(f.owner, f.proposal, &expectation())
            .await,
        Err(ReviewContextError::Busy)
    ));
    assert!(
        lease_job(&pool, Uuid::new_v4(), 60)
            .await
            .unwrap()
            .is_none()
    );
    let worker = Uuid::new_v4();
    let lease = store.claim(worker, 60).await.unwrap().unwrap();
    assert_eq!(lease.operation_id(), ticket.operation_id);
    assert_eq!(lease.attempts(), 1);
    let auth = store.authorization(&lease);
    let scope = auth
        .resolve(f.owner, f.proposal, lease.expectation())
        .await
        .unwrap();
    assert_eq!(scope.review.configuration_version, 2);
    assert!(
        !complete_job(
            &pool,
            ticket.job_id,
            worker,
            serde_json::json!({"phase":"READY"})
        )
        .await
        .unwrap()
    );
    let (state, version): (String, i64) =
        sqlx::query_as("SELECT state,version FROM proposals WHERE id=$1")
            .bind(f.proposal)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((state.as_str(), version), ("DRAFT", 2));
}

#[sqlx::test(migrations = "../../migrations")]
async fn changed_keys_busy_proposals_and_private_status_never_replace_an_existing_intent(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    let store = PgReviewSubmissions::new(pool.clone());
    let original = input(&f);
    let saved_key = key();
    let ticket = store.reserve(&original, &saved_key).await.unwrap();
    assert!(matches!(
        store.reserve(&original, &key()).await,
        Err(SubmissionError::Busy)
    ));
    let mut changed = original.request().clone();
    changed.expected_target_sha = "3".repeat(40);
    let changed = ValidatedReviewSubmission::new(f.owner, f.proposal, changed).unwrap();
    assert!(matches!(
        store.reserve(&changed, &saved_key).await,
        Err(SubmissionError::IdempotencyMismatch)
    ));
    let admin = user(&pool, f.org, "ADMIN", vec![]).await;
    assert!(matches!(
        store.status(admin, ticket.job_id).await,
        Err(SubmissionError::NotFound)
    ));
    let foreign = fixture(&pool).await;
    assert!(matches!(
        store.status(foreign.owner, ticket.job_id).await,
        Err(SubmissionError::NotFound)
    ));
    let lease = store.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    assert!(matches!(
        store
            .authorization(&lease)
            .resolve(f.owner, foreign.proposal, lease.expectation())
            .await,
        Err(ReviewContextError::LeaseLost)
    ));
}

#[sqlx::test(migrations = "../../migrations")]
async fn exact_replay_survives_head_and_state_changes_but_still_requires_current_edit_authority(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    let store = PgReviewSubmissions::new(pool.clone());
    let input = input(&f);
    let key = key();
    let ticket = store.reserve(&input, &key).await.unwrap();
    let lease = store.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    assert!(
        !store
            .fail(&lease, "REVIEW_SOURCE_INVALID", None)
            .await
            .unwrap()
    );
    sqlx::query(
        "UPDATE proposals SET state='CLOSED',version=3,head_sha=repeat('3',40) WHERE id=$1",
    )
    .bind(f.proposal)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(store.reserve(&input, &key).await.unwrap(), ticket);
    assert_eq!(
        store.status(f.owner, ticket.job_id).await.unwrap().state,
        "FAILED"
    );
    sqlx::query("UPDATE memberships SET role='READER' WHERE organization_id=$1 AND user_id=$2")
        .bind(f.org)
        .bind(f.owner)
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        store.reserve(&input, &key).await,
        Err(SubmissionError::Forbidden)
    ));
    assert!(matches!(
        store.status(f.owner, ticket.job_id).await,
        Err(SubmissionError::Forbidden)
    ));
}

#[sqlx::test(migrations = "../../migrations")]
async fn same_worker_reclaim_is_attempt_fenced_and_expired_owners_cannot_extend_fail_or_authorize(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    let store = PgReviewSubmissions::new(pool.clone());
    let ticket = store.reserve(&input(&f), &key()).await.unwrap();
    let worker = Uuid::new_v4();
    let first = store.claim(worker, 60).await.unwrap().unwrap();
    store.heartbeat(&first, 60).await.unwrap();
    assert!(store.claim(Uuid::new_v4(), 60).await.unwrap().is_none());
    sqlx::query("UPDATE jobs SET leased_until=clock_timestamp()-interval '1 second' WHERE id=$1")
        .bind(ticket.job_id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        store.heartbeat(&first, 60).await,
        Err(SubmissionError::LeaseLost)
    ));
    assert!(matches!(
        store.fail(&first, "REVIEW_TIMEOUT", None).await,
        Err(SubmissionError::LeaseLost)
    ));
    let second = store.claim(worker, 60).await.unwrap().unwrap();
    assert_eq!(second.attempts(), 2);
    assert!(matches!(
        store
            .authorization(&first)
            .resolve(f.owner, f.proposal, first.expectation())
            .await,
        Err(ReviewContextError::LeaseLost)
    ));
    assert!(matches!(
        store.fail(&first, "REVIEW_TIMEOUT", None).await,
        Err(SubmissionError::LeaseLost)
    ));
    store
        .authorization(&second)
        .resolve(f.owner, f.proposal, second.expectation())
        .await
        .unwrap();
    store.heartbeat(&second, 60).await.unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn revocation_and_metadata_changes_stop_lease_authority_while_definite_cleanup_remains_possible(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    let store = PgReviewSubmissions::new(pool.clone());
    store.reserve(&input(&f), &key()).await.unwrap();
    let lease = store.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    sqlx::query("UPDATE memberships SET role='READER' WHERE organization_id=$1 AND user_id=$2")
        .bind(f.org)
        .bind(f.owner)
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        store
            .authorization(&lease)
            .resolve(f.owner, f.proposal, lease.expectation())
            .await,
        Err(ReviewContextError::Forbidden)
    ));
    assert!(matches!(
        store.heartbeat(&lease, 60).await,
        Err(SubmissionError::Forbidden)
    ));
    assert!(
        !store
            .fail(&lease, "REVIEW_NOT_AUTHORIZED", None)
            .await
            .unwrap()
    );
    sqlx::query(
        "UPDATE memberships SET role='CONTRIBUTOR' WHERE organization_id=$1 AND user_id=$2",
    )
    .bind(f.org)
    .bind(f.owner)
    .execute(&pool)
    .await
    .unwrap();
    store.reserve(&input(&f), &key()).await.unwrap();
    let next = store.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    sqlx::query(
        "UPDATE memberships SET review_domains='{}' WHERE organization_id=$1 AND user_id=$2",
    )
    .bind(f.org)
    .bind(f.product)
    .execute(&pool)
    .await
    .unwrap();
    assert!(matches!(
        store
            .authorization(&next)
            .resolve(f.owner, f.proposal, next.expectation())
            .await,
        Err(ReviewContextError::Stale)
    ));
    assert!(matches!(
        store.heartbeat(&next, 60).await,
        Err(SubmissionError::Context(ReviewContextError::Stale))
    ));
    assert!(
        !store
            .fail(&next, "REVIEWER_NOT_ELIGIBLE", None)
            .await
            .unwrap()
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn started_or_unknown_provider_operations_are_never_reclaimed_or_failed_by_read_workers(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    let store = PgReviewSubmissions::new(pool.clone());
    let ticket = store.reserve(&input(&f), &key()).await.unwrap();
    let lease = store.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    for state in ["RUNNING", "UNKNOWN"] {
        sqlx::query("UPDATE git_operations SET state=$2 WHERE id=$1")
            .bind(ticket.operation_id)
            .bind(state)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE jobs SET leased_until=clock_timestamp()+interval '60 seconds' WHERE id=$1",
        )
        .bind(ticket.job_id)
        .execute(&pool)
        .await
        .unwrap();
        assert!(matches!(
            store.fail(&lease, "PROVIDER_UNKNOWN", None).await,
            Err(SubmissionError::LeaseLost)
        ));
        sqlx::query(
            "UPDATE jobs SET leased_until=clock_timestamp()-interval '1 second' WHERE id=$1",
        )
        .bind(ticket.job_id)
        .execute(&pool)
        .await
        .unwrap();
        assert!(store.claim(Uuid::new_v4(), 60).await.unwrap().is_none());
        assert_eq!(
            store.status(f.owner, ticket.job_id).await.unwrap().phase,
            "RECONCILIATION_REQUIRED"
        );
        assert!(matches!(
            store.reserve(&input(&f), &key()).await,
            Err(SubmissionError::Busy)
        ));
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn retry_backoff_exhaustion_and_definite_failure_release_only_read_only_reservations(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    let store = PgReviewSubmissions::new(pool.clone());
    let ticket = store.reserve(&input(&f), &key()).await.unwrap();
    let first = store.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    assert!(
        store
            .fail(&first, "PROVIDER_RATE_LIMIT", Some(41))
            .await
            .unwrap()
    );
    assert_eq!(
        store.status(f.owner, ticket.job_id).await.unwrap().phase,
        "RETRY_WAIT"
    );
    assert!(store.claim(Uuid::new_v4(), 60).await.unwrap().is_none());
    sqlx::query("UPDATE jobs SET available_at=clock_timestamp()-interval '1 second' WHERE id=$1")
        .bind(ticket.job_id)
        .execute(&pool)
        .await
        .unwrap();
    let second = store.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    assert_eq!(second.attempts(), 2);
    sqlx::query("UPDATE jobs SET attempts=max_attempts,leased_until=clock_timestamp()-interval '1 second' WHERE id=$1").bind(ticket.job_id).execute(&pool).await.unwrap();
    assert!(store.claim(Uuid::new_v4(), 60).await.unwrap().is_none());
    let result = store.status(f.owner, ticket.job_id).await.unwrap();
    assert_eq!(result.state, "FAILED");
    assert_eq!(
        result.error_code.as_deref(),
        Some("REVIEW_SUBMISSION_EXHAUSTED")
    );
    let operation: String = sqlx::query_scalar("SELECT state FROM git_operations WHERE id=$1")
        .bind(ticket.operation_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(operation, "FAILED");
    assert!(store.claim(Uuid::new_v4(), 60).await.unwrap().is_none());
    let failures:i64=sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE action='REVIEW_SUBMISSION_FAILED' AND after_ref=$1").bind(ticket.operation_id.to_string()).fetch_one(&pool).await.unwrap();
    assert_eq!(failures, 1);
    assert!(store.reserve(&input(&f), &key()).await.is_ok());
}

#[sqlx::test(migrations = "../../migrations")]
async fn database_guards_freeze_intent_operation_and_job_scope_and_reject_orphan_jobs(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    let store = PgReviewSubmissions::new(pool.clone());
    let ticket = store.reserve(&input(&f), &key()).await.unwrap();
    for statement in [
        "UPDATE review_submissions SET branch='change/forged' WHERE operation_id=$1",
        "DELETE FROM review_submissions WHERE operation_id=$1",
        "UPDATE git_operations SET expected_sha=repeat('3',40) WHERE id=$1",
        "UPDATE git_operations SET request_hash=repeat('a',64) WHERE id=$1",
    ] {
        assert!(
            sqlx::query(statement)
                .bind(ticket.operation_id)
                .execute(&pool)
                .await
                .is_err(),
            "{statement}"
        );
    }
    for statement in [
        "UPDATE jobs SET payload='{}' WHERE id=$1",
        "UPDATE jobs SET kind='OTHER' WHERE id=$1",
        "UPDATE jobs SET max_attempts=100 WHERE id=$1",
    ] {
        assert!(
            sqlx::query(statement)
                .bind(ticket.job_id)
                .execute(&pool)
                .await
                .is_err(),
            "{statement}"
        );
    }
    assert!(
        sqlx::query("TRUNCATE review_submissions CASCADE")
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("INSERT INTO jobs(id,organization_id,kind) VALUES($1,$2,'SUBMIT_REVIEW')")
            .bind(Uuid::new_v4())
            .bind(f.org)
            .execute(&pool)
            .await
            .is_err()
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn reservation_rolls_back_every_row_when_audit_or_deferred_checks_fail(pool: PgPool) {
    let f = fixture(&pool).await;
    let store = PgReviewSubmissions::new(pool.clone());
    let input = input(&f);
    let key = key();
    sqlx::query("CREATE FUNCTION reject_review_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='REVIEW_SUBMISSION_REQUESTED' THEN RAISE EXCEPTION 'synthetic audit failure'; END IF; RETURN NEW; END $$").execute(&pool).await.unwrap();
    sqlx::query("CREATE TRIGGER reject_review_audit BEFORE INSERT ON audit_events FOR EACH ROW EXECUTE FUNCTION reject_review_audit()").execute(&pool).await.unwrap();
    assert!(matches!(
        store.reserve(&input, &key).await,
        Err(SubmissionError::Unavailable)
    ));
    let counts:(i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM review_submissions),(SELECT count(*) FROM git_operations),(SELECT count(*) FROM jobs)").fetch_one(&pool).await.unwrap();
    assert_eq!(counts, (0, 0, 0));
    sqlx::query("DROP TRIGGER reject_review_audit ON audit_events")
        .execute(&pool)
        .await
        .unwrap();
    assert!(store.reserve(&input, &key).await.is_ok());
}

#[sqlx::test(migrations = "../../migrations")]
async fn stale_expectations_do_not_queue_work_and_status_never_returns_untrusted_result_payloads(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    let store = PgReviewSubmissions::new(pool.clone());
    let original = input(&f);
    for field in 0..4 {
        let mut changed = original.request().clone();
        match field {
            0 => changed.expected_head_sha = "4".repeat(40),
            1 => changed.expected_proposal_version += 1,
            2 => changed.expected_configuration_version += 1,
            _ => changed.expected_policy_version += 1,
        }
        let input = ValidatedReviewSubmission::new(f.owner, f.proposal, changed).unwrap();
        assert!(matches!(
            store.reserve(&input, &key()).await,
            Err(SubmissionError::Context(ReviewContextError::Stale))
        ));
    }
    let ticket = store.reserve(&original, &key()).await.unwrap();
    let lease = store.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    assert!(matches!(
        store.fail(&lease, "private source text", Some(1)).await,
        Err(SubmissionError::InvalidQuery)
    ));
    assert!(matches!(
        store.fail(&lease, "CODE", Some(0)).await,
        Err(SubmissionError::InvalidQuery)
    ));
    sqlx::query("UPDATE jobs SET result=$2 WHERE id=$1")
        .bind(ticket.job_id)
        .bind(serde_json::json!({"phase":"PREPARING","private_source":"synthetic private source"}))
        .execute(&pool)
        .await
        .unwrap();
    let status = store.status(f.owner, ticket.job_id).await.unwrap();
    assert!(
        !serde_json::to_string(&status)
            .unwrap()
            .contains("synthetic private source")
    );
    sqlx::query(
        "UPDATE jobs SET result=jsonb_build_object('phase','untrusted arbitrary text') WHERE id=$1",
    )
    .bind(ticket.job_id)
    .execute(&pool)
    .await
    .unwrap();
    assert!(matches!(
        store.status(f.owner, ticket.job_id).await,
        Err(SubmissionError::InvalidIntent)
    ));
    sqlx::query("UPDATE jobs SET state='SUCCEEDED',lease_owner=NULL,leased_until=NULL,result=jsonb_build_object('phase','PREPARING') WHERE id=$1").bind(ticket.job_id).execute(&pool).await.unwrap();
    sqlx::query("UPDATE git_operations SET state='SUCCEEDED' WHERE id=$1")
        .bind(ticket.operation_id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        store.status(f.owner, ticket.job_id).await,
        Err(SubmissionError::InvalidIntent)
    ));
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_lease_that_expires_while_waiting_for_current_membership_cannot_return_authority(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    let store = PgReviewSubmissions::new(pool.clone());
    let ticket = store.reserve(&input(&f), &key()).await.unwrap();
    let lease = store.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    let mut blocker = pool.begin().await.unwrap();
    sqlx::query(
        "SELECT user_id FROM memberships WHERE organization_id=$1 AND user_id=$2 FOR UPDATE",
    )
    .bind(f.org)
    .bind(f.product)
    .execute(&mut *blocker)
    .await
    .unwrap();
    sqlx::query("UPDATE jobs SET leased_until=clock_timestamp()+interval '1 second' WHERE id=$1")
        .bind(ticket.job_id)
        .execute(&pool)
        .await
        .unwrap();
    let auth = store.authorization(&lease);
    let actor = f.owner;
    let proposal = f.proposal;
    let expected = lease.expectation().clone();
    let work = tokio::spawn(async move { auth.resolve(actor, proposal, &expected).await });
    let observe = async {
        loop {
            let (blocked,expired):(i64,bool)=sqlx::query_as("SELECT (SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock'),leased_until<=clock_timestamp() FROM jobs WHERE id=$1").bind(ticket.job_id).fetch_one(&pool).await.unwrap();
            if blocked > 0 && expired {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(3), observe)
        .await
        .unwrap();
    blocker.commit().await.unwrap();
    assert!(matches!(
        work.await.unwrap(),
        Err(ReviewContextError::LeaseLost)
    ));
    assert_eq!(
        store
            .claim(Uuid::new_v4(), 60)
            .await
            .unwrap()
            .unwrap()
            .attempts(),
        2
    );
}
