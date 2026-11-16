use async_trait::async_trait;
use specspace_application::{
    comparison_reader::{
        BoundProviderFactory, ComparisonReadError, ComparisonReader, RepositoryBinding, Selection,
    },
    proposal::IdempotencyKey,
};
use specspace_domain::{BranchName, RepositoryPath};
use specspace_git_adapter::{
    CommitMetadata, CommitReceipt, CommitRequest, CommitSha, FileEntry, FileMode, GitError,
    GitProvider, blob,
};
use specspace_infrastructure::{
    comparison_authorization::PgComparisonAuthorization,
    comparison_worker::{ComparisonWork, ComparisonWorker},
    comparisons::{ComparisonStoreError, ComparisonTicket, PgComparisons},
};
use sqlx::PgPool;
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use uuid::Uuid;
const OLD: &str = "1111111111111111111111111111111111111111";
const HEAD: &str = "2222222222222222222222222222222222222222";
const MAIN: &str = "3333333333333333333333333333333333333333";
fn sha(value: &str) -> CommitSha {
    CommitSha::parse(value).unwrap()
}
struct Provider {
    files: BTreeMap<String, String>,
    reads: AtomicUsize,
    backoff: AtomicUsize,
    hook: Mutex<Option<(PgPool, Uuid)>>,
}
impl Provider {
    fn text(&self, commit: &CommitSha, path: &str) -> String {
        let text = self.files[path].clone();
        if commit.as_str() == HEAD && path.ends_with(".md") {
            text.replace("before", "edited")
        } else {
            text
        }
    }
}
#[async_trait]
impl GitProvider for Provider {
    async fn main_head(&self) -> Result<CommitSha, GitError> {
        if self
            .backoff
            .compare_exchange(1, 0, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            return Err(GitError::RateLimited {
                retry_after_seconds: Some(13),
            });
        }
        Ok(sha(MAIN))
    }
    async fn branch_head(&self, _: &BranchName) -> Result<CommitSha, GitError> {
        Ok(sha(HEAD))
    }
    async fn create_branch(&self, _: &BranchName, _: &CommitSha) -> Result<CommitSha, GitError> {
        panic!("comparison cannot mutate Git")
    }
    async fn commit(&self, _: CommitRequest<'_>) -> Result<CommitSha, GitError> {
        panic!("comparison cannot mutate Git")
    }
    async fn read_text(
        &self,
        commit: &CommitSha,
        path: &RepositoryPath,
    ) -> Result<String, GitError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        let hook = self.hook.lock().unwrap().take();
        if let Some((pool, actor)) = hook {
            sqlx::query("UPDATE users SET status='DISABLED' WHERE id=$1")
                .bind(actor)
                .execute(&pool)
                .await
                .unwrap();
        }
        Ok(self.text(commit, path.as_str()))
    }
    async fn read_bytes(
        &self,
        _: &CommitSha,
        _: &RepositoryPath,
        _: usize,
    ) -> Result<Vec<u8>, GitError> {
        panic!("inventory cannot fetch opaque media")
    }
    async fn list_files(&self, commit: &CommitSha) -> Result<Vec<FileEntry>, GitError> {
        Ok(self
            .files
            .keys()
            .map(|path| {
                let text = self.text(commit, path);
                FileEntry {
                    path: RepositoryPath::parse(path).unwrap(),
                    blob_sha: blob::oid(text.as_bytes(), 40).unwrap(),
                    size: text.len() as u64,
                    mode: FileMode::Regular,
                }
            })
            .collect())
    }
    async fn commit_metadata(&self, _: &CommitSha) -> Result<CommitMetadata, GitError> {
        panic!("unused metadata")
    }
    async fn commit_receipt(&self, _: &CommitSha) -> Result<CommitReceipt, GitError> {
        panic!("no mutation evidence required")
    }
    async fn merge_base(&self, _: &CommitSha, _: &CommitSha) -> Result<CommitSha, GitError> {
        Ok(sha(OLD))
    }
    async fn is_ancestor(&self, a: &CommitSha, b: &CommitSha) -> Result<bool, GitError> {
        Ok(a == b || a.as_str() == OLD && [HEAD, MAIN].contains(&b.as_str()))
    }
}
struct Factory {
    provider: Arc<Provider>,
    repository: Uuid,
}
#[async_trait]
impl BoundProviderFactory for Factory {
    async fn provider_bound(
        &self,
        binding: &RepositoryBinding,
    ) -> Result<Arc<dyn GitProvider>, ComparisonReadError> {
        assert_eq!(binding.repository, self.repository);
        assert_eq!(binding.remote_id, 42);
        assert_eq!(binding.installation_id, 7);
        Ok(self.provider.clone())
    }
}
struct Fixture {
    actor: Uuid,
    owner: Uuid,
    organization: Uuid,
    proposal: Uuid,
    store: Arc<PgComparisons>,
    reader: Arc<ComparisonReader>,
    provider: Arc<Provider>,
    worker: ComparisonWorker,
}
async fn actor(pool: &PgPool, organization: Uuid, role: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO users(id,issuer,external_subject,display_name) VALUES($1,'fixture',$1::text,'Synthetic comparison user')").bind(id).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO memberships(organization_id,user_id,role) VALUES($1,$2,$3)")
        .bind(organization)
        .bind(id)
        .bind(role)
        .execute(pool)
        .await
        .unwrap();
    id
}
async fn fixture(pool: &PgPool) -> Fixture {
    fixture_count(pool, 3).await
}
async fn fixture_count(pool: &PgPool, documents: usize) -> Fixture {
    let organization = Uuid::new_v4();
    let repository = Uuid::new_v4();
    let proposal = Uuid::new_v4();
    let team = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO organizations(id,name) VALUES($1,'Synthetic comparison organization')",
    )
    .bind(organization)
    .execute(pool)
    .await
    .unwrap();
    let actor = actor(pool, organization, "READER").await;
    let owner = super_actor(pool, organization).await;
    sqlx::query("INSERT INTO teams(id,organization_id,name) VALUES($1,$2,'Synthetic team')")
        .bind(team)
        .bind(organization)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO repositories(id,organization_id,provider,remote_id,installation_id) VALUES($1,$2,'GITHUB','42','7')").bind(repository).bind(organization).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO proposals(id,organization_id,repository_id,title,description,branch,owner_id,author_id,team_id,state,ui_impact,base_sha,head_sha,target_sha) VALUES($1,$2,$3,'Synthetic comparison','Synthetic comparison description','change/compare',$4,$4,$5,'DRAFT','UNDECIDED',$6,$7,$6)")
        .bind(proposal).bind(organization).bind(repository).bind(owner).bind(team).bind(OLD).bind(HEAD).execute(pool).await.unwrap();
    let mut files = BTreeMap::new();
    let entries:Vec<_>=(0..documents).map(|n| {
        let path=format!("docs/{n}.md");let id=format!("DOC-{n}");files.insert(path.clone(),format!("---\ndocument_id: {id}\ntitle: Synthetic\nowner_team: fixture\nschema_version: 1\n---\nPRIVATE SOURCE SENTINEL before\n"));serde_json::json!({"document_id":id,"path":path})
    }).collect();
    files.insert(
        ".specspace/catalog.json".into(),
        serde_json::json!({"schema_version":1,"documents":entries}).to_string(),
    );
    let provider = Arc::new(Provider {
        files,
        reads: AtomicUsize::new(0),
        backoff: AtomicUsize::new(0),
        hook: Mutex::new(None),
    });
    let store = Arc::new(PgComparisons::new(pool.clone()));
    let reader = Arc::new(
        ComparisonReader::new(
            Arc::new(PgComparisonAuthorization::new(pool.clone())),
            Arc::new(Factory {
                provider: provider.clone(),
                repository,
            }),
            Duration::from_secs(900),
        )
        .unwrap(),
    );
    let worker =
        ComparisonWorker::new(store.clone(), reader.clone(), Duration::from_secs(900)).unwrap();
    Fixture {
        actor,
        owner,
        organization,
        proposal,
        store,
        reader,
        provider,
        worker,
    }
}
async fn super_actor(pool: &PgPool, organization: Uuid) -> Uuid {
    actor(pool, organization, "ADMIN").await
}
async fn reserve(f: &Fixture, key: &str) -> ComparisonTicket {
    f.store
        .reserve(
            f.actor,
            f.proposal,
            &IdempotencyKey::parse(key).unwrap(),
            Selection::default(),
        )
        .await
        .unwrap()
}
#[sqlx::test(migrations = "../../migrations")]
async fn ready_publication_survives_restart_and_pages_without_git_or_private_source_payloads(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    let ticket = reserve(&f, "ready").await;
    assert_eq!(
        f.store
            .status(f.actor, f.proposal, ticket.snapshot_id)
            .await
            .unwrap()
            .phase,
        "QUEUED"
    );
    assert!(matches!(
        f.store
            .page(f.actor, f.proposal, ticket.snapshot_id, 0, 1)
            .await,
        Err(ComparisonStoreError::NotReady)
    ));
    assert_eq!(
        f.worker.run_once().await.unwrap(),
        ComparisonWork::Completed(ticket.job_id)
    );
    let restarted = PgComparisons::new(pool.clone());
    let reads = f.provider.reads.load(Ordering::SeqCst);
    let status = restarted
        .status(f.actor, f.proposal, ticket.snapshot_id)
        .await
        .unwrap();
    assert_eq!(status.state, "READY");
    assert_eq!(status.summary.as_ref().unwrap().merge_base_sha, OLD);
    let first = restarted
        .page(f.actor, f.proposal, ticket.snapshot_id, 0, 1)
        .await
        .unwrap();
    assert_eq!(first.total, 3);
    assert_eq!(first.next_offset, Some(1));
    let rest = restarted
        .page(f.actor, f.proposal, ticket.snapshot_id, 1, 100)
        .await
        .unwrap();
    assert_eq!(rest.files.len(), 2);
    assert!(rest.next_offset.is_none());
    assert_eq!(first.content_id, rest.content_id);
    assert_ne!(first.files[0].id, rest.files[0].id);
    assert_eq!(f.provider.reads.load(Ordering::SeqCst), reads);
    let values:Vec<serde_json::Value>=sqlx::query_scalar("SELECT metadata FROM comparison_snapshots WHERE state='READY' UNION ALL SELECT change FROM comparison_files UNION ALL SELECT payload FROM jobs UNION ALL SELECT result FROM jobs WHERE result IS NOT NULL").fetch_all(&pool).await.unwrap();
    assert!(
        values
            .iter()
            .all(|v| !v.to_string().contains("PRIVATE SOURCE SENTINEL"))
    );
    let operations: i64 = sqlx::query_scalar("SELECT count(*) FROM git_operations")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(operations, 0);
    sqlx::query(
        "UPDATE proposals SET head_sha=repeat('4',40),version=2,state='CLOSED' WHERE id=$1",
    )
    .bind(f.proposal)
    .execute(&pool)
    .await
    .unwrap();
    let historical = restarted
        .status(f.actor, f.proposal, ticket.snapshot_id)
        .await
        .unwrap();
    assert_eq!(historical.current_version, 2);
    assert_eq!(historical.summary.unwrap().proposal_version, 1);
    restarted
        .page(f.actor, f.proposal, ticket.snapshot_id, 0, 100)
        .await
        .unwrap();
}
#[sqlx::test(migrations = "../../migrations")]
async fn concurrent_idempotent_reads_share_one_job_and_actor_quotas_serialize_other_keys(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    let key = IdempotencyKey::parse("same").unwrap();
    let (a, b) = tokio::join!(
        f.store
            .reserve(f.actor, f.proposal, &key, Selection::default()),
        f.store
            .reserve(f.actor, f.proposal, &key, Selection::default())
    );
    assert_eq!(a.unwrap(), b.unwrap());
    assert!(matches!(
        f.store
            .reserve(
                f.actor,
                f.proposal,
                &key,
                Selection {
                    head: Some(sha(OLD)),
                    target: None
                }
            )
            .await,
        Err(ComparisonStoreError::IdempotencyMismatch)
    ));
    let keys = [
        IdempotencyKey::parse("other-a").unwrap(),
        IdempotencyKey::parse("other-b").unwrap(),
    ];
    let (a, b) = tokio::join!(
        f.store
            .reserve(f.actor, f.proposal, &keys[0], Selection::default()),
        f.store
            .reserve(f.actor, f.proposal, &keys[1], Selection::default())
    );
    assert_eq!(a.is_ok() as usize + b.is_ok() as usize, 1);
    assert!(matches!(
        a.err().or(b.err()),
        Some(ComparisonStoreError::QuotaExceeded)
    ));
    reserve(&f, "same").await;
}
#[sqlx::test(migrations = "../../migrations")]
async fn stale_owners_cannot_extend_fail_or_publish_after_read_only_reclaim(pool: PgPool) {
    let f = fixture(&pool).await;
    let ticket = reserve(&f, "fenced").await;
    let first = f.store.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    let loaded = f
        .reader
        .load(f.actor, f.proposal, first.selection())
        .await
        .unwrap();
    sqlx::query("UPDATE jobs SET leased_until=clock_timestamp()-interval '1 second' WHERE id=$1")
        .bind(ticket.job_id)
        .execute(&pool)
        .await
        .unwrap();
    let second = f.store.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    assert_eq!(second.attempts(), 2);
    assert!(matches!(
        f.store.heartbeat(&first, 60).await,
        Err(ComparisonStoreError::LeaseLost)
    ));
    assert!(matches!(
        f.store.fail(&first, "OLD_OWNER", None).await,
        Err(ComparisonStoreError::LeaseLost)
    ));
    assert!(matches!(
        f.store.publish(&first, &loaded).await,
        Err(ComparisonStoreError::LeaseLost)
    ));
    f.store.publish(&second, &loaded).await.unwrap();
    assert_eq!(
        f.store
            .status(f.actor, f.proposal, ticket.snapshot_id)
            .await
            .unwrap()
            .phase,
        "READY"
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn provider_backoff_is_observed_and_no_partial_metadata_is_exposed(pool: PgPool) {
    let f = fixture(&pool).await;
    let ticket = reserve(&f, "backoff").await;
    f.provider.backoff.store(1, Ordering::SeqCst);
    assert_eq!(
        f.worker.run_once().await.unwrap(),
        ComparisonWork::Deferred(ticket.job_id)
    );
    let state = f
        .store
        .status(f.actor, f.proposal, ticket.snapshot_id)
        .await
        .unwrap();
    assert_eq!(state.phase, "RETRY_WAIT");
    assert!(state.summary.is_none());
    let delay:f64=sqlx::query_scalar("SELECT extract(epoch FROM available_at-clock_timestamp())::double precision FROM jobs WHERE id=$1").bind(ticket.job_id).fetch_one(&pool).await.unwrap();
    assert!(delay > 11. && delay <= 13.);
    assert_eq!(f.worker.run_once().await.unwrap(), ComparisonWork::Idle);
    sqlx::query("UPDATE jobs SET available_at=clock_timestamp()-interval '1 second' WHERE id=$1")
        .bind(ticket.job_id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        f.worker.run_once().await.unwrap(),
        ComparisonWork::Completed(ticket.job_id)
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn a_failed_mid_publication_transaction_rolls_back_all_rows_and_can_be_read_again(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    let ticket = reserve(&f, "rollback").await;
    sqlx::query("CREATE FUNCTION reject_comparison_row() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.ordinal=1 THEN RAISE EXCEPTION 'synthetic publication failure'; END IF; RETURN NEW; END; $$").execute(&pool).await.unwrap();
    sqlx::query("CREATE TRIGGER reject_comparison BEFORE INSERT ON comparison_files FOR EACH ROW EXECUTE FUNCTION reject_comparison_row()").execute(&pool).await.unwrap();
    assert!(matches!(
        f.worker.run_once().await,
        Err(ComparisonStoreError::Unavailable)
    ));
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM comparison_files")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 0);
    assert!(
        f.store
            .status(f.actor, f.proposal, ticket.snapshot_id)
            .await
            .unwrap()
            .summary
            .is_none()
    );
    sqlx::query("DROP TRIGGER reject_comparison ON comparison_files")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE jobs SET leased_until=clock_timestamp()-interval '1 second' WHERE id=$1")
        .bind(ticket.job_id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        f.worker.run_once().await.unwrap(),
        ComparisonWork::Completed(ticket.job_id)
    );
    let ready: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE action='COMPARISON_READY'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(ready, 1);
}
#[sqlx::test(migrations = "../../migrations")]
async fn malformed_jobs_and_exhaustion_terminate_without_reading_or_publishing(pool: PgPool) {
    let f = fixture(&pool).await;
    let ticket = reserve(&f, "malformed").await;
    sqlx::query(
        "UPDATE jobs SET payload=jsonb_build_object('snapshot_id',gen_random_uuid()) WHERE id=$1",
    )
    .bind(ticket.job_id)
    .execute(&pool)
    .await
    .unwrap();
    assert!(matches!(
        f.worker.run_once().await,
        Err(ComparisonStoreError::InvalidSnapshot)
    ));
    assert_eq!(
        f.store
            .status(f.actor, f.proposal, ticket.snapshot_id)
            .await
            .unwrap()
            .state,
        "FAILED"
    );
    assert_eq!(f.provider.reads.load(Ordering::SeqCst), 0);
    let exhausted = reserve(&f, "exhausted").await;
    sqlx::query("UPDATE jobs SET attempts=max_attempts WHERE id=$1")
        .bind(exhausted.job_id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(f.worker.run_once().await.unwrap(), ComparisonWork::Idle);
    assert_eq!(
        f.store
            .status(f.actor, f.proposal, exhausted.snapshot_id)
            .await
            .unwrap()
            .state,
        "FAILED"
    );
}
#[sqlx::test(migrations = "../../migrations")]
async fn current_access_and_frozen_repository_scope_are_rechecked_for_every_page(pool: PgPool) {
    let f = fixture(&pool).await;
    let ticket = reserve(&f, "scope").await;
    f.worker.run_once().await.unwrap();
    assert!(matches!(
        f.store
            .status(f.owner, f.proposal, ticket.snapshot_id)
            .await,
        Err(ComparisonStoreError::NotFound)
    ));
    sqlx::query("UPDATE repositories SET installation_id='8' WHERE id=(SELECT repository_id FROM proposals WHERE id=$1)").bind(f.proposal).execute(&pool).await.unwrap();
    assert!(matches!(
        f.store
            .page(f.actor, f.proposal, ticket.snapshot_id, 0, 100)
            .await,
        Err(ComparisonStoreError::Read(
            ComparisonReadError::ContextChanged
        ))
    ));
    sqlx::query("UPDATE repositories SET installation_id='7' WHERE id=(SELECT repository_id FROM proposals WHERE id=$1)").bind(f.proposal).execute(&pool).await.unwrap();
    sqlx::query("DELETE FROM memberships WHERE organization_id=$1 AND user_id=$2")
        .bind(f.organization)
        .bind(f.actor)
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        f.store
            .page(f.actor, f.proposal, ticket.snapshot_id, 0, 100)
            .await,
        Err(ComparisonStoreError::Read(ComparisonReadError::NotFound))
    ));
}
#[sqlx::test(migrations = "../../migrations")]
async fn queued_metadata_changes_and_mid_read_revocation_do_not_publish_ready_content(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    let ticket = reserve(&f, "changed").await;
    sqlx::query("UPDATE proposals SET version=version+1 WHERE id=$1")
        .bind(f.proposal)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        f.worker.run_once().await.unwrap(),
        ComparisonWork::Failed(ticket.job_id)
    );
    assert_eq!(f.provider.reads.load(Ordering::SeqCst), 0);
    let ticket = reserve(&f, "revoked").await;
    *f.provider.hook.lock().unwrap() = Some((pool.clone(), f.actor));
    assert_eq!(
        f.worker.run_once().await.unwrap(),
        ComparisonWork::Failed(ticket.job_id)
    );
    let ready: i64 =
        sqlx::query_scalar("SELECT count(*) FROM comparison_snapshots WHERE state='READY'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(ready, 0);
}
#[sqlx::test(migrations = "../../migrations")]
async fn database_guards_frozen_intents_and_all_terminal_metadata_and_file_rows(pool: PgPool) {
    let f = fixture(&pool).await;
    let ticket = reserve(&f, "immutable").await;
    assert!(
        sqlx::query("UPDATE comparison_snapshots SET selected_head=repeat('a',40) WHERE id=$1")
            .bind(ticket.snapshot_id)
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM comparison_snapshots WHERE id=$1")
            .bind(ticket.snapshot_id)
            .execute(&pool)
            .await
            .is_err()
    );
    f.worker.run_once().await.unwrap();
    assert!(
        sqlx::query("UPDATE comparison_snapshots SET metadata='{}' WHERE id=$1")
            .bind(ticket.snapshot_id)
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM comparison_files WHERE snapshot_id=$1")
            .bind(ticket.snapshot_id)
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("UPDATE comparison_files SET ordinal=9 WHERE snapshot_id=$1")
            .bind(ticket.snapshot_id)
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(matches!(
        f.store
            .page(f.actor, f.proposal, ticket.snapshot_id, 0, 101)
            .await,
        Err(ComparisonStoreError::InvalidQuery)
    ));
}

#[sqlx::test(migrations = "../../migrations")]
async fn multi_batch_publication_and_empty_comparisons_have_complete_contiguous_pages(
    pool: PgPool,
) {
    let f = fixture_count(&pool, 620).await;
    let ticket = reserve(&f, "many").await;
    assert_eq!(
        f.worker.run_once().await.unwrap(),
        ComparisonWork::Completed(ticket.job_id)
    );
    let mut offset = 0;
    let mut ids = std::collections::HashSet::new();
    loop {
        let page = f
            .store
            .page(f.actor, f.proposal, ticket.snapshot_id, offset, 100)
            .await
            .unwrap();
        assert_eq!(page.total, 620);
        for file in page.files {
            assert!(ids.insert(file.id));
        }
        match page.next_offset {
            Some(next) => {
                assert!(next > offset);
                offset = next
            }
            None => break,
        }
    }
    assert_eq!(ids.len(), 620);
    let ticket = f
        .store
        .reserve(
            f.actor,
            f.proposal,
            &IdempotencyKey::parse("empty").unwrap(),
            Selection {
                head: Some(sha(OLD)),
                target: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        f.worker.run_once().await.unwrap(),
        ComparisonWork::Completed(ticket.job_id)
    );
    let page = f
        .store
        .page(f.actor, f.proposal, ticket.snapshot_id, 0, 100)
        .await
        .unwrap();
    assert_eq!(page.total, 0);
    assert!(page.files.is_empty());
    assert!(page.next_offset.is_none());
}
#[sqlx::test(migrations = "../../migrations")]
async fn incomplete_or_malformed_database_publication_is_rejected_and_ready_rows_cannot_grow(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    let ticket = reserve(&f, "incomplete").await;
    let lease = f.store.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    let loaded = f
        .reader
        .load(f.actor, f.proposal, lease.selection())
        .await
        .unwrap();
    let metadata = serde_json::to_value(loaded.summary()).unwrap();
    assert!(sqlx::query("UPDATE comparison_snapshots SET state='READY',completed_at=clock_timestamp(),metadata=$2,change_count=3 WHERE id=$1").bind(ticket.snapshot_id).bind(&metadata).execute(&pool).await.is_err());
    assert!(sqlx::query("INSERT INTO comparison_files(organization_id,snapshot_id,ordinal,file_id,change) VALUES($1,$2,0,$3,'{}')").bind(f.organization).bind(ticket.snapshot_id).bind("d".repeat(64)).execute(&pool).await.is_err());
    let change = &loaded.inventory().changes[0];
    sqlx::query("INSERT INTO comparison_files(organization_id,snapshot_id,ordinal,file_id,change) VALUES($1,$2,1,$3,$4)").bind(f.organization).bind(ticket.snapshot_id).bind(&change.id).bind(serde_json::to_value(change).unwrap()).execute(&pool).await.unwrap();
    let mut incomplete = metadata;
    incomplete["changes_total"] = serde_json::json!(1);
    assert!(sqlx::query("UPDATE comparison_snapshots SET state='READY',completed_at=clock_timestamp(),metadata=$2,change_count=1 WHERE id=$1").bind(ticket.snapshot_id).bind(incomplete).execute(&pool).await.is_err());
    f.store.publish(&lease, &loaded).await.unwrap();
    let mut extra = serde_json::to_value(change).unwrap();
    extra["id"] = serde_json::json!("d".repeat(64));
    assert!(sqlx::query("INSERT INTO comparison_files(organization_id,snapshot_id,ordinal,file_id,change) VALUES($1,$2,3,$3,$4)").bind(f.organization).bind(ticket.snapshot_id).bind("d".repeat(64)).bind(extra).execute(&pool).await.is_err());
}
#[sqlx::test(migrations = "../../migrations")]
async fn new_request_rate_limits_do_not_break_exact_replay_after_metadata_advances(pool: PgPool) {
    let f = fixture(&pool).await;
    let mut first = None;
    for n in 0..10 {
        let ticket = reserve(&f, &format!("rate-{n}")).await;
        if n == 0 {
            first = Some(ticket.clone());
        }
        let lease = f.store.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
        f.store
            .fail(&lease, "SYNTHETIC_FAILURE", None)
            .await
            .unwrap();
    }
    assert!(matches!(
        f.store
            .reserve(
                f.actor,
                f.proposal,
                &IdempotencyKey::parse("rate-11").unwrap(),
                Selection::default()
            )
            .await,
        Err(ComparisonStoreError::QuotaExceeded)
    ));
    sqlx::query("UPDATE proposals SET head_sha=repeat('4',40),version=2 WHERE id=$1")
        .bind(f.proposal)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(reserve(&f, "rate-0").await, first.unwrap());
    assert_eq!(f.provider.reads.load(Ordering::SeqCst), 0);
}
