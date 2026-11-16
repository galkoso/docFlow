use async_trait::async_trait;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use specspace_application::{
    book::BookError,
    proposal::IdempotencyKey,
    reader::BookProviderFactory,
    save::{DocumentSave, PreparedSave, SaveRequest, SaveSnapshot, SourceFile, ValidatedSave},
};
use specspace_domain::{BranchName, RepositoryPath};
use specspace_git_adapter::{
    CommitMetadata, CommitReceipt, CommitRequest, CommitSha, FileChange, FileEntry, FileMode,
    GitError, GitProvider,
};
use specspace_infrastructure::{
    save_worker::{SaveWork, SaveWorker},
    saves::{PgSaves, SaveJournalError},
};
use sqlx::PgPool;
use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::Notify;
use uuid::Uuid;

const OLD: &str = "1111111111111111111111111111111111111111";
const NEW: &str = "2222222222222222222222222222222222222222";
const LATER: &str = "3333333333333333333333333333333333333333";
const BRANCH: &str = "change/save";
type Files = BTreeMap<String, (Vec<u8>, FileMode)>;
struct Fixture {
    pool: PgPool,
    organization: Uuid,
    repository: Uuid,
    actor: Uuid,
    proposal: Uuid,
    source: Arc<Provider>,
    journal: Arc<PgSaves>,
}
fn document(id: &str, section: &str, text: &str) -> String {
    format!(
        "---\ndocument_id: {id}\ntitle: Test\nowner_team: test\nschema_version: 1\n---\n\n<!-- ss:section:{section} -->\n## Heading\n\n{text}\n"
    )
}
fn initial() -> Files {
    BTreeMap::from([
    (".specspace/catalog.json".into(),(serde_json::to_vec(&json!({"schema_version":1,"documents":[{"document_id":"DOC-A","path":"docs/a.md"},{"document_id":"DOC-B","path":"docs/b.md"}]})).unwrap(),FileMode::Regular)),
    ("docs/a.md".into(),(document("DOC-A","SEC-A","first").into_bytes(),FileMode::Regular)),
    ("docs/b.md".into(),(document("DOC-B","SEC-B","second").into_bytes(),FileMode::Regular)),
    ("README.md".into(),(b"untouched".to_vec(),FileMode::Regular)),
])
}
fn blob(bytes: &[u8]) -> CommitSha {
    let mut digest = sha1::Sha1::new();
    digest.update(format!("blob {}\0", bytes.len()).as_bytes());
    digest.update(bytes);
    CommitSha::parse(format!("{:x}", digest.finalize())).unwrap()
}
struct Record {
    parent: String,
    actor: Uuid,
    operation: Uuid,
}
struct Provider {
    pool: PgPool,
    actor: Uuid,
    proposal: Uuid,
    head: Mutex<String>,
    snapshots: Mutex<HashMap<String, Files>>,
    receipts: Mutex<HashMap<String, Record>>,
    mode: AtomicU8,
    writes: AtomicUsize,
    reads: AtomicUsize,
    read_hook: AtomicU8,
    applied: Notify,
    release: Notify,
}
#[async_trait]
impl GitProvider for Provider {
    async fn main_head(&self) -> Result<CommitSha, GitError> {
        panic!("a proposal save must never write or retarget main")
    }
    async fn branch_head(&self, branch: &BranchName) -> Result<CommitSha, GitError> {
        assert_eq!(branch.as_str(), BRANCH);
        self.reads.fetch_add(1, Ordering::SeqCst);
        if self
            .mode
            .compare_exchange(8, 0, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            return Err(GitError::RateLimited {
                retry_after_seconds: Some(17),
            });
        }
        CommitSha::parse(self.head.lock().unwrap().clone())
    }
    async fn create_branch(&self, _: &BranchName, _: &CommitSha) -> Result<CommitSha, GitError> {
        panic!("save must use the reserved proposal ref")
    }
    async fn commit(&self, request: CommitRequest<'_>) -> Result<CommitSha, GitError> {
        assert_eq!(request.branch.as_str(), BRANCH);
        self.writes.fetch_add(1, Ordering::SeqCst);
        let mode = self.mode.load(Ordering::SeqCst);
        if mode == 2 {
            return Err(GitError::ResultUnknown);
        }
        {
            let mut head = self.head.lock().unwrap();
            if *head != request.expected_head.as_str() {
                return Err(GitError::HeadMismatch);
            }
            let mut snapshots = self.snapshots.lock().unwrap();
            let mut files = snapshots[head.as_str()].clone();
            for change in request.changes {
                match change {
                    FileChange::Put { path, content } => {
                        files.insert(path.as_str().into(), (content.clone(), FileMode::Regular));
                    }
                    _ => panic!("unexpected delete"),
                }
            }
            if mode == 4 {
                files.get_mut("README.md").unwrap().0 = b"foreign edit".to_vec();
            }
            if mode == 5 {
                files
                    .get_mut("docs/a.md")
                    .unwrap()
                    .0
                    .extend_from_slice(b"not planned");
            }
            if mode == 6 {
                files.get_mut("README.md").unwrap().1 = FileMode::Executable;
            }
            if mode == 13 {
                files.insert(
                    "foreign.md".into(),
                    (b"unplanned".to_vec(), FileMode::Regular),
                );
            }
            if mode == 14 {
                files.remove("README.md");
            }
            snapshots.insert(NEW.into(), files.clone());
            self.receipts.lock().unwrap().insert(
                NEW.into(),
                Record {
                    parent: head.clone(),
                    actor: if mode == 16 {
                        Uuid::new_v4()
                    } else {
                        request.actor_id
                    },
                    operation: if mode == 17 {
                        Uuid::new_v4()
                    } else {
                        request.operation_id
                    },
                },
            );
            *head = NEW.into();
            if mode == 9 {
                files.get_mut("README.md").unwrap().0 = b"later external update".to_vec();
                snapshots.insert(LATER.into(), files);
                self.receipts.lock().unwrap().insert(
                    LATER.into(),
                    Record {
                        parent: NEW.into(),
                        actor: Uuid::new_v4(),
                        operation: Uuid::new_v4(),
                    },
                );
                *head = LATER.into();
            }
            if mode == 15 {
                let original = snapshots[OLD].clone();
                snapshots.insert(LATER.into(), original);
                self.receipts.lock().unwrap().insert(
                    LATER.into(),
                    Record {
                        parent: OLD.into(),
                        actor: Uuid::new_v4(),
                        operation: Uuid::new_v4(),
                    },
                );
                *head = LATER.into();
            }
        }
        if mode == 7 {
            sqlx::query("UPDATE users SET status='DISABLED' WHERE id=$1")
                .bind(self.actor)
                .execute(&self.pool)
                .await
                .unwrap();
        }
        if mode == 10 {
            sqlx::query("UPDATE proposals SET state='CLOSED',version=version+1 WHERE id=$1")
                .bind(self.proposal)
                .execute(&self.pool)
                .await
                .unwrap();
        }
        self.applied.notify_one();
        if mode == 3 {
            self.release.notified().await;
        }
        if [1, 9].contains(&mode) {
            Err(GitError::ResultUnknown)
        } else {
            CommitSha::parse(NEW)
        }
    }
    async fn read_text(
        &self,
        commit: &CommitSha,
        path: &RepositoryPath,
    ) -> Result<String, GitError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        match self.read_hook.swap(0, Ordering::SeqCst) {
            1 => {
                sqlx::query("UPDATE users SET status='DISABLED' WHERE id=$1")
                    .bind(self.actor)
                    .execute(&self.pool)
                    .await
                    .unwrap();
            }
            2 => {
                sqlx::query("UPDATE repositories SET remote_id='99' WHERE id=(SELECT repository_id FROM proposals WHERE id=$1)").bind(self.proposal).execute(&self.pool).await.unwrap();
            }
            _ => {}
        }
        let files = self.snapshots.lock().unwrap();
        let bytes = &files
            .get(commit.as_str())
            .ok_or(GitError::FileMissing)?
            .get(path.as_str())
            .ok_or(GitError::FileMissing)?
            .0;
        String::from_utf8(bytes.clone()).map_err(|_| GitError::InvalidResponse)
    }
    async fn read_bytes(
        &self,
        commit: &CommitSha,
        path: &RepositoryPath,
        max: usize,
    ) -> Result<Vec<u8>, GitError> {
        let files = self.snapshots.lock().unwrap();
        let bytes = &files
            .get(commit.as_str())
            .ok_or(GitError::FileMissing)?
            .get(path.as_str())
            .ok_or(GitError::FileMissing)?
            .0;
        if bytes.len() > max {
            return Err(GitError::ResponseTooLarge);
        }
        Ok(bytes.clone())
    }
    async fn list_files(&self, commit: &CommitSha) -> Result<Vec<FileEntry>, GitError> {
        let files = self.snapshots.lock().unwrap();
        Ok(files
            .get(commit.as_str())
            .ok_or(GitError::FileMissing)?
            .iter()
            .map(|(path, (bytes, mode))| FileEntry {
                path: RepositoryPath::parse(path).unwrap(),
                blob_sha: if self.mode.load(Ordering::SeqCst) == 12
                    && commit.as_str() == NEW
                    && path == "docs/a.md"
                {
                    CommitSha::parse("a".repeat(40)).unwrap()
                } else {
                    blob(bytes)
                },
                size: bytes.len() as u64,
                mode: *mode,
            })
            .collect())
    }
    async fn commit_metadata(&self, commit: &CommitSha) -> Result<CommitMetadata, GitError> {
        Ok(CommitMetadata {
            sha: commit.clone(),
            committed_at: "2026-11-16T00:00:00Z".parse().unwrap(),
        })
    }
    async fn commit_receipt(&self, commit: &CommitSha) -> Result<CommitReceipt, GitError> {
        if self
            .mode
            .compare_exchange(11, 0, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            return Err(GitError::RateLimited {
                retry_after_seconds: Some(23),
            });
        }
        let receipts = self.receipts.lock().unwrap();
        let record = receipts.get(commit.as_str()).ok_or(GitError::FileMissing)?;
        Ok(CommitReceipt {
            sha: if self.mode.load(Ordering::SeqCst) == 18 {
                CommitSha::parse(LATER).unwrap()
            } else {
                commit.clone()
            },
            tree: CommitSha::parse("a".repeat(40)).unwrap(),
            parents: vec![CommitSha::parse(&record.parent).unwrap()],
            committed_at: "2026-11-16T00:00:00Z".parse().unwrap(),
            message: format!(
                "Update proposal\n\nSpecSpace-Operation: {}\nSpecSpace-Actor: {}",
                record.operation, record.actor
            ),
        })
    }
    async fn is_ancestor(
        &self,
        ancestor: &CommitSha,
        descendant: &CommitSha,
    ) -> Result<bool, GitError> {
        let receipts = self.receipts.lock().unwrap();
        let mut current = descendant.as_str();
        for _ in 0..20 {
            if current == ancestor.as_str() {
                return Ok(true);
            }
            match receipts.get(current) {
                Some(record) => current = &record.parent,
                None => return Ok(false),
            }
        }
        Ok(false)
    }
}
struct Factory {
    repository: Uuid,
    provider: Arc<Provider>,
}
#[async_trait]
impl BookProviderFactory for Factory {
    async fn provider(&self, repository: Uuid) -> Result<Arc<dyn GitProvider>, BookError> {
        assert_eq!(repository, self.repository);
        Ok(self.provider.clone())
    }
}
impl Fixture {
    fn worker(&self) -> SaveWorker {
        SaveWorker::new(
            self.journal.clone(),
            Arc::new(Factory {
                repository: self.repository,
                provider: self.source.clone(),
            }),
            Duration::from_secs(60),
        )
        .unwrap()
    }
    fn body(&self) -> SaveRequest {
        let snapshots = self.source.snapshots.lock().unwrap();
        let files = &snapshots[OLD];
        SaveRequest {
            expected_head_sha: OLD.into(),
            expected_proposal_version: 1,
            message: "Update proposal".into(),
            documents: [("DOC-A", "docs/a.md"), ("DOC-B", "docs/b.md")]
                .into_iter()
                .map(|(id, path)| {
                    let raw = &files[path].0;
                    DocumentSave {
                        document_id: id.into(),
                        expected_document_sha256: format!("{:x}", Sha256::digest(raw)),
                        markdown: String::from_utf8(raw.clone())
                            .unwrap()
                            .replace("## Heading", "## Updated"),
                    }
                })
                .collect(),
        }
    }
    fn intent(&self, body: SaveRequest) -> ValidatedSave {
        ValidatedSave::new(self.actor, self.proposal, self.repository, body).unwrap()
    }
    async fn ticket(&self) -> specspace_infrastructure::saves::SaveTicket {
        self.journal
            .reserve(
                &self.intent(self.body()),
                &IdempotencyKey::parse("save-one").unwrap(),
            )
            .await
            .unwrap()
    }
    async fn ready(&self, job: Uuid) {
        sqlx::query("UPDATE jobs SET available_at=clock_timestamp() WHERE id=$1")
            .bind(job)
            .execute(&self.pool)
            .await
            .unwrap();
    }
    async fn state(&self) -> (String, String, i64) {
        sqlx::query_as("SELECT state,head_sha,version FROM proposals WHERE id=$1")
            .bind(self.proposal)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }
}
async fn fixture(pool: PgPool) -> Fixture {
    let organization = Uuid::new_v4();
    let repository = Uuid::new_v4();
    let actor = Uuid::new_v4();
    let team = Uuid::new_v4();
    let proposal = Uuid::new_v4();
    sqlx::query("INSERT INTO organizations(id,name) VALUES($1,'Synthetic save organization')")
        .bind(organization)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO users(id,issuer,external_subject,display_name) VALUES($1,'fixture',$1::text,'Synthetic save user')").bind(actor).execute(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO memberships(organization_id,user_id,role) VALUES($1,$2,'CONTRIBUTOR')",
    )
    .bind(organization)
    .bind(actor)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO repositories(id,organization_id,provider,remote_id,installation_id) VALUES($1,$2,'GITHUB','42','7')").bind(repository).bind(organization).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO teams(id,organization_id,name) VALUES($1,$2,'Save team')")
        .bind(team)
        .bind(organization)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO proposals(id,organization_id,repository_id,title,branch,owner_id,author_id,team_id,state,description,ui_impact,base_sha,head_sha,target_sha) VALUES($1,$2,$3,'Synthetic save proposal',$4,$5,$5,$6,'APPROVED','Synthetic proposal description','UNDECIDED',$7,$7,$7)")
        .bind(proposal).bind(organization).bind(repository).bind(BRANCH).bind(actor).bind(team).bind(OLD).execute(&pool).await.unwrap();
    let source = Arc::new(Provider {
        pool: pool.clone(),
        actor,
        proposal,
        head: Mutex::new(OLD.into()),
        snapshots: Mutex::new(HashMap::from([(OLD.into(), initial())])),
        receipts: Mutex::new(HashMap::new()),
        mode: AtomicU8::new(0),
        writes: AtomicUsize::new(0),
        reads: AtomicUsize::new(0),
        read_hook: AtomicU8::new(0),
        applied: Notify::new(),
        release: Notify::new(),
    });
    Fixture {
        journal: Arc::new(PgSaves::new(pool.clone())),
        pool,
        organization,
        repository,
        actor,
        proposal,
        source,
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn one_verified_atomic_save_updates_head_invalidates_approval_and_purges_private_input(
    pool: PgPool,
) {
    let f = fixture(pool).await;
    let ticket = f.ticket().await;
    assert_eq!(
        f.worker().run_once().await.unwrap(),
        SaveWork::Completed(ticket.job_id)
    );
    assert_eq!(f.source.writes.load(Ordering::SeqCst), 1);
    assert_eq!(f.state().await, ("DRAFT".into(), NEW.into(), 2));
    let result: Value = sqlx::query_scalar("SELECT result FROM jobs WHERE id=$1")
        .bind(ticket.job_id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(result["head_sha"], NEW);
    assert_eq!(result["no_op"], false);
    assert_eq!(result["documents"].as_array().unwrap().len(), 2);
    assert_eq!(result["committed_at"], "2026-11-16T00:00:00+00:00");
    let request: Option<Value> =
        sqlx::query_scalar("SELECT request FROM proposal_saves WHERE operation_id=$1")
            .bind(ticket.operation_id)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert!(request.is_none());
    let outbox: Value = sqlx::query_scalar(
        "SELECT payload FROM outbox WHERE event_type='PROPOSAL_CONTENT_CHANGED'",
    )
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(outbox["invalidate_review"], true);
    let audit: Vec<(String, Option<String>, Option<String>)> =
        sqlx::query_as("SELECT action,before_ref,after_ref FROM audit_events ORDER BY timestamp")
            .fetch_all(&f.pool)
            .await
            .unwrap();
    assert_eq!(
        audit.last().unwrap(),
        &("PROPOSAL_SAVED".into(), Some(OLD.into()), Some(NEW.into()))
    );
    let safe: Vec<Value> =
        sqlx::query_scalar("SELECT payload FROM jobs UNION ALL SELECT payload FROM outbox")
            .fetch_all(&f.pool)
            .await
            .unwrap();
    assert!(!format!("{safe:?}").contains("## Updated"));
    assert!(!result.to_string().contains("markdown"));
    // Same original intent returns its ticket despite the now different version.
    assert_eq!(f.ticket().await, ticket);
    assert_eq!(f.source.writes.load(Ordering::SeqCst), 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn no_op_does_not_write_git_advance_versions_or_invalidate_approval(pool: PgPool) {
    let f = fixture(pool).await;
    let mut body = f.body();
    for document in &mut body.documents {
        document.markdown = document.markdown.replace("## Updated", "## Heading");
    }
    let ticket = f
        .journal
        .reserve(&f.intent(body), &IdempotencyKey::parse("noop").unwrap())
        .await
        .unwrap();
    assert_eq!(
        f.worker().run_once().await.unwrap(),
        SaveWork::Completed(ticket.job_id)
    );
    assert_eq!(f.source.writes.load(Ordering::SeqCst), 0);
    assert_eq!(f.state().await, ("APPROVED".into(), OLD.into(), 1));
    let result: Value = sqlx::query_scalar("SELECT result FROM jobs WHERE id=$1")
        .bind(ticket.job_id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(result["no_op"], true);
    assert!(result["committed_at"].is_null());
    let outbox: i64 = sqlx::query_scalar("SELECT count(*) FROM outbox")
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(outbox, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn concurrent_idempotent_reservations_share_one_job_and_other_intents_cannot_overwrite_it(
    pool: PgPool,
) {
    let f = fixture(pool).await;
    let intent = f.intent(f.body());
    let key = IdempotencyKey::parse("same").unwrap();
    let (a, b) = tokio::join!(
        f.journal.reserve(&intent, &key),
        f.journal.reserve(&intent, &key)
    );
    assert_eq!(a.unwrap(), b.unwrap());
    assert!(matches!(
        f.journal
            .reserve(&intent, &IdempotencyKey::parse("different").unwrap())
            .await,
        Err(SaveJournalError::Busy)
    ));
    let mut body = f.body();
    body.message = "different body same key".into();
    assert!(matches!(
        f.journal.reserve(&f.intent(body), &key).await,
        Err(SaveJournalError::IdempotencyMismatch)
    ));
    let counts: (i64, i64) =
        sqlx::query_as("SELECT (SELECT count(*) FROM git_operations),(SELECT count(*) FROM jobs)")
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(counts, (1, 1));
}

#[sqlx::test(migrations = "../../migrations")]
async fn unknown_success_recovers_by_evidence_without_a_second_commit(pool: PgPool) {
    let f = fixture(pool).await;
    let ticket = f.ticket().await;
    f.source.mode.store(1, Ordering::SeqCst);
    assert_eq!(
        f.worker().run_once().await.unwrap(),
        SaveWork::Deferred(ticket.job_id)
    );
    assert_eq!(f.state().await.1, OLD);
    f.ready(ticket.job_id).await;
    assert_eq!(
        f.worker().run_once().await.unwrap(),
        SaveWork::Completed(ticket.job_id)
    );
    assert_eq!(f.source.writes.load(Ordering::SeqCst), 1);
    assert_eq!(f.state().await.1, NEW);
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_started_unknown_write_is_never_repeated_even_when_head_still_matches(pool: PgPool) {
    let f = fixture(pool).await;
    let ticket = f.ticket().await;
    f.source.mode.store(2, Ordering::SeqCst);
    assert_eq!(
        f.worker().run_once().await.unwrap(),
        SaveWork::Deferred(ticket.job_id)
    );
    f.ready(ticket.job_id).await;
    assert_eq!(
        f.worker().run_once().await.unwrap(),
        SaveWork::Deferred(ticket.job_id)
    );
    assert_eq!(f.source.writes.load(Ordering::SeqCst), 1);
    assert_eq!(f.state().await.1, OLD);
    assert!(matches!(
        f.journal
            .reserve(&f.intent(f.body()), &IdempotencyKey::parse("new").unwrap())
            .await,
        Err(SaveJournalError::Busy)
    ));
}

#[sqlx::test(migrations = "../../migrations")]
async fn crash_after_git_write_reclaims_only_observation_and_fences_the_old_worker(pool: PgPool) {
    let f = fixture(pool).await;
    let ticket = f.ticket().await;
    f.source.mode.store(3, Ordering::SeqCst);
    let worker = f.worker();
    let task = tokio::spawn(async move { worker.run_once().await });
    f.source.applied.notified().await;
    task.abort();
    let _ = task.await;
    sqlx::query("UPDATE jobs SET leased_until=clock_timestamp()-interval '1 second' WHERE id=$1")
        .bind(ticket.job_id)
        .execute(&f.pool)
        .await
        .unwrap();
    assert_eq!(
        f.worker().run_once().await.unwrap(),
        SaveWork::Completed(ticket.job_id)
    );
    assert_eq!(f.source.writes.load(Ordering::SeqCst), 1);
    assert_eq!(f.state().await.1, NEW);
}

#[sqlx::test(migrations = "../../migrations")]
async fn forged_matching_trailers_cannot_hide_wrong_content_extra_changes_or_mode_changes(
    pool: PgPool,
) {
    let f = fixture(pool).await;
    // Separate proposals/fixtures keep each unresolved mutation isolated.
    for mode in [4, 5, 6, 12, 13, 14, 15, 16, 17, 18] {
        let g = fixture(f.pool.clone()).await;
        let ticket = g.ticket().await;
        g.source.mode.store(mode, Ordering::SeqCst);
        assert_eq!(
            g.worker().run_once().await.unwrap(),
            SaveWork::Failed(ticket.job_id)
        );
        assert_eq!(g.state().await.1, OLD);
        let state: String = sqlx::query_scalar("SELECT state FROM git_operations WHERE id=$1")
            .bind(ticket.operation_id)
            .fetch_one(&g.pool)
            .await
            .unwrap();
        assert_eq!(state, "UNKNOWN");
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn permission_revocation_and_final_state_after_git_write_prevent_false_publication(
    pool: PgPool,
) {
    let f = fixture(pool).await;
    for mode in [7, 10] {
        let g = fixture(f.pool.clone()).await;
        let ticket = g.ticket().await;
        g.source.mode.store(mode, Ordering::SeqCst);
        assert_eq!(
            g.worker().run_once().await.unwrap(),
            SaveWork::Failed(ticket.job_id)
        );
        assert_eq!(g.state().await.1, OLD);
        assert_eq!(*g.source.head.lock().unwrap(), NEW);
        let state: String = sqlx::query_scalar("SELECT state FROM git_operations WHERE id=$1")
            .bind(ticket.operation_id)
            .fetch_one(&g.pool)
            .await
            .unwrap();
        assert_eq!(state, "UNKNOWN");
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn preflight_revocation_and_repository_reconfiguration_never_cross_the_write_boundary(
    pool: PgPool,
) {
    let f = fixture(pool).await;
    for hook in [1, 2] {
        let g = fixture(f.pool.clone()).await;
        let ticket = g.ticket().await;
        g.source.read_hook.store(hook, Ordering::SeqCst);
        assert_eq!(
            g.worker().run_once().await.unwrap(),
            SaveWork::Failed(ticket.job_id)
        );
        assert_eq!(g.source.writes.load(Ordering::SeqCst), 0);
        let state: String = sqlx::query_scalar("SELECT state FROM git_operations WHERE id=$1")
            .bind(ticket.operation_id)
            .fetch_one(&g.pool)
            .await
            .unwrap();
        assert_eq!(state, "FAILED");
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn reconciliation_keeps_a_verified_save_when_the_branch_has_later_external_descendants(
    pool: PgPool,
) {
    let f = fixture(pool).await;
    let ticket = f.ticket().await;
    f.source.mode.store(9, Ordering::SeqCst);
    assert_eq!(
        f.worker().run_once().await.unwrap(),
        SaveWork::Deferred(ticket.job_id)
    );
    f.ready(ticket.job_id).await;
    assert_eq!(
        f.worker().run_once().await.unwrap(),
        SaveWork::Completed(ticket.job_id)
    );
    assert_eq!(f.source.writes.load(Ordering::SeqCst), 1);
    assert_eq!(f.state().await.1, NEW);
    assert_eq!(*f.source.head.lock().unwrap(), LATER);
}

#[sqlx::test(migrations = "../../migrations")]
async fn provider_backoff_before_mutation_is_honored_and_does_not_claim_a_save(pool: PgPool) {
    let f = fixture(pool).await;
    let ticket = f.ticket().await;
    f.source.mode.store(8, Ordering::SeqCst);
    assert_eq!(
        f.worker().run_once().await.unwrap(),
        SaveWork::Deferred(ticket.job_id)
    );
    assert_eq!(f.source.writes.load(Ordering::SeqCst), 0);
    assert_eq!(f.worker().run_once().await.unwrap(), SaveWork::Idle);
    let delay: f64 = sqlx::query_scalar(
        "SELECT extract(epoch FROM available_at-clock_timestamp())::float8 FROM jobs WHERE id=$1",
    )
    .bind(ticket.job_id)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert!(delay > 15.0);
    f.ready(ticket.job_id).await;
    assert_eq!(
        f.worker().run_once().await.unwrap(),
        SaveWork::Completed(ticket.job_id)
    );
}

async fn actor(pool: &PgPool, organization: Uuid, role: &str) -> Uuid {
    let actor = Uuid::new_v4();
    sqlx::query("INSERT INTO users(id,issuer,external_subject,display_name) VALUES($1,'fixture',$1::text,'Synthetic additional editor')").bind(actor).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO memberships(organization_id,user_id,role) VALUES($1,$2,$3)")
        .bind(organization)
        .bind(actor)
        .bind(role)
        .execute(pool)
        .await
        .unwrap();
    actor
}
fn prepared(f: &Fixture, lease: &specspace_infrastructure::saves::SaveLease) -> PreparedSave {
    let snapshots = f.source.snapshots.lock().unwrap();
    let files = &snapshots[OLD];
    let catalog = serde_json::from_slice(&files[".specspace/catalog.json"].0).unwrap();
    lease
        .request()
        .unwrap()
        .prepare(SaveSnapshot {
            repository_id: f.repository,
            commit: CommitSha::parse(OLD).unwrap(),
            catalog,
            files: files
                .iter()
                .filter(|(path, _)| path.starts_with("docs/"))
                .map(|(path, (bytes, _))| SourceFile {
                    path: path.clone(),
                    markdown: String::from_utf8(bytes.clone()).unwrap(),
                })
                .collect(),
            asset_paths: Default::default(),
        })
        .unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
async fn current_scope_requires_active_membership_nonreader_and_current_edit_assignment(
    pool: PgPool,
) {
    let f = fixture(pool).await;
    let other = actor(&f.pool, f.organization, "CONTRIBUTOR").await;
    let reader = actor(&f.pool, f.organization, "READER").await;
    let admin = actor(&f.pool, f.organization, "ADMIN").await;
    let foreign = fixture(f.pool.clone()).await;
    for (id, expected) in [
        (other, SaveJournalError::Forbidden),
        (reader, SaveJournalError::Forbidden),
        (foreign.actor, SaveJournalError::NotFound),
    ] {
        assert_eq!(
            f.journal.context(id, f.proposal).await.err().unwrap(),
            expected
        );
    }
    sqlx::query("INSERT INTO proposal_collaborators(organization_id,proposal_id,user_id,access) VALUES($1,$2,$3,'READ')").bind(f.organization).bind(f.proposal).bind(other).execute(&f.pool).await.unwrap();
    assert_eq!(
        f.journal.context(other, f.proposal).await.err().unwrap(),
        SaveJournalError::Forbidden
    );
    sqlx::query(
        "UPDATE proposal_collaborators SET access='EDIT' WHERE proposal_id=$1 AND user_id=$2",
    )
    .bind(f.proposal)
    .bind(other)
    .execute(&f.pool)
    .await
    .unwrap();
    assert_eq!(
        f.journal.context(other, f.proposal).await.unwrap().actor(),
        other
    );
    assert_eq!(
        f.journal.context(admin, f.proposal).await.unwrap().actor(),
        admin
    );
    let intent = ValidatedSave::new(other, f.proposal, f.repository, f.body()).unwrap();
    f.journal
        .reserve(&intent, &IdempotencyKey::parse("collaborator").unwrap())
        .await
        .unwrap();
    sqlx::query("DELETE FROM proposal_collaborators WHERE proposal_id=$1 AND user_id=$2")
        .bind(f.proposal)
        .bind(other)
        .execute(&f.pool)
        .await
        .unwrap();
    assert!(matches!(
        f.worker().run_once().await.unwrap(),
        SaveWork::Failed(_)
    ));
    assert_eq!(f.source.reads.load(Ordering::SeqCst), 0);
    assert_eq!(f.source.writes.load(Ordering::SeqCst), 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn verification_rate_limit_after_write_remains_unknown_and_never_allows_a_second_write(
    pool: PgPool,
) {
    let f = fixture(pool).await;
    let ticket = f.ticket().await;
    f.source.mode.store(11, Ordering::SeqCst);
    assert_eq!(
        f.worker().run_once().await.unwrap(),
        SaveWork::Deferred(ticket.job_id)
    );
    assert_eq!(f.state().await.1, OLD);
    let state: String = sqlx::query_scalar("SELECT state FROM git_operations WHERE id=$1")
        .bind(ticket.operation_id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(state, "UNKNOWN");
    let delay: f64 = sqlx::query_scalar(
        "SELECT extract(epoch FROM available_at-clock_timestamp())::float8 FROM jobs WHERE id=$1",
    )
    .bind(ticket.job_id)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert!(delay > 21.0);
    assert!(matches!(
        f.journal
            .reserve(
                &f.intent(f.body()),
                &IdempotencyKey::parse("another").unwrap()
            )
            .await,
        Err(SaveJournalError::Busy)
    ));
    f.ready(ticket.job_id).await;
    assert_eq!(
        f.worker().run_once().await.unwrap(),
        SaveWork::Completed(ticket.job_id)
    );
    assert_eq!(f.source.writes.load(Ordering::SeqCst), 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn stale_lease_cannot_start_mutation_report_failure_or_extend_ownership(pool: PgPool) {
    let f = fixture(pool).await;
    let ticket = f.ticket().await;
    let old = f.journal.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    let plan = prepared(&f, &old);
    sqlx::query("UPDATE jobs SET leased_until=clock_timestamp()-interval '1 second' WHERE id=$1")
        .bind(ticket.job_id)
        .execute(&f.pool)
        .await
        .unwrap();
    let current = f.journal.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    assert!(!current.needs_reconciliation());
    assert!(matches!(
        f.journal.begin_mutation(&old, &plan).await,
        Err(SaveJournalError::LeaseLost)
    ));
    assert_eq!(
        f.journal.heartbeat(&old, 60).await,
        Err(SaveJournalError::LeaseLost)
    );
    assert_eq!(
        f.journal.fail(&old, "STALE", None, true).await,
        Err(SaveJournalError::LeaseLost)
    );
    f.journal
        .fail(&current, "CANCELLED_BEFORE_WRITE", None, true)
        .await
        .unwrap();
    assert_eq!(f.source.writes.load(Ordering::SeqCst), 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn database_preserves_the_original_scope_source_and_sealed_manifest(pool: PgPool) {
    let f = fixture(pool).await;
    let ticket = f.ticket().await;
    assert!(
        sqlx::query("UPDATE proposal_saves SET request='{}'::jsonb WHERE operation_id=$1")
            .bind(ticket.operation_id)
            .execute(&f.pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("UPDATE proposal_saves SET expected_version=2 WHERE operation_id=$1")
            .bind(ticket.operation_id)
            .execute(&f.pool)
            .await
            .is_err()
    );
    let lease = f.journal.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    let plan = prepared(&f, &lease);
    f.journal.begin_mutation(&lease, &plan).await.unwrap();
    assert!(sqlx::query("UPDATE proposal_saves SET manifest=manifest||'{\"extra\":1}'::jsonb WHERE operation_id=$1").bind(ticket.operation_id).execute(&f.pool).await.is_err());
    assert!(
        sqlx::query("UPDATE proposal_saves SET manifest_hash=repeat('0',64) WHERE operation_id=$1")
            .bind(ticket.operation_id)
            .execute(&f.pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM proposal_saves WHERE operation_id=$1")
            .bind(ticket.operation_id)
            .execute(&f.pool)
            .await
            .is_err()
    );
    f.journal
        .fail(&lease, "OBSERVATION_REQUIRED", None, false)
        .await
        .unwrap();
    let source: Option<Value> =
        sqlx::query_scalar("SELECT request FROM proposal_saves WHERE operation_id=$1")
            .bind(ticket.operation_id)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert!(source.is_some());
}

#[sqlx::test(migrations = "../../migrations")]
async fn attempt_exhaustion_preserves_unknown_reservations_and_audits_definite_failures(
    pool: PgPool,
) {
    let f = fixture(pool).await;
    for uncertain in [false, true] {
        let g = fixture(f.pool.clone()).await;
        let ticket = g.ticket().await;
        if uncertain {
            g.source.mode.store(2, Ordering::SeqCst);
            assert_eq!(
                g.worker().run_once().await.unwrap(),
                SaveWork::Deferred(ticket.job_id)
            );
        }
        sqlx::query(
            "UPDATE jobs SET attempts=max_attempts,available_at=clock_timestamp() WHERE id=$1",
        )
        .bind(ticket.job_id)
        .execute(&g.pool)
        .await
        .unwrap();
        assert_eq!(g.worker().run_once().await.unwrap(), SaveWork::Idle);
        let state: String = sqlx::query_scalar("SELECT state FROM git_operations WHERE id=$1")
            .bind(ticket.operation_id)
            .fetch_one(&g.pool)
            .await
            .unwrap();
        assert_eq!(state, if uncertain { "UNKNOWN" } else { "FAILED" });
        let result: Value = sqlx::query_scalar("SELECT result FROM jobs WHERE id=$1")
            .bind(ticket.job_id)
            .fetch_one(&g.pool)
            .await
            .unwrap();
        assert_eq!(
            result["phase"],
            if uncertain {
                "RECONCILIATION_REQUIRED"
            } else {
                "PREFLIGHT"
            }
        );
        let count:i64=sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE operation_id=$1 AND action='PROPOSAL_SAVE_EXHAUSTED'").bind(ticket.operation_id).fetch_one(&g.pool).await.unwrap();
        assert_eq!(count, 1);
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn bounded_validation_diagnostics_identify_incoming_references_without_exposing_source(
    pool: PgPool,
) {
    let f = fixture(pool).await;
    f.source
        .snapshots
        .lock()
        .unwrap()
        .get_mut(OLD)
        .unwrap()
        .get_mut("docs/a.md")
        .unwrap()
        .0
        .extend_from_slice(b"\n[incoming](ss:DOC-B#SEC-B)\n");
    let mut body = f.body();
    body.documents[1].markdown = document("DOC-B", "SEC-NEW", "replacement");
    let ticket = f
        .journal
        .reserve(
            &f.intent(body),
            &IdempotencyKey::parse("broken-link").unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        f.worker().run_once().await.unwrap(),
        SaveWork::Failed(ticket.job_id)
    );
    assert_eq!(f.source.writes.load(Ordering::SeqCst), 0);
    let result: Value = sqlx::query_scalar("SELECT result FROM jobs WHERE id=$1")
        .bind(ticket.job_id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(result["error_code"], "SAVE_VALIDATION_FAILED");
    let findings = result["details"]["findings"].as_array().unwrap();
    assert!(
        findings
            .iter()
            .any(|finding| finding["code"] == "SECTION_REFERENCE_MISSING"
                && finding["path"] == "docs/a.md")
    );
    assert!(result["details"].get("section_ids").is_none());
    assert!(!result.to_string().contains("[incoming]"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn image_hash_mime_and_pixels_are_checked_before_the_mutation_boundary(pool: PgPool) {
    let f = fixture(pool).await;
    let valid=include_bytes!("../../../../fixtures/repository/assets/5e3d382db4dd83d59aa5742793ad6b7903409e865c83bcbc54835049f043bc15.png").to_vec();
    for (bytes, accepted) in [(b"not an image".to_vec(), false), (valid, true)] {
        let g = fixture(f.pool.clone()).await;
        let path = format!("assets/{:x}.png", Sha256::digest(&bytes));
        g.source
            .snapshots
            .lock()
            .unwrap()
            .get_mut(OLD)
            .unwrap()
            .insert(path.clone(), (bytes, FileMode::Regular));
        let mut body = g.body();
        body.documents[0]
            .markdown
            .push_str(&format!("\n![description]({path})\n"));
        let ticket = g
            .journal
            .reserve(&g.intent(body), &IdempotencyKey::parse("image").unwrap())
            .await
            .unwrap();
        let result = g.worker().run_once().await.unwrap();
        assert_eq!(
            result,
            if accepted {
                SaveWork::Completed(ticket.job_id)
            } else {
                SaveWork::Failed(ticket.job_id)
            }
        );
        assert_eq!(
            g.source.writes.load(Ordering::SeqCst),
            usize::from(accepted)
        );
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn failed_database_publication_after_git_success_rolls_back_and_reconciles_once(
    pool: PgPool,
) {
    let f = fixture(pool).await;
    let ticket = f.ticket().await;
    sqlx::raw_sql("CREATE FUNCTION reject_save_publication() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.head_sha IS DISTINCT FROM OLD.head_sha THEN RAISE EXCEPTION 'synthetic storage failure'; END IF; RETURN NEW; END; $$; CREATE TRIGGER reject_save_publication BEFORE UPDATE ON proposals FOR EACH ROW EXECUTE FUNCTION reject_save_publication();").execute(&f.pool).await.unwrap();
    assert_eq!(
        f.worker().run_once().await.err().unwrap(),
        SaveJournalError::Unavailable
    );
    assert_eq!(f.state().await, ("APPROVED".into(), OLD.into(), 1));
    assert_eq!(*f.source.head.lock().unwrap(), NEW);
    let state: String = sqlx::query_scalar("SELECT state FROM git_operations WHERE id=$1")
        .bind(ticket.operation_id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(state, "RUNNING");
    sqlx::raw_sql("DROP TRIGGER reject_save_publication ON proposals; DROP FUNCTION reject_save_publication();").execute(&f.pool).await.unwrap();
    sqlx::query("UPDATE jobs SET leased_until=clock_timestamp()-interval '1 second' WHERE id=$1")
        .bind(ticket.job_id)
        .execute(&f.pool)
        .await
        .unwrap();
    assert_eq!(
        f.worker().run_once().await.unwrap(),
        SaveWork::Completed(ticket.job_id)
    );
    assert_eq!(f.source.writes.load(Ordering::SeqCst), 1);
    assert_eq!(f.state().await.1, NEW);
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE operation_id=$1 AND action='PROPOSAL_SAVED'",
    )
    .bind(ticket.operation_id)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(count, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn malformed_job_payload_cannot_publish_or_retarget_a_reserved_save(pool: PgPool) {
    let f = fixture(pool).await;
    let ticket = f.ticket().await;
    sqlx::query("UPDATE jobs SET payload=payload||'{\"unexpected\":true}'::jsonb WHERE id=$1")
        .bind(ticket.job_id)
        .execute(&f.pool)
        .await
        .unwrap();
    assert_eq!(
        f.worker().run_once().await.err().unwrap(),
        SaveJournalError::InvalidPlan
    );
    assert_eq!(f.source.reads.load(Ordering::SeqCst), 0);
    assert_eq!(f.source.writes.load(Ordering::SeqCst), 0);
    let result: Value = sqlx::query_scalar("SELECT result FROM jobs WHERE id=$1")
        .bind(ticket.job_id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(result["phase"], "PREFLIGHT");
    assert_eq!(result["error_code"], "INVALID_SAVE_PLAN");
    let raw: Option<Value> =
        sqlx::query_scalar("SELECT request FROM proposal_saves WHERE operation_id=$1")
            .bind(ticket.operation_id)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert!(raw.is_none());
}
