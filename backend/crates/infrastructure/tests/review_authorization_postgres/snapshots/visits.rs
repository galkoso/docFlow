use super::*;
use serde_json::Value;
use specspace_application::{
    comparison_file::{
        ComparisonFileReader, discussion::AnchorSide, review_unit::ReviewUnitReader,
    },
    review_configuration::ReviewArea,
    review_visits::*,
};
use specspace_infrastructure::{
    official_reviews::OfficialReviewError,
    review_files::{PgReviewFileAuthorization, PgReviewUnitAuthorization},
    review_visits::{PgReviewVisits, VisitError},
};
struct Ready {
    f: Fixture,
    source: Arc<Source>,
    snapshot: Uuid,
    request: ReviewSourceRequest,
    store: PgReviewVisits,
}
fn store(pool: &PgPool, factory: Arc<dyn BoundProviderFactory>) -> PgReviewVisits {
    let files = Arc::new(
        ComparisonFileReader::new(
            Arc::new(PgReviewFileAuthorization::new(pool.clone())),
            factory,
            Duration::from_secs(60),
        )
        .unwrap(),
    );
    PgReviewVisits::new(
        pool.clone(),
        Arc::new(ReviewUnitReader::new(
            files,
            Arc::new(PgReviewUnitAuthorization::new(pool.clone())),
        )),
    )
}
async fn ready(pool: &PgPool) -> Ready {
    let (f, source, preparer) = setup_media(pool, false).await;
    let (lease, verified) = prepared(pool, &f, &preparer, 2).await;
    let saved = PgReviewSnapshots::new(pool.clone())
        .publish(&lease, &verified, &preparer)
        .await
        .unwrap();
    let request = ReviewSourceRequest {
        expected_fingerprint: saved.fingerprint,
        domain: ReviewArea::Product,
        unit_id: verified.candidate().plan().required_units()[0].id.clone(),
        side: AnchorSide::After,
        offset: 0,
    };
    let service = store(
        pool,
        Arc::new(Factory {
            source: source.clone(),
            repository: f.repository,
        }),
    );
    Ready {
        f,
        source,
        snapshot: saved.snapshot_id,
        request,
        store: service,
    }
}
fn ack(w: &ReviewSourceWindow) -> ReviewVisitRequest {
    ReviewVisitRequest {
        request_id: Uuid::new_v4(),
        delivery_id: w.delivery_id,
        expected_fingerprint: w.fingerprint.clone(),
        domain: w.domain,
    }
}
async fn deliver(r: &Ready) -> Result<ReviewSourceWindow, VisitError> {
    r.store
        .deliver(r.f.product, r.f.proposal, r.snapshot, r.request.clone())
        .await
}
async fn counts(pool: &PgPool) -> (i64, i64, i64, i64) {
    sqlx::query_as("SELECT (SELECT count(*) FROM review_source_attempts),(SELECT count(*) FROM review_source_deliveries),(SELECT count(*) FROM review_source_visits),(SELECT count(*) FROM official_reviews)").fetch_one(pool).await.unwrap()
}
#[sqlx::test(migrations = "../../migrations")]
async fn source_delivery_is_not_a_visit_and_exact_ack_replays_without_source_io(pool: PgPool) {
    let r = ready(&pool).await;
    let w = deliver(&r).await.unwrap();
    assert!(w.text.contains("Saved text-only proposal"));
    assert_eq!(
        w.window_sha256,
        format!("{:x}", Sha256::digest(w.text.as_bytes()))
    );
    assert_eq!(w.actor_id, r.f.product);
    assert_eq!(w.snapshot_id, r.snapshot);
    assert_eq!(w.offset, 0);
    assert_eq!(counts(&pool).await, (1, 1, 0, 0));
    let body = ack(&w);
    let reads = r.source.reads.load(Ordering::SeqCst);
    let (a, b) = tokio::join!(
        r.store
            .acknowledge(r.f.product, r.f.proposal, r.snapshot, body.clone()),
        r.store
            .acknowledge(r.f.product, r.f.proposal, r.snapshot, body.clone())
    );
    let a = a.unwrap();
    let b = b.unwrap();
    assert_eq!(a.id, b.id);
    assert_ne!(a.replayed, b.replayed);
    assert_eq!(a.recorded_at, b.recorded_at);
    assert_eq!((a.start_byte, a.end_byte), (w.start_byte, w.end_byte));
    assert_eq!(counts(&pool).await, (1, 1, 1, 0));
    assert_eq!(r.source.reads.load(Ordering::SeqCst), reads);
    let saved: Value =
        sqlx::query_scalar("SELECT to_jsonb(d) FROM review_source_deliveries d WHERE id=$1")
            .bind(w.delivery_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!saved.to_string().contains("Saved text-only proposal"));
    for table in [
        "review_source_attempts",
        "review_source_deliveries",
        "review_source_visits",
    ] {
        for sql in [
            format!("DELETE FROM {table}"),
            format!("UPDATE {table} SET id=gen_random_uuid()"),
            format!("TRUNCATE {table} CASCADE"),
        ] {
            assert!(sqlx::query(&sql).execute(&pool).await.is_err());
        }
    }
    assert!(
        sqlx::query("UPDATE outbox SET payload='{}' WHERE event_type='REVIEW_SOURCE_ACKNOWLEDGED'")
            .execute(&pool)
            .await
            .is_err()
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn source_admission_requires_current_qualified_assignment_and_exact_context_before_io(
    pool: PgPool,
) {
    let r = ready(&pool).await;
    let reads = r.source.reads.load(Ordering::SeqCst);
    for actor in [
        r.f.owner,
        r.f.development,
        user(&pool, r.f.org, "REVIEWER", vec!["PRODUCT"]).await,
    ] {
        assert!(
            r.store
                .deliver(actor, r.f.proposal, r.snapshot, r.request.clone())
                .await
                .is_err()
        );
    }
    let mut request = r.request.clone();
    request.expected_fingerprint = "a".repeat(64);
    assert!(matches!(
        r.store
            .deliver(r.f.product, r.f.proposal, r.snapshot, request)
            .await,
        Err(VisitError::Authority(OfficialReviewError::Stale))
    ));
    let mut request = r.request.clone();
    request.offset = 2097153;
    assert!(
        r.store
            .deliver(r.f.product, r.f.proposal, r.snapshot, request)
            .await
            .is_err()
    );
    assert_eq!(counts(&pool).await, (0, 0, 0, 0));
    assert_eq!(r.source.reads.load(Ordering::SeqCst), reads);
}
#[sqlx::test(migrations = "../../migrations")]
async fn acknowledgements_bind_actor_domain_nonce_delivery_and_current_snapshot(pool: PgPool) {
    let r = ready(&pool).await;
    let w = deliver(&r).await.unwrap();
    let body = ack(&w);
    assert!(
        r.store
            .acknowledge(r.f.development, r.f.proposal, r.snapshot, body.clone())
            .await
            .is_err()
    );
    let mut changed = body.clone();
    changed.domain = ReviewArea::Development;
    assert!(
        r.store
            .acknowledge(r.f.product, r.f.proposal, r.snapshot, changed)
            .await
            .is_err()
    );
    r.store
        .acknowledge(r.f.product, r.f.proposal, r.snapshot, body.clone())
        .await
        .unwrap();
    let w2 = deliver(&r).await.unwrap();
    let mut changed = body.clone();
    changed.delivery_id = w2.delivery_id;
    assert!(matches!(
        r.store
            .acknowledge(r.f.product, r.f.proposal, r.snapshot, changed)
            .await,
        Err(VisitError::Conflict)
    ));
    assert!(matches!(
        r.store
            .acknowledge(r.f.product, r.f.proposal, r.snapshot, ack(&w))
            .await,
        Err(VisitError::Conflict)
    ));
    sqlx::query("UPDATE proposals SET head_sha=$2,version=version+1 WHERE id=$1")
        .bind(r.f.proposal)
        .bind(sha(3).as_str())
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        r.store
            .acknowledge(r.f.product, r.f.proposal, r.snapshot, body)
            .await
            .is_err()
    );
    assert!(deliver(&r).await.is_err());
    assert_eq!(counts(&pool).await, (2, 2, 1, 0));
}
#[sqlx::test(migrations = "../../migrations")]
async fn failed_source_reads_are_charged_and_provider_backoff_blocks_new_io(pool: PgPool) {
    let r = ready(&pool).await;
    *r.source.failure.lock().unwrap() = Some(GitError::RateLimited {
        retry_after_seconds: Some(90),
    });
    assert!(matches!(deliver(&r).await, Err(VisitError::Backoff(90))));
    let reads = r.source.reads.load(Ordering::SeqCst);
    assert!(matches!(
        deliver(&r).await,
        Err(VisitError::Backoff(1..=90))
    ));
    assert_eq!(r.source.reads.load(Ordering::SeqCst), reads);
    assert_eq!(counts(&pool).await, (1, 0, 0, 0));
}
#[sqlx::test(migrations = "../../migrations")]
async fn source_acknowledgement_rolls_back_on_outbox_failure_and_expiry_does_not_revoke_recorded_replay(
    pool: PgPool,
) {
    let r = ready(&pool).await;
    let w = deliver(&r).await.unwrap();
    let body = ack(&w);
    sqlx::raw_sql("CREATE FUNCTION fail_visit_event() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.event_type='REVIEW_SOURCE_ACKNOWLEDGED' THEN RAISE EXCEPTION 'injected'; END IF; RETURN NEW; END $$; CREATE TRIGGER fail_visit_event BEFORE INSERT ON outbox FOR EACH ROW EXECUTE FUNCTION fail_visit_event();").execute(&pool).await.unwrap();
    assert!(
        r.store
            .acknowledge(r.f.product, r.f.proposal, r.snapshot, body.clone())
            .await
            .is_err()
    );
    assert_eq!(counts(&pool).await, (1, 1, 0, 0));
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM audit_events WHERE action='REVIEW_SOURCE_ACKNOWLEDGED'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        0
    );
    sqlx::query("DROP TRIGGER fail_visit_event ON outbox")
        .execute(&pool)
        .await
        .unwrap();
    let receipt = r
        .store
        .acknowledge(r.f.product, r.f.proposal, r.snapshot, body.clone())
        .await
        .unwrap();
    let unacknowledged = deliver(&r).await.unwrap();
    // Move test evidence into the past explicitly, bypassing only its append-only
    // update trigger. Production callers cannot extend or rewrite these records.
    sqlx::raw_sql("ALTER TABLE review_source_deliveries DISABLE TRIGGER review_source_deliveries_immutable; UPDATE review_source_deliveries SET created_at=clock_timestamp()-interval '31 minutes',expires_at=clock_timestamp()-interval '1 minute'; ALTER TABLE review_source_deliveries ENABLE TRIGGER review_source_deliveries_immutable;").execute(&pool).await.unwrap();
    assert!(matches!(
        r.store
            .acknowledge(r.f.product, r.f.proposal, r.snapshot, ack(&unacknowledged))
            .await,
        Err(VisitError::Expired)
    ));
    let replay = r
        .store
        .acknowledge(r.f.product, r.f.proposal, r.snapshot, body)
        .await
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.id, receipt.id);
}
struct RevokingFactory {
    pool: PgPool,
    actor: Uuid,
    inner: Factory,
}
#[async_trait]
impl BoundProviderFactory for RevokingFactory {
    async fn provider_bound(
        &self,
        binding: &RepositoryBinding,
    ) -> Result<Arc<dyn GitProvider>, ComparisonReadError> {
        sqlx::query("UPDATE memberships SET role='READER' WHERE user_id=$1")
            .bind(self.actor)
            .execute(&self.pool)
            .await
            .unwrap();
        self.inner.provider_bound(binding).await
    }
}
#[sqlx::test(migrations = "../../migrations")]
async fn role_loss_during_provider_connection_prevents_delivery_even_when_source_remains_readable(
    pool: PgPool,
) {
    let r = ready(&pool).await;
    let service = store(
        &pool,
        Arc::new(RevokingFactory {
            pool: pool.clone(),
            actor: r.f.product,
            inner: Factory {
                source: r.source.clone(),
                repository: r.f.repository,
            },
        }),
    );
    assert!(matches!(
        service
            .deliver(r.f.product, r.f.proposal, r.snapshot, r.request.clone())
            .await,
        Err(VisitError::Authority(OfficialReviewError::Forbidden))
    ));
    assert_eq!(counts(&pool).await, (1, 0, 0, 0));
}

#[sqlx::test(migrations = "../../migrations")]
async fn source_attempt_quota_counts_prior_admissions_and_stops_io(pool: PgPool) {
    let r = ready(&pool).await;
    deliver(&r).await.unwrap();
    let reads = r.source.reads.load(Ordering::SeqCst);
    sqlx::query("INSERT INTO review_source_attempts(id,organization_id,proposal_id,snapshot_id,reviewer_id,domain,fingerprint,unit_id,side,start_offset) SELECT gen_random_uuid(),a.organization_id,a.proposal_id,a.snapshot_id,a.reviewer_id,a.domain,a.fingerprint,a.unit_id,a.side,a.start_offset FROM review_source_attempts a CROSS JOIN generate_series(1,599)").execute(&pool).await.unwrap();
    assert!(matches!(deliver(&r).await, Err(VisitError::Quota)));
    assert_eq!(r.source.reads.load(Ordering::SeqCst), reads);
    assert_eq!(counts(&pool).await, (600, 1, 0, 0));
}
#[sqlx::test(migrations = "../../migrations")]
async fn database_rejects_forged_delivery_ranges_and_visits_without_atomic_evidence(pool: PgPool) {
    let r = ready(&pool).await;
    let w = deliver(&r).await.unwrap();
    for fault in 0..3 {
        let mut tx = pool.begin().await.unwrap();
        let attempt = Uuid::new_v4();
        sqlx::query("INSERT INTO review_source_attempts(id,organization_id,proposal_id,snapshot_id,reviewer_id,domain,fingerprint,unit_id,side,start_offset) SELECT $2,a.organization_id,a.proposal_id,a.snapshot_id,a.reviewer_id,a.domain,a.fingerprint,a.unit_id,a.side,a.start_offset FROM review_source_attempts a JOIN review_source_deliveries d ON d.attempt_id=a.id WHERE d.id=$1")
   .bind(w.delivery_id).bind(attempt).execute(&mut *tx).await.unwrap();
        let mut selected = serde_json::to_value(&w.source).unwrap();
        if fault == 0 {
            selected["source"]["path"] = serde_json::json!("docs/forged.md");
        }
        let result=sqlx::query("INSERT INTO review_source_deliveries(id,attempt_id,source,start_byte,end_byte,window_sha256) VALUES($1,$2,$3,$4,$5,$6)")
   .bind(Uuid::new_v4()).bind(attempt).bind(selected).bind(w.start_byte as i32+if fault==1 {1}else{0}).bind(w.end_byte as i32+if fault==2 {1}else{0}).bind("a".repeat(64)).execute(&mut *tx).await;
        assert!(result.is_err());
        tx.rollback().await.unwrap();
    }
    let fresh = deliver(&r).await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("INSERT INTO review_source_visits(id,organization_id,delivery_id,request_id,request_hash,audit_id,event_id) VALUES($1,$2,$3,$4,$5,$6,$7)")
  .bind(Uuid::new_v4()).bind(r.f.org).bind(fresh.delivery_id).bind(Uuid::new_v4()).bind("a".repeat(64)).bind(Uuid::new_v4()).bind(Uuid::new_v4()).execute(&mut *tx).await.unwrap();
    assert!(tx.commit().await.is_err());
    assert_eq!(counts(&pool).await, (2, 2, 0, 0));
}
