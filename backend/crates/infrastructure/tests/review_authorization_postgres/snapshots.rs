use super::*;
use async_trait::async_trait;
use sha2::{Digest, Sha256};
use specspace_application::{
    comparison_reader::{BoundProviderFactory, ComparisonReader, RepositoryBinding},
    proposal::IdempotencyKey,
    review_preparation::{ReviewPreparer, VerifiedReview},
    review_submission::{ReviewSubmissionRequest, ValidatedReviewSubmission},
};
use specspace_domain::{BranchName, RepositoryPath};
use specspace_git_adapter::{
    CommitMetadata, CommitReceipt, CommitRequest, FileEntry, FileMode, GitError, GitProvider, blob,
};
use specspace_infrastructure::{
    comparison_authorization::PgComparisonAuthorization,
    review_snapshots::{PgReviewSnapshots, SnapshotError},
    review_submissions::{PgReviewSubmissions, SubmissionError, SubmissionLease},
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
#[path = "snapshots/files.rs"]
mod files;
#[path = "snapshots/official_reviews.rs"]
mod official_reviews;
#[path = "snapshots/pr_sync.rs"]
mod pr_sync;
#[path = "snapshots/worker.rs"]
mod worker;
struct Source {
    trees: BTreeMap<String, BTreeMap<String, Vec<u8>>>,
    reads: AtomicUsize,
    target: AtomicUsize,
    branch_head: AtomicUsize,
    failure: Mutex<Option<GitError>>,
    blocked: AtomicBool,
    started: tokio::sync::Notify,
    release: tokio::sync::Notify,
    invalid_media: AtomicBool,
    base_updated: AtomicBool,
}
fn sha(digit: usize) -> CommitSha {
    CommitSha::parse(digit.to_string().repeat(40)).unwrap()
}
#[async_trait]
impl GitProvider for Source {
    async fn main_head(&self) -> Result<CommitSha, GitError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.started.notify_one();
        if self.blocked.load(Ordering::SeqCst) {
            self.release.notified().await;
        }
        if let Some(error) = self.failure.lock().unwrap().clone() {
            return Err(error);
        }
        Ok(sha(self.target.load(Ordering::SeqCst)))
    }
    async fn branch_head(&self, branch: &BranchName) -> Result<CommitSha, GitError> {
        assert_eq!(branch.as_str(), "change/review-scope");
        Ok(sha(self.branch_head.load(Ordering::SeqCst)))
    }
    async fn create_branch(&self, _: &BranchName, _: &CommitSha) -> Result<CommitSha, GitError> {
        panic!("snapshot publication cannot create Git branches")
    }
    async fn commit(&self, _: CommitRequest<'_>) -> Result<CommitSha, GitError> {
        panic!("snapshot publication cannot write Git")
    }
    async fn read_text(
        &self,
        commit: &CommitSha,
        path: &RepositoryPath,
    ) -> Result<String, GitError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        Ok(String::from_utf8(self.trees[commit.as_str()][path.as_str()].clone()).unwrap())
    }
    async fn read_bytes(
        &self,
        commit: &CommitSha,
        path: &RepositoryPath,
        max: usize,
    ) -> Result<Vec<u8>, GitError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        if self.invalid_media.load(Ordering::SeqCst) {
            return Ok(vec![0]);
        }
        let bytes = self.trees[commit.as_str()][path.as_str()].clone();
        assert!(bytes.len() <= max);
        Ok(bytes)
    }
    async fn list_files(&self, commit: &CommitSha) -> Result<Vec<FileEntry>, GitError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        Ok(self.trees[commit.as_str()]
            .iter()
            .map(|(path, bytes)| FileEntry {
                path: RepositoryPath::parse(path).unwrap(),
                blob_sha: blob::oid(bytes, 40).unwrap(),
                size: bytes.len() as u64,
                mode: FileMode::Regular,
            })
            .collect())
    }
    async fn commit_metadata(&self, _: &CommitSha) -> Result<CommitMetadata, GitError> {
        panic!("no metadata reads needed")
    }
    async fn commit_receipt(&self, _: &CommitSha) -> Result<CommitReceipt, GitError> {
        panic!("no write receipts needed")
    }
    async fn merge_base(&self, left: &CommitSha, right: &CommitSha) -> Result<CommitSha, GitError> {
        assert_eq!(left, &sha(2));
        assert_eq!(right, &sha(1));
        Ok(if self.base_updated.load(Ordering::SeqCst) {
            sha(2)
        } else {
            sha(0)
        })
    }
    async fn is_ancestor(&self, a: &CommitSha, d: &CommitSha) -> Result<bool, GitError> {
        Ok(a == d
            || a == &sha(0)
            || self.base_updated.load(Ordering::SeqCst) && a == &sha(2) && d == &sha(1))
    }
}
struct Factory {
    source: Arc<Source>,
    repository: Uuid,
}
#[async_trait]
impl BoundProviderFactory for Factory {
    async fn provider_bound(
        &self,
        binding: &RepositoryBinding,
    ) -> Result<Arc<dyn GitProvider>, ComparisonReadError> {
        assert_eq!(binding.repository, self.repository);
        assert_eq!((binding.remote_id, binding.installation_id), (42, 7));
        Ok(self.source.clone())
    }
}
async fn setup(pool: &PgPool) -> (Fixture, Arc<Source>, ReviewPreparer) {
    setup_media(pool, true).await
}
async fn setup_media(pool: &PgPool, include_media: bool) -> (Fixture, Arc<Source>, ReviewPreparer) {
    setup_body(pool, include_media, None).await
}
async fn setup_body(
    pool: &PgPool,
    include_media: bool,
    body: Option<&str>,
) -> (Fixture, Arc<Source>, ReviewPreparer) {
    setup_body_options(pool, include_media, body, false, None).await
}
async fn setup_body_options(
    pool: &PgPool,
    include_media: bool,
    body: Option<&str>,
    reuse_media: bool,
    before_body: Option<&str>,
) -> (Fixture, Arc<Source>, ReviewPreparer) {
    let f = fixture(pool).await;
    sqlx::query("UPDATE proposals SET base_sha=$2,target_sha=$3 WHERE id=$1")
        .bind(f.proposal)
        .bind(sha(0).as_str())
        .bind(sha(2).as_str())
        .execute(pool)
        .await
        .unwrap();
    let catalog =
        br#"{"schema_version":1,"documents":[{"document_id":"DOC-A","path":"docs/a.md"}]}"#
            .to_vec();
    let tree = |body: &str| {
        BTreeMap::from([
        (".specspace/catalog.json".to_owned(),catalog.clone()),
        ("docs/a.md".to_owned(),format!("---\ndocument_id: DOC-A\ntitle: Synthetic\nowner_team: fixture\nschema_version: 1\n---\n<!-- ss:section:SEC-A -->\n# Synthetic\n\n{body}\n").into_bytes())])
    };
    let image=include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"),"/../../../fixtures/repository/assets/5e3d382db4dd83d59aa5742793ad6b7903409e865c83bcbc54835049f043bc15.png")).to_vec();
    let path = format!("assets/{:x}.png", Sha256::digest(&image));
    let mut before = tree(before_body.unwrap_or("Before"));
    if reuse_media {
        before.insert(path.clone(), image.clone());
    }
    let mut after = tree("Saved text-only proposal");
    if include_media {
        after = tree(&format!(
            "Saved proposal with verified media\n\n![Synthetic image]({path})"
        ));
        after.insert(path, image);
    }
    if let Some(body) = body {
        after = tree(body);
    }
    let source = Arc::new(Source {
        trees: BTreeMap::from([
            (sha(0).as_str().into(), before),
            (sha(1).as_str().into(), after),
            (sha(2).as_str().into(), tree("Main only change")),
        ]),
        reads: AtomicUsize::new(0),
        target: AtomicUsize::new(2),
        branch_head: AtomicUsize::new(1),
        failure: Mutex::new(None),
        blocked: AtomicBool::new(false),
        started: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
        invalid_media: AtomicBool::new(false),
        base_updated: AtomicBool::new(false),
    });
    let factory = Arc::new(Factory {
        source: source.clone(),
        repository: f.repository,
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
    (f, source, preparer)
}
async fn prepared(
    pool: &PgPool,
    f: &Fixture,
    preparer: &ReviewPreparer,
    version: i64,
) -> (SubmissionLease, VerifiedReview) {
    let store = PgReviewSubmissions::new(pool.clone());
    reserve(pool, f, version).await;
    let lease = store.claim(Uuid::new_v4(), 300).await.unwrap().unwrap();
    let bound = preparer.with_authorization(Arc::new(store.authorization(&lease)));
    let candidate = bound
        .prepare(f.owner, f.proposal, lease.expectation().clone())
        .await
        .unwrap();
    let verified = bound.verify_media(candidate).await.unwrap();
    (lease, verified)
}
async fn reserve(
    pool: &PgPool,
    f: &Fixture,
    version: i64,
) -> specspace_application::review_submission::ReviewSubmissionAccepted {
    let store = PgReviewSubmissions::new(pool.clone());
    let input = ValidatedReviewSubmission::new(
        f.owner,
        f.proposal,
        ReviewSubmissionRequest {
            expected_head_sha: sha(1).as_str().into(),
            expected_target_sha: sha(2).as_str().into(),
            expected_proposal_version: version,
            expected_configuration_version: 2,
            expected_policy_version: 1,
        },
    )
    .unwrap();
    store
        .reserve(
            &input,
            &IdempotencyKey::parse(Uuid::new_v4().to_string()).unwrap(),
        )
        .await
        .unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
async fn atomic_publication_preserves_complete_shared_evidence_and_private_job_status(
    pool: PgPool,
) {
    let (f, source, preparer) = setup(&pool).await;
    let (lease, verified) = prepared(&pool, &f, &preparer, 2).await;
    let store = PgReviewSnapshots::new(pool.clone());
    let result = store.publish(&lease, &verified, &preparer).await.unwrap();
    assert!(result.activated && !result.replayed);
    assert_eq!(result.proposal_version, 3);
    assert_eq!(result.fingerprint, verified.fingerprint());
    assert_eq!(result.evidence_digest, verified.evidence_digest());
    let reader = user(&pool, f.org, "READER", vec![]).await;
    let reads = source.reads.load(Ordering::SeqCst);
    let shared = store
        .get(reader, f.proposal, result.snapshot_id)
        .await
        .unwrap();
    assert!(shared.current_metadata);
    assert_eq!((shared.file_count, shared.media_count), (2, 1));
    assert_eq!(
        shared.metadata["assignments"],
        serde_json::to_value(verified.candidate().plan().assignments()).unwrap()
    );
    assert_eq!(shared.metadata["summary"]["base_outdated"], true);
    let mut files = vec![];
    for offset in 0..3 {
        let page = store
            .files(reader, f.proposal, result.snapshot_id, offset, 1)
            .await
            .unwrap();
        assert_eq!(page.total, 2);
        files.extend(page.files);
    }
    assert_eq!(
        serde_json::to_value(files).unwrap(),
        serde_json::to_value(verified.candidate().plan().files()).unwrap()
    );
    let receipt: serde_json::Value =
        sqlx::query_scalar("SELECT receipt FROM review_snapshot_media WHERE snapshot_id=$1")
            .bind(result.snapshot_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(receipt, serde_json::to_value(&verified.media()[0]).unwrap());
    let (state, version, head, target): (String, i64, String, String) =
        sqlx::query_as("SELECT state,version,head_sha,target_sha FROM proposals WHERE id=$1")
            .bind(f.proposal)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        (state.as_str(), version, head.as_str(), target.as_str()),
        ("IN_REVIEW", 3, sha(1).as_str(), sha(2).as_str())
    );
    let submissions = PgReviewSubmissions::new(pool.clone());
    let status = submissions.status(f.owner, lease.job_id()).await.unwrap();
    assert_eq!(status.phase, "SNAPSHOT_READY");
    assert_eq!(status.publication, Some(result.clone()));
    assert!(matches!(
        submissions.status(reader, lease.job_id()).await,
        Err(SubmissionError::NotFound)
    ));
    let events: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT payload FROM outbox WHERE event_type='REVIEW_SNAPSHOT_PUBLISHED'",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["pr_sync_required"], true);
    assert_eq!(events[0]["approvals_granted"], false);
    sqlx::query("UPDATE proposals SET state='CLOSED',version=version+1 WHERE id=$1")
        .bind(f.proposal)
        .execute(&pool)
        .await
        .unwrap();
    let replay = store.publish(&lease, &verified, &preparer).await.unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.snapshot_id, result.snapshot_id);
    assert_eq!(replay.proposal_version, 3);
    assert_eq!(source.reads.load(Ordering::SeqCst), reads);
    let outside = user(&pool, fixture(&pool).await.org, "ADMIN", vec![]).await;
    assert!(matches!(
        store.get(outside, f.proposal, result.snapshot_id).await,
        Err(SnapshotError::Submission(SubmissionError::NotFound))
    ));
    sqlx::query("DELETE FROM memberships WHERE organization_id=$1 AND user_id=$2")
        .bind(f.org)
        .bind(reader)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        store
            .get(reader, f.proposal, result.snapshot_id)
            .await
            .is_err()
    );
    sqlx::query("UPDATE memberships SET role='READER' WHERE organization_id=$1 AND user_id=$2")
        .bind(f.org)
        .bind(f.owner)
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        store.publish(&lease, &verified, &preparer).await,
        Err(SnapshotError::Submission(SubmissionError::Forbidden))
    ));
    assert!(matches!(
        submissions.status(f.owner, lease.job_id()).await,
        Err(SubmissionError::Forbidden)
    ));
}

#[sqlx::test(migrations = "../../migrations")]
async fn identical_active_snapshot_preserves_approved_state_but_history_never_reactivates(
    pool: PgPool,
) {
    let (f, _, preparer) = setup(&pool).await;
    let store = PgReviewSnapshots::new(pool.clone());
    let (lease, verified) = prepared(&pool, &f, &preparer, 2).await;
    let first = store.publish(&lease, &verified, &preparer).await.unwrap();
    sqlx::query("UPDATE proposals SET state='APPROVED' WHERE id=$1")
        .bind(f.proposal)
        .execute(&pool)
        .await
        .unwrap();
    let (lease, verified) = prepared(&pool, &f, &preparer, 3).await;
    let same = store.publish(&lease, &verified, &preparer).await.unwrap();
    assert_eq!(same.snapshot_id, first.snapshot_id);
    assert!(!same.activated);
    assert_eq!(same.proposal_version, 3);
    let state: String = sqlx::query_scalar("SELECT state FROM proposals WHERE id=$1")
        .bind(f.proposal)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state, "APPROVED");
    sqlx::query("UPDATE proposals SET head_sha=$2,version=4 WHERE id=$1")
        .bind(f.proposal)
        .bind(sha(3).as_str())
        .execute(&pool)
        .await
        .unwrap();
    let (pointer, state): (Option<Uuid>, String) =
        sqlx::query_as("SELECT active_review_snapshot_id,state FROM proposals WHERE id=$1")
            .bind(f.proposal)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(pointer.is_none());
    assert_eq!(state, "IN_REVIEW");
    sqlx::query("UPDATE proposals SET head_sha=$2,version=5 WHERE id=$1")
        .bind(f.proposal)
        .bind(sha(1).as_str())
        .execute(&pool)
        .await
        .unwrap();
    let (lease, verified) = prepared(&pool, &f, &preparer, 5).await;
    let new = store.publish(&lease, &verified, &preparer).await.unwrap();
    assert_eq!(new.fingerprint, first.fingerprint);
    assert_ne!(new.snapshot_id, first.snapshot_id);
    assert!(new.activated);
    assert_eq!(new.proposal_version, 6);
    assert!(
        !store
            .get(f.owner, f.proposal, first.snapshot_id)
            .await
            .unwrap()
            .current_metadata
    );
    assert!(
        store
            .get(f.owner, f.proposal, new.snapshot_id)
            .await
            .unwrap()
            .current_metadata
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn stale_authority_refs_and_expired_or_reclaimed_leases_publish_nothing(pool: PgPool) {
    for fault in 0..5 {
        let (f, source, preparer) = setup(&pool).await;
        let (lease, verified) = prepared(&pool, &f, &preparer, 2).await;
        match fault {
            0 => {
                source.target.store(3, Ordering::SeqCst);
            }
            1 => {
                sqlx::query("UPDATE proposals SET description='Changed without changing the expected head' WHERE id=$1").bind(f.proposal).execute(&pool).await.unwrap();
            }
            2 => {
                sqlx::query("DELETE FROM memberships WHERE organization_id=$1 AND user_id=$2")
                    .bind(f.org)
                    .bind(f.product)
                    .execute(&pool)
                    .await
                    .unwrap();
            }
            3 => {
                sqlx::query("UPDATE users SET status='DISABLED' WHERE id=$1")
                    .bind(f.owner)
                    .execute(&pool)
                    .await
                    .unwrap();
            }
            _ => {
                sqlx::query("UPDATE jobs SET leased_until=clock_timestamp()-interval '1 second' WHERE id=$1").bind(lease.job_id()).execute(&pool).await.unwrap();
                let fresh = PgReviewSubmissions::new(pool.clone())
                    .claim(Uuid::new_v4(), 300)
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(fresh.attempts(), 2);
            }
        }
        assert!(
            PgReviewSnapshots::new(pool.clone())
                .publish(&lease, &verified, &preparer)
                .await
                .is_err(),
            "fault {fault}"
        );
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM review_snapshots WHERE proposal_id=$1")
                .bind(f.proposal)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 0);
        PgReviewSubmissions::new(pool.clone())
            .fail(&lease, "TEST_CLEANUP", None)
            .await
            .ok();
        // A reclaimed job from this fixture must not be claimed by the next fixture.
        sqlx::query(
            "UPDATE jobs SET state='FAILED',lease_owner=NULL,leased_until=NULL WHERE id=$1",
        )
        .bind(lease.job_id())
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("UPDATE git_operations SET state='FAILED' WHERE id=$1")
            .bind(lease.operation_id())
            .execute(&pool)
            .await
            .unwrap();
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn outbox_failure_rolls_back_every_publication_write_and_safe_retry_succeeds(pool: PgPool) {
    let (f, _, preparer) = setup(&pool).await;
    let (lease, verified) = prepared(&pool, &f, &preparer, 2).await;
    sqlx::raw_sql("CREATE FUNCTION fail_review_event() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.event_type='REVIEW_SNAPSHOT_PUBLISHED' THEN RAISE EXCEPTION 'injected outbox failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER fail_review_event BEFORE INSERT ON outbox FOR EACH ROW EXECUTE FUNCTION fail_review_event()").execute(&pool).await.unwrap();
    let store = PgReviewSnapshots::new(pool.clone());
    assert!(store.publish(&lease, &verified, &preparer).await.is_err());
    let (snapshots,results,version,state):(i64,i64,i64,String)=sqlx::query_as("SELECT (SELECT count(*) FROM review_snapshots),(SELECT count(*) FROM review_submission_results),version,state FROM proposals WHERE id=$1").bind(f.proposal).fetch_one(&pool).await.unwrap();
    assert_eq!(
        (snapshots, results, version, state.as_str()),
        (0, 0, 2, "DRAFT")
    );
    sqlx::query("DROP TRIGGER fail_review_event ON outbox")
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        store
            .publish(&lease, &verified, &preparer)
            .await
            .unwrap()
            .activated
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn sealed_evidence_and_results_cannot_be_mutated_or_extended(pool: PgPool) {
    let (f, _, preparer) = setup(&pool).await;
    let (lease, verified) = prepared(&pool, &f, &preparer, 2).await;
    let store = PgReviewSnapshots::new(pool.clone());
    let published = store.publish(&lease, &verified, &preparer).await.unwrap();
    for statement in [
        "UPDATE review_snapshots SET title='Different title'",
        "DELETE FROM review_snapshots",
        "UPDATE review_snapshot_files SET ordinal=ordinal+10",
        "DELETE FROM review_snapshot_files",
        "UPDATE review_snapshot_media SET path=path",
        "DELETE FROM review_snapshot_media",
        "UPDATE review_submission_results SET applied_version=999",
        "DELETE FROM review_submission_results",
        "TRUNCATE review_snapshots CASCADE",
        "TRUNCATE review_snapshot_files",
        "TRUNCATE review_snapshot_media",
        "TRUNCATE review_submission_results",
    ] {
        assert!(
            sqlx::query(statement).execute(&pool).await.is_err(),
            "{statement}"
        );
    }
    assert!(sqlx::query("INSERT INTO review_snapshot_files SELECT organization_id,proposal_id,snapshot_id,30,repeat('a',64),jsonb_set(change,'{id}',to_jsonb(repeat('a',64))) FROM review_snapshot_files LIMIT 1").execute(&pool).await.is_err());
    assert!(
        store
            .files(f.owner, f.proposal, published.snapshot_id, -1, 10)
            .await
            .is_err()
    );
    assert!(
        store
            .files(f.owner, f.proposal, published.snapshot_id, 0, 101)
            .await
            .is_err()
    );
    // Provider reconfiguration does not prevent reading immutable history.
    sqlx::query("UPDATE repositories SET remote_id='unavailable' WHERE id=$1")
        .bind(f.repository)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        !store
            .get(f.owner, f.proposal, published.snapshot_id)
            .await
            .unwrap()
            .current_metadata
    );
    sqlx::query("UPDATE repositories SET remote_id='42' WHERE id=$1")
        .bind(f.repository)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        !store
            .get(f.owner, f.proposal, published.snapshot_id)
            .await
            .unwrap()
            .current_metadata
    );
    sqlx::query("UPDATE organizations SET policy_version=2 WHERE id=$1")
        .bind(f.org)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        sqlx::query("UPDATE organizations SET policy_version=1 WHERE id=$1")
            .bind(f.org)
            .execute(&pool)
            .await
            .is_err()
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn concurrent_publication_returns_one_durable_outcome(pool: PgPool) {
    let (f, _, preparer) = setup(&pool).await;
    let (lease, verified) = prepared(&pool, &f, &preparer, 2).await;
    let store = PgReviewSnapshots::new(pool.clone());
    let (a, b) = tokio::join!(
        store.publish(&lease, &verified, &preparer),
        store.publish(&lease, &verified, &preparer)
    );
    let a = a.unwrap();
    let b = b.unwrap();
    assert_eq!(a.snapshot_id, b.snapshot_id);
    assert_ne!(a.replayed, b.replayed);
    let (snapshots,results,events):(i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM review_snapshots),(SELECT count(*) FROM review_submission_results),(SELECT count(*) FROM outbox WHERE event_type='REVIEW_SNAPSHOT_PUBLISHED')").fetch_one(&pool).await.unwrap();
    assert_eq!((snapshots, results, events), (1, 1, 1));
}

#[sqlx::test(migrations = "../../migrations")]
async fn expiry_after_writes_is_fenced_and_rolls_back_without_waiting_for_a_timer(pool: PgPool) {
    let (f, _, preparer) = setup(&pool).await;
    let (lease, verified) = prepared(&pool, &f, &preparer, 2).await;
    // Expire the lease at the outbox boundary, after header/files/result/pointer
    // writes. The final clock fence must undo every one of those writes.
    sqlx::raw_sql("CREATE FUNCTION expire_review_lease() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.event_type='REVIEW_SNAPSHOT_PUBLISHED' THEN UPDATE jobs SET leased_until=clock_timestamp()-interval '1 second' WHERE operation_id=(NEW.payload->>'operation_id')::uuid; END IF; RETURN NEW; END $$; CREATE TRIGGER expire_review_lease BEFORE INSERT ON outbox FOR EACH ROW EXECUTE FUNCTION expire_review_lease()").execute(&pool).await.unwrap();
    let result = PgReviewSnapshots::new(pool.clone())
        .publish(&lease, &verified, &preparer)
        .await;
    assert!(matches!(
        result,
        Err(SnapshotError::Submission(SubmissionError::LeaseLost))
    ));
    let (snapshots,results,version,state):(i64,i64,i64,String)=sqlx::query_as("SELECT (SELECT count(*) FROM review_snapshots),(SELECT count(*) FROM review_submission_results),version,state FROM proposals WHERE id=$1").bind(f.proposal).fetch_one(&pool).await.unwrap();
    assert_eq!(
        (snapshots, results, version, state.as_str()),
        (0, 0, 2, "DRAFT")
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn database_refuses_incomplete_headers_missing_files_and_activation_without_a_result(
    pool: PgPool,
) {
    let (f, _, preparer) = setup(&pool).await;
    let (lease, verified) = prepared(&pool, &f, &preparer, 2).await;
    let published = PgReviewSnapshots::new(pool.clone())
        .publish(&lease, &verified, &preparer)
        .await
        .unwrap();
    let (next, _) = prepared(&pool, &f, &preparer, 3).await;
    let copy = "INSERT INTO review_snapshots SELECT (jsonb_populate_record(NULL::review_snapshots,to_jsonb(h)||jsonb_build_object('id',$2::uuid,'created_operation',$3::uuid,'state','PUBLISHING','prepared_version',3,'metadata',jsonb_set(h.metadata,'{summary,expected_proposal_version}','3'::jsonb)))).* FROM review_snapshots h WHERE id=$1";
    for fault in 0..5 {
        let mut tx = pool.begin().await.unwrap();
        let id = Uuid::new_v4();
        sqlx::query(copy)
            .bind(published.snapshot_id)
            .bind(id)
            .bind(next.operation_id())
            .execute(&mut *tx)
            .await
            .unwrap();
        if fault == 2 {
            assert!(sqlx::query("INSERT INTO review_snapshot_media SELECT organization_id,proposal_id,$2,ordinal,path,jsonb_set(receipt,'{display_width}','null'::jsonb) FROM review_snapshot_media WHERE snapshot_id=$1").bind(published.snapshot_id).bind(id).execute(&mut *tx).await.is_err());
            tx.rollback().await.unwrap();
            continue;
        }
        if fault >= 3 {
            sqlx::query("INSERT INTO review_snapshot_files SELECT organization_id,proposal_id,$2,ordinal,file_id,change FROM review_snapshot_files WHERE snapshot_id=$1").bind(published.snapshot_id).bind(id).execute(&mut *tx).await.unwrap();
        }
        if fault == 4 {
            sqlx::query("INSERT INTO review_snapshot_media SELECT organization_id,proposal_id,$2,ordinal,path,receipt FROM review_snapshot_media WHERE snapshot_id=$1").bind(published.snapshot_id).bind(id).execute(&mut *tx).await.unwrap();
            sqlx::query("UPDATE review_snapshots SET state='READY' WHERE id=$1")
                .bind(id)
                .execute(&mut *tx)
                .await
                .unwrap();
            assert!(tx.commit().await.is_err());
            continue;
        }
        if fault == 1 || fault == 3 {
            assert!(
                sqlx::query("UPDATE review_snapshots SET state='READY' WHERE id=$1")
                    .bind(id)
                    .execute(&mut *tx)
                    .await
                    .is_err()
            );
            tx.rollback().await.unwrap();
        } else {
            assert!(tx.commit().await.is_err());
        }
    }
    sqlx::query("UPDATE proposals SET active_review_snapshot_id=NULL WHERE id=$1")
        .bind(f.proposal)
        .execute(&pool)
        .await
        .unwrap();
    assert!(sqlx::query("UPDATE proposals SET active_review_snapshot_id=$2,version=version+1,state='IN_REVIEW' WHERE id=$1").bind(f.proposal).bind(published.snapshot_id).execute(&pool).await.is_err());
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM review_snapshots")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn text_only_reviews_seal_an_explicit_empty_media_set(pool: PgPool) {
    let (f, _, preparer) = setup_media(&pool, false).await;
    let (lease, verified) = prepared(&pool, &f, &preparer, 2).await;
    assert!(verified.media().is_empty());
    let store = PgReviewSnapshots::new(pool.clone());
    let published = store.publish(&lease, &verified, &preparer).await.unwrap();
    let snapshot = store
        .get(f.owner, f.proposal, published.snapshot_id)
        .await
        .unwrap();
    assert_eq!((snapshot.file_count, snapshot.media_count), (1, 0));
    assert_eq!(snapshot.metadata["summary"]["media_checks_required"], 0);
    let receipts: i64 =
        sqlx::query_scalar("SELECT count(*) FROM review_snapshot_media WHERE snapshot_id=$1")
            .bind(published.snapshot_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(receipts, 0);
}

#[path = "snapshots/media.rs"]
mod media;

#[path = "snapshots/before_media.rs"]
mod before_media;
