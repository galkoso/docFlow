use super::*;
use serde_json::json;
use specspace_application::{pr_sync::PrSiteOrigin, review_submission::ReviewPublication};
use specspace_infrastructure::pr_sync::{PgPrSync, PrSyncStoreError};
fn site() -> PrSiteOrigin {
    PrSiteOrigin::parse("https://specs.example", false).unwrap()
}
async fn publication(pool: &PgPool) -> (Fixture, Arc<Source>, ReviewPublication, Uuid) {
    let (f, source, preparer) = setup(pool).await;
    let (lease, verified) = prepared(pool, &f, &preparer, 2).await;
    let result = PgReviewSnapshots::new(pool.clone())
        .publish(&lease, &verified, &preparer)
        .await
        .unwrap();
    let event = sqlx::query_scalar(
        "SELECT id FROM outbox WHERE entity_id=$1 AND event_type='REVIEW_SNAPSHOT_PUBLISHED'",
    )
    .bind(f.proposal)
    .fetch_one(pool)
    .await
    .unwrap();
    (f, source, result, event)
}
async fn counts(pool: &PgPool) -> (i64, i64, i64, i64) {
    sqlx::query_as("SELECT (SELECT count(*) FROM pr_sync_intents),(SELECT count(*) FROM jobs WHERE kind='SYNC_REVIEW_PR'),(SELECT count(*) FROM git_operations WHERE kind='PR_SYNC'),(SELECT count(*) FROM outbox WHERE event_type='REVIEW_SNAPSHOT_PUBLISHED' AND sent_at IS NOT NULL)").fetch_one(pool).await.unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
async fn delivery_atomically_freezes_exact_intent_and_replay_survives_metadata_and_site_changes(
    pool: PgPool,
) {
    let (f, source, published, event) = publication(&pool).await;
    let reads = source.reads.load(Ordering::SeqCst);
    let first = PgPrSync::new(pool.clone())
        .reserve(f.owner, f.proposal, event, &site())
        .await
        .unwrap();
    assert!(!first.replayed);
    assert_eq!(first.intent().source().snapshot, published.snapshot_id);
    assert_eq!(first.intent().source().proposal_version, 3);
    assert_eq!(first.intent().source().fingerprint, published.fingerprint);
    assert!(
        first
            .intent()
            .content()
            .body()
            .contains("רשימה מלאה לא נכללה")
    );
    assert_eq!(counts(&pool).await, (1, 1, 1, 1));
    assert!(
        specspace_infrastructure::lease_job(&pool, Uuid::new_v4(), 60)
            .await
            .unwrap()
            .is_none()
    );
    sqlx::query("UPDATE proposals SET title='Changed title',description='Changed description',version=version+1,state='CLOSED' WHERE id=$1").bind(f.proposal).execute(&pool).await.unwrap();
    let replay = PgPrSync::new(pool.clone())
        .reserve(
            f.owner,
            f.proposal,
            event,
            &PrSiteOrigin::parse("https://new.example", false).unwrap(),
        )
        .await
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(
        (replay.operation_id, replay.job_id),
        (first.operation_id, first.job_id)
    );
    assert_eq!(
        replay.intent().request_hash(),
        first.intent().request_hash()
    );
    assert_eq!(
        replay.intent().content().body(),
        first.intent().content().body()
    );
    assert_eq!(counts(&pool).await, (1, 1, 1, 1));
    assert_eq!(source.reads.load(Ordering::SeqCst), reads);
    sqlx::query("UPDATE users SET status='DISABLED' WHERE id=$1")
        .bind(f.owner)
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        PgPrSync::new(pool.clone())
            .reserve(f.owner, f.proposal, event, &site())
            .await,
        Err(PrSyncStoreError::Access(SubmissionError::NotFound))
    ));
}

#[sqlx::test(migrations = "../../migrations")]
async fn concurrent_deliveries_share_one_durable_operation_and_job(pool: PgPool) {
    let (f, _, _, event) = publication(&pool).await;
    let a = PgPrSync::new(pool.clone());
    let b = PgPrSync::new(pool.clone());
    let origin = site();
    let (left, right) = tokio::join!(
        a.reserve(f.owner, f.proposal, event, &origin),
        b.reserve(f.owner, f.proposal, event, &origin)
    );
    let left = left.unwrap();
    let right = right.unwrap();
    assert_eq!(left.operation_id, right.operation_id);
    assert_ne!(left.replayed, right.replayed);
    assert_eq!(counts(&pool).await, (1, 1, 1, 1));
}

#[sqlx::test(migrations = "../../migrations")]
async fn event_payload_requires_exact_flags_schema_and_real_successful_publication(pool: PgPool) {
    let (f, _, _, event) = publication(&pool).await;
    let original: serde_json::Value = sqlx::query_scalar("SELECT payload FROM outbox WHERE id=$1")
        .bind(event)
        .fetch_one(&pool)
        .await
        .unwrap();
    let mut invalid = Vec::new();
    for (key, value) in [
        ("pr_sync_required", json!(false)),
        ("approvals_granted", json!(true)),
        ("activated", json!(false)),
        ("operation_id", json!(Uuid::new_v4())),
        ("snapshot_id", json!(Uuid::new_v4())),
        ("unexpected", json!(true)),
    ] {
        let mut body = original.clone();
        body[key] = value;
        invalid.push(body);
    }
    for body in invalid {
        sqlx::query("UPDATE outbox SET payload=$2 WHERE id=$1")
            .bind(event)
            .bind(body)
            .execute(&pool)
            .await
            .unwrap();
        assert!(matches!(
            PgPrSync::new(pool.clone())
                .reserve(f.owner, f.proposal, event, &site())
                .await,
            Err(PrSyncStoreError::InvalidEvent)
        ));
        assert_eq!(counts(&pool).await, (0, 0, 0, 0));
    }
    sqlx::query("UPDATE outbox SET payload=$2 WHERE id=$1")
        .bind(event)
        .bind(original)
        .execute(&pool)
        .await
        .unwrap();
    PgPrSync::new(pool.clone())
        .reserve(f.owner, f.proposal, event, &site())
        .await
        .unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn inactive_or_redirected_snapshot_never_admits_a_new_pr_operation(pool: PgPool) {
    for mode in 0..5 {
        let (f, _, _, event) = publication(&pool).await;
        match mode {
            0 => {
                sqlx::query("UPDATE proposals SET head_sha=$2,version=version+1 WHERE id=$1")
                    .bind(f.proposal)
                    .bind(sha(9).as_str())
                    .execute(&pool)
                    .await
                    .unwrap();
            }
            1 => {
                sqlx::query("UPDATE organizations SET policy_version=policy_version+1 WHERE id=$1")
                    .bind(f.org)
                    .execute(&pool)
                    .await
                    .unwrap();
            }
            2 => {
                sqlx::query("UPDATE repositories SET installation_id='8' WHERE id=$1")
                    .bind(f.repository)
                    .execute(&pool)
                    .await
                    .unwrap();
            }
            3 => {
                sqlx::query("UPDATE proposals SET state='CLOSED' WHERE id=$1")
                    .bind(f.proposal)
                    .execute(&pool)
                    .await
                    .unwrap();
            }
            _ => {
                sqlx::query("UPDATE proposals SET target_sha=$2,version=version+1 WHERE id=$1")
                    .bind(f.proposal)
                    .bind(sha(9).as_str())
                    .execute(&pool)
                    .await
                    .unwrap();
            }
        }
        assert!(matches!(
            PgPrSync::new(pool.clone())
                .reserve(f.owner, f.proposal, event, &site())
                .await,
            Err(PrSyncStoreError::Stale)
        ));
    }
    assert_eq!(counts(&pool).await, (0, 0, 0, 0));
}

#[sqlx::test(migrations = "../../migrations")]
async fn event_access_is_proposal_organization_and_original_actor_scoped(pool: PgPool) {
    let (f, _, _, event) = publication(&pool).await;
    let reader = user(&pool, f.org, "READER", vec![]).await;
    let editor = user(&pool, f.org, "CONTRIBUTOR", vec![]).await;
    sqlx::query("INSERT INTO proposal_collaborators(organization_id,proposal_id,user_id,access) VALUES($1,$2,$3,'EDIT')").bind(f.org).bind(f.proposal).bind(editor).execute(&pool).await.unwrap();
    for actor in [reader, editor] {
        assert!(matches!(
            PgPrSync::new(pool.clone())
                .reserve(actor, f.proposal, event, &site())
                .await,
            Err(PrSyncStoreError::Access(SubmissionError::Forbidden))
        ));
    }
    let foreign = fixture(&pool).await;
    assert!(matches!(
        PgPrSync::new(pool.clone())
            .reserve(foreign.owner, foreign.proposal, event, &site())
            .await,
        Err(PrSyncStoreError::InvalidEvent)
    ));
    assert!(matches!(
        PgPrSync::new(pool.clone())
            .reserve(foreign.owner, f.proposal, event, &site())
            .await,
        Err(PrSyncStoreError::Access(SubmissionError::NotFound))
    ));
    assert_eq!(counts(&pool).await, (0, 0, 0, 0));
}

#[sqlx::test(migrations = "../../migrations")]
async fn stored_intent_event_and_scope_cannot_be_rewritten_or_completed_by_generic_workers(
    pool: PgPool,
) {
    let (f, _, _, event) = publication(&pool).await;
    let receipt = PgPrSync::new(pool.clone())
        .reserve(f.owner, f.proposal, event, &site())
        .await
        .unwrap();
    for statement in [
        "UPDATE pr_sync_intents SET body=body||'tampered' WHERE operation_id=$1",
        "DELETE FROM pr_sync_intents WHERE operation_id=$1",
        "UPDATE git_operations SET expected_sha=repeat('9',40) WHERE id=$1",
        "UPDATE git_operations SET state='SUCCEEDED' WHERE id=$1",
        "UPDATE jobs SET payload='{}' WHERE operation_id=$1",
        "UPDATE jobs SET state='SUCCEEDED' WHERE operation_id=$1",
        "DELETE FROM jobs WHERE operation_id=$1",
        "UPDATE outbox SET payload='{}' WHERE id=(SELECT source_event FROM pr_sync_intents WHERE operation_id=$1)",
        "UPDATE outbox SET sent_at=NULL WHERE id=(SELECT source_event FROM pr_sync_intents WHERE operation_id=$1)",
        "DELETE FROM outbox WHERE id=(SELECT source_event FROM pr_sync_intents WHERE operation_id=$1)",
    ] {
        assert!(
            sqlx::query(statement)
                .bind(receipt.operation_id)
                .execute(&pool)
                .await
                .is_err(),
            "{statement}"
        );
    }
    let worker = Uuid::new_v4();
    sqlx::query("UPDATE jobs SET state='RUNNING',lease_owner=$2,leased_until=clock_timestamp()+interval '1 minute' WHERE id=$1").bind(receipt.job_id).bind(worker).execute(&pool).await.unwrap();
    assert!(
        !specspace_infrastructure::complete_job(
            &pool,
            receipt.job_id,
            worker,
            json!({"phase":"SYNCED"})
        )
        .await
        .unwrap()
    );
    assert_eq!(counts(&pool).await, (1, 1, 1, 1));
}

#[sqlx::test(migrations = "../../migrations")]
async fn failed_audit_rolls_back_intent_job_operation_and_delivery_then_retry_succeeds(
    pool: PgPool,
) {
    let (f, _, _, event) = publication(&pool).await;
    sqlx::query("CREATE FUNCTION reject_pr_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='PR_SYNC_RESERVED' THEN RAISE EXCEPTION 'synthetic audit failure'; END IF; RETURN NEW; END $$").execute(&pool).await.unwrap();
    sqlx::query("CREATE TRIGGER reject_pr_audit BEFORE INSERT ON audit_events FOR EACH ROW EXECUTE FUNCTION reject_pr_audit()").execute(&pool).await.unwrap();
    assert!(matches!(
        PgPrSync::new(pool.clone())
            .reserve(f.owner, f.proposal, event, &site())
            .await,
        Err(PrSyncStoreError::Unavailable)
    ));
    assert_eq!(counts(&pool).await, (0, 0, 0, 0));
    sqlx::query("DROP TRIGGER reject_pr_audit ON audit_events")
        .execute(&pool)
        .await
        .unwrap();
    PgPrSync::new(pool.clone())
        .reserve(f.owner, f.proposal, event, &site())
        .await
        .unwrap();
    assert_eq!(counts(&pool).await, (1, 1, 1, 1));
}

#[sqlx::test(migrations = "../../migrations")]
async fn pr_work_does_not_block_content_review_but_serializes_the_next_pr_projection(pool: PgPool) {
    let (f, _, preparer) = setup(&pool).await;
    let (lease, verified) = prepared(&pool, &f, &preparer, 2).await;
    PgReviewSnapshots::new(pool.clone())
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
    PgPrSync::new(pool.clone())
        .reserve(f.owner, f.proposal, event, &site())
        .await
        .unwrap();
    let (lease, verified) = prepared(&pool, &f, &preparer, 3).await;
    PgReviewSnapshots::new(pool.clone())
        .publish(&lease, &verified, &preparer)
        .await
        .unwrap();
    let second:Uuid=sqlx::query_scalar("SELECT id FROM outbox WHERE entity_id=$1 AND event_type='REVIEW_SNAPSHOT_PUBLISHED' AND id<>$2").bind(f.proposal).bind(event).fetch_one(&pool).await.unwrap();
    assert!(matches!(
        PgPrSync::new(pool.clone())
            .reserve(f.owner, f.proposal, second, &site())
            .await,
        Err(PrSyncStoreError::Busy)
    ));
    assert!(
        PgPrSync::new(pool.clone())
            .reserve(f.owner, f.proposal, event, &site())
            .await
            .unwrap()
            .replayed
    );
    assert_eq!(counts(&pool).await, (1, 1, 1, 1));
}

#[sqlx::test(migrations = "../../migrations")]
async fn database_rejects_unbacked_pr_operations_jobs_and_unrelated_deletes_still_work(
    pool: PgPool,
) {
    let (f, _, _, _) = publication(&pool).await;
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("INSERT INTO git_operations(id,organization_id,proposal_id,actor_id,kind,idempotency_key,request_hash) VALUES($1,$2,$3,$4,'PR_SYNC','synthetic-unbacked',repeat('a',64))")
        .bind(Uuid::new_v4()).bind(f.org).bind(f.proposal).bind(f.owner).execute(&mut *tx).await.unwrap();
    assert!(tx.commit().await.is_err());
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("INSERT INTO jobs(id,organization_id,kind) VALUES($1,$2,'SYNC_REVIEW_PR')")
        .bind(Uuid::new_v4())
        .bind(f.org)
        .execute(&mut *tx)
        .await
        .unwrap();
    assert!(tx.commit().await.is_err());
    let job = Uuid::new_v4();
    let event = Uuid::new_v4();
    sqlx::query("INSERT INTO jobs(id,organization_id,kind) VALUES($1,$2,'SYNTHETIC_UNRELATED')")
        .bind(job)
        .bind(f.org)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO outbox(id,organization_id,event_type,entity_id) VALUES($1,$2,'SYNTHETIC_UNRELATED',$3)").bind(event).bind(f.org).bind(f.proposal).execute(&pool).await.unwrap();
    assert_eq!(
        sqlx::query("DELETE FROM jobs WHERE id=$1")
            .bind(job)
            .execute(&pool)
            .await
            .unwrap()
            .rows_affected(),
        1
    );
    assert_eq!(
        sqlx::query("DELETE FROM outbox WHERE id=$1")
            .bind(event)
            .execute(&pool)
            .await
            .unwrap()
            .rows_affected(),
        1
    );
    assert_eq!(counts(&pool).await, (0, 0, 0, 0));
}

#[sqlx::test(migrations = "../../migrations")]
async fn invalid_projection_is_not_delivered_and_valid_links_are_captured_exactly(pool: PgPool) {
    let (f, _, _, event) = publication(&pool).await;
    for url in [
        "http://internal.example/ticket",
        "https://user:secret@example.com/ticket",
    ] {
        sqlx::query("UPDATE proposals SET links=$2 WHERE id=$1")
            .bind(f.proposal)
            .bind(json!([{"kind":"ticket","url":url}]))
            .execute(&pool)
            .await
            .unwrap();
        assert!(matches!(
            PgPrSync::new(pool.clone())
                .reserve(f.owner, f.proposal, event, &site())
                .await,
            Err(PrSyncStoreError::Projection(_))
        ));
        assert_eq!(counts(&pool).await, (0, 0, 0, 0));
    }
    sqlx::query("UPDATE proposals SET links=$2 WHERE id=$1")
        .bind(f.proposal)
        .bind(json!([{"kind":"ticket","url":"https://tracker.example/T-42"}]))
        .execute(&pool)
        .await
        .unwrap();
    let saved = PgPrSync::new(pool.clone())
        .reserve(f.owner, f.proposal, event, &site())
        .await
        .unwrap();
    assert!(
        saved
            .intent()
            .content()
            .body()
            .contains("https://tracker.example/T-42")
    );
    sqlx::query("UPDATE proposals SET links='[]' WHERE id=$1")
        .bind(f.proposal)
        .execute(&pool)
        .await
        .unwrap();
    let replay = PgPrSync::new(pool.clone())
        .reserve(f.owner, f.proposal, event, &site())
        .await
        .unwrap();
    assert_eq!(
        saved.intent().request_hash(),
        replay.intent().request_hash()
    );
}

// Create another real proposal in the same organization/actor admission scope,
// using the synthetic provider's same immutable trees in another repository.
struct SiblingFactory {
    source: Arc<Source>,
    repository: Uuid,
    remote: u64,
}
#[async_trait]
impl BoundProviderFactory for SiblingFactory {
    async fn provider_bound(
        &self,
        binding: &RepositoryBinding,
    ) -> Result<Arc<dyn GitProvider>, ComparisonReadError> {
        assert_eq!(binding.repository, self.repository);
        assert_eq!(
            (binding.remote_id, binding.installation_id),
            (self.remote, 7)
        );
        Ok(self.source.clone())
    }
}
async fn sibling_publication(
    pool: &PgPool,
    first: &Fixture,
    source: Arc<Source>,
) -> (Fixture, Uuid) {
    let f = Fixture {
        org: first.org,
        repository: Uuid::new_v4(),
        proposal: Uuid::new_v4(),
        owner: first.owner,
        product: first.product,
        development: first.development,
    };
    let remote = (f.repository.as_u128() as u64).max(100);
    sqlx::query("INSERT INTO repositories(id,organization_id,provider,remote_id,installation_id) VALUES($1,$2,'GITHUB',$3,'7')").bind(f.repository).bind(f.org).bind(remote.to_string()).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO proposals(id,organization_id,repository_id,title,branch,owner_id,author_id,team_id,state,description,ui_impact,base_sha,head_sha,target_sha) SELECT $1,organization_id,$2,title,branch,owner_id,author_id,team_id,'DRAFT',description,'UNDECIDED',base_sha,head_sha,target_sha FROM proposals WHERE id=$3")
        .bind(f.proposal).bind(f.repository).bind(first.proposal).execute(pool).await.unwrap();
    PgReviewConfiguration::new(pool.clone())
        .update(
            f.owner,
            f.proposal,
            ReviewConfigurationRequest {
                request_id: Uuid::new_v4(),
                expected_head_sha: sha(1).as_str().into(),
                expected_proposal_version: 1,
                expected_configuration_version: 1,
                settings: ReviewSettings {
                    ui_impact: UiImpact::No,
                    ui_impact_reason: Some("No UI change".into()),
                    assignments: vec![
                        ReviewerSelection {
                            reviewer_id: f.product,
                            domain: ReviewArea::Product,
                            required: true,
                        },
                        ReviewerSelection {
                            reviewer_id: f.development,
                            domain: ReviewArea::Development,
                            required: true,
                        },
                    ],
                },
            },
        )
        .await
        .unwrap();
    let factory = Arc::new(SiblingFactory {
        source,
        repository: f.repository,
        remote,
    });
    let reader = Arc::new(
        ComparisonReader::new(
            Arc::new(PgComparisonAuthorization::new(pool.clone())),
            factory.clone(),
            Duration::from_secs(30),
        )
        .unwrap(),
    );
    let preparer = ReviewPreparer::new(
        Arc::new(PgReviewAuthorization::new(pool.clone())),
        reader,
        factory,
        Duration::from_secs(30),
    )
    .unwrap();
    let (lease, verified) = prepared(pool, &f, &preparer, 2).await;
    PgReviewSnapshots::new(pool.clone())
        .publish(&lease, &verified, &preparer)
        .await
        .unwrap();
    let event = sqlx::query_scalar(
        "SELECT id FROM outbox WHERE entity_id=$1 AND event_type='REVIEW_SNAPSHOT_PUBLISHED'",
    )
    .bind(f.proposal)
    .fetch_one(pool)
    .await
    .unwrap();
    (f, event)
}

#[sqlx::test(migrations = "../../migrations")]
async fn active_actor_quota_limits_new_events_without_breaking_exact_replay(pool: PgPool) {
    let (f, source, _, event) = publication(&pool).await;
    let first = PgPrSync::new(pool.clone())
        .reserve(f.owner, f.proposal, event, &site())
        .await
        .unwrap();
    let (second, event2) = sibling_publication(&pool, &f, source.clone()).await;
    PgPrSync::new(pool.clone())
        .reserve(second.owner, second.proposal, event2, &site())
        .await
        .unwrap();
    let (third, event3) = sibling_publication(&pool, &f, source).await;
    assert!(matches!(
        PgPrSync::new(pool.clone())
            .reserve(third.owner, third.proposal, event3, &site())
            .await,
        Err(PrSyncStoreError::QuotaExceeded)
    ));
    let replay = PgPrSync::new(pool.clone())
        .reserve(f.owner, f.proposal, event, &site())
        .await
        .unwrap();
    assert_eq!(replay.operation_id, first.operation_id);
    assert!(replay.replayed);
    assert_eq!(counts(&pool).await, (2, 2, 2, 2));
}
