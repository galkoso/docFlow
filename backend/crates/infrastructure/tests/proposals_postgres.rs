use specspace_application::proposal::{
    CreateProposal, IdempotencyKey, InitialMode, ProposalError, UiImpact, ValidatedProposal,
};
use specspace_infrastructure::proposals::PgProposals;
use sqlx::PgPool;
use uuid::Uuid;

struct Fixture {
    organization: Uuid,
    repository: Uuid,
    actor: Uuid,
    team: Uuid,
}
async fn fixture(pool: &PgPool, role: &str) -> Fixture {
    let (organization, repository, actor, team) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    sqlx::query("INSERT INTO organizations(id,name) VALUES($1,'Synthetic proposal organization')")
        .bind(organization)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO users(id,issuer,external_subject,display_name) VALUES($1,'fixture',$1::text,'Synthetic creator')").bind(actor).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO memberships(organization_id,user_id,role) VALUES($1,$2,$3)")
        .bind(organization)
        .bind(actor)
        .bind(role)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO repositories(id,organization_id,provider,remote_id,installation_id) VALUES($1,$2,'GITHUB','42','7')").bind(repository).bind(organization).execute(pool).await.unwrap();
    sqlx::query(
        "INSERT INTO teams(id,organization_id,name) VALUES($1,$2,'Synthetic responsible team')",
    )
    .bind(team)
    .bind(organization)
    .execute(pool)
    .await
    .unwrap();
    Fixture {
        organization,
        repository,
        actor,
        team,
    }
}
fn request(f: &Fixture, branch: &str) -> CreateProposal {
    CreateProposal {
        repository_id: f.repository,
        title: "הצעת שינוי חדשה".into(),
        branch: branch.into(),
        source_branch: "main".into(),
        expected_source_sha: "1".repeat(40),
        description: "תיאור מפורט של תכולת השינוי המוצע".into(),
        team_id: f.team,
        owner_id: None,
        initial_mode: InitialMode::Ai,
        ui_impact: UiImpact::No,
        ui_impact_reason: Some("שינוי לוגיקה בלבד".into()),
        links: vec![],
        creation_context_id: None,
    }
}
fn intent(f: &Fixture, branch: &str) -> ValidatedProposal {
    ValidatedProposal::validate(request(f, branch), f.actor).unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
async fn concurrent_replays_reserve_one_identity_operation_job_and_audit(pool: PgPool) {
    let f = fixture(&pool, "CONTRIBUTOR").await;
    let store = PgProposals::new(pool.clone());
    let input = intent(&f, "change/1-synthetic");
    let key = IdempotencyKey::parse("double-click").unwrap();
    assert!(
        store
            .existing(f.actor, &input, &key)
            .await
            .unwrap()
            .is_none()
    );
    let (a, b) = tokio::join!(
        store.reserve(f.actor, &input, &key),
        store.reserve(f.actor, &input, &key)
    );
    let ticket = a.unwrap();
    assert_eq!(ticket, b.unwrap());
    assert_eq!(
        store.existing(f.actor, &input, &key).await.unwrap(),
        Some(ticket.clone())
    );
    for table in [
        "proposals",
        "git_operations",
        "jobs",
        "audit_events",
        "outbox",
    ] {
        let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 1, "duplicate {table}");
    }
    let saved: (String, String, Option<String>, String) =
        sqlx::query_as("SELECT state,initial_mode,head_sha,branch FROM proposals WHERE id=$1")
            .bind(ticket.proposal_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        saved,
        (
            "PROVISIONING".into(),
            "ai".into(),
            None,
            "change/1-synthetic".into()
        )
    );
    let source: (Option<String>, Option<String>) =
        sqlx::query_as("SELECT base_sha,target_sha FROM proposals WHERE id=$1")
            .bind(ticket.proposal_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(source, (Some("1".repeat(40)), Some("1".repeat(40))));
    let operation: (String, String, String) =
        sqlx::query_as("SELECT state,expected_sha,request_hash FROM git_operations WHERE id=$1")
            .bind(ticket.operation_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        operation,
        ("PENDING".into(), "1".repeat(40), input.fingerprint().into())
    );
    let payload: serde_json::Value =
        sqlx::query_scalar("SELECT payload FROM jobs WHERE id=$1 AND state='QUEUED'")
            .bind(ticket.job_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(payload["repository_id"], f.repository.to_string());
    assert_eq!(payload["proposal_id"], ticket.proposal_id.to_string());
    let mut changed = request(&f, "change/1-synthetic");
    changed.description.push_str(" שינוי נוסף");
    let changed = ValidatedProposal::validate(changed, f.actor).unwrap();
    assert_eq!(
        store.reserve(f.actor, &changed, &key).await,
        Err(ProposalError::IdempotencyMismatch)
    );
    assert_eq!(
        store.existing(f.actor, &changed, &key).await,
        Err(ProposalError::IdempotencyMismatch)
    );
    sqlx::query("UPDATE memberships SET role='READER' WHERE user_id=$1")
        .bind(f.actor)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        store.existing(f.actor, &input, &key).await,
        Err(ProposalError::Forbidden)
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn branch_reservations_are_case_folded_permanent_and_races_roll_back_fully(pool: PgPool) {
    let f = fixture(&pool, "MAINTAINER").await;
    let store = PgProposals::new(pool.clone());
    let a = intent(&f, "change/Keep-Name");
    let b = intent(&f, "change/keep-name");
    let key_a = IdempotencyKey::parse("first").unwrap();
    let key_b = IdempotencyKey::parse("second").unwrap();
    let (a, b) = tokio::join!(
        store.reserve(f.actor, &a, &key_a),
        store.reserve(f.actor, &b, &key_b)
    );
    let ticket = match (a, b) {
        (Ok(ticket), Err(ProposalError::BranchTaken))
        | (Err(ProposalError::BranchTaken), Ok(ticket)) => ticket,
        other => panic!("invalid race result {other:?}"),
    };
    // Model a fully recorded provisioned branch later closed by its workflow.
    sqlx::query(
        "UPDATE proposals SET state='CLOSED',base_sha=$2,head_sha=$2,target_sha=$2 WHERE id=$1",
    )
    .bind(ticket.proposal_id)
    .bind("1".repeat(40))
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        store
            .reserve(
                f.actor,
                &intent(&f, "change/KEEP-NAME"),
                &IdempotencyKey::parse("third").unwrap()
            )
            .await,
        Err(ProposalError::BranchTaken)
    );
    for table in [
        "proposals",
        "git_operations",
        "jobs",
        "audit_events",
        "outbox",
    ] {
        let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 1, "failed reservation leaked {table}");
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn creation_rechecks_actor_owner_team_and_organization_before_reservation(pool: PgPool) {
    let f = fixture(&pool, "CONTRIBUTOR").await;
    let other = fixture(&pool, "ADMIN").await;
    let reader = fixture(&pool, "READER").await;
    let store = PgProposals::new(pool.clone());
    let key = IdempotencyKey::parse("scope-check").unwrap();
    assert_eq!(
        store.authorize_create(f.actor, other.repository).await,
        Err(ProposalError::NotFound)
    );
    assert_eq!(
        store
            .reserve(reader.actor, &intent(&reader, "change/reader"), &key)
            .await,
        Err(ProposalError::Forbidden)
    );
    let mut invalid = request(&f, "change/foreign-team");
    invalid.team_id = other.team;
    assert!(matches!(
        store
            .reserve(
                f.actor,
                &ValidatedProposal::validate(invalid, f.actor).unwrap(),
                &key
            )
            .await,
        Err(ProposalError::Validation(_))
    ));
    let mut invalid = request(&f, "change/foreign-owner");
    invalid.owner_id = Some(other.actor);
    assert!(matches!(
        store
            .reserve(
                f.actor,
                &ValidatedProposal::validate(invalid, f.actor).unwrap(),
                &key
            )
            .await,
        Err(ProposalError::Validation(_))
    ));
    assert_eq!(
        store.authorize_create(f.actor, f.repository).await.unwrap(),
        f.organization
    );
    sqlx::query("UPDATE users SET status='DISABLED' WHERE id=$1")
        .bind(f.actor)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        store
            .reserve(f.actor, &intent(&f, "change/revoked"), &key)
            .await,
        Err(ProposalError::NotFound)
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM proposals")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn provisioning_reclaims_only_observation_after_mutation_and_fences_old_owners(pool: PgPool) {
    use specspace_infrastructure::provisioning::{
        FailureCertainty, PgProvisioning, ProvisionError,
    };
    let f = fixture(&pool, "CONTRIBUTOR").await;
    let input = intent(&f, "change/lease-recovery");
    let ticket = PgProposals::new(pool.clone())
        .reserve(f.actor, &input, &IdempotencyKey::parse("recovery").unwrap())
        .await
        .unwrap();
    let journal = PgProvisioning::new(pool.clone());
    // The generic worker must not steal work owned by this protocol.
    assert!(
        specspace_infrastructure::lease_job(&pool, Uuid::new_v4(), 60)
            .await
            .unwrap()
            .is_none()
    );
    let old = journal.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    assert!(!old.needs_reconciliation());
    assert!(journal.claim(Uuid::new_v4(), 60).await.unwrap().is_none());
    journal.begin_mutation(&old).await.unwrap();
    sqlx::query("UPDATE jobs SET leased_until=clock_timestamp()-interval '1 second' WHERE id=$1")
        .bind(old.id)
        .execute(&pool)
        .await
        .unwrap();
    let replacement = journal.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    assert!(replacement.needs_reconciliation());
    assert_eq!(
        journal.begin_mutation(&replacement).await,
        Err(ProvisionError::InvalidJob)
    );
    assert_eq!(
        journal.heartbeat(&old, 60).await,
        Err(ProvisionError::LeaseLost)
    );
    assert_eq!(
        journal.complete(&old, input.source()).await,
        Err(ProvisionError::LeaseLost)
    );
    assert_eq!(
        journal
            .record_failure(&old, "STALE_WORKER", None, FailureCertainty::ObserveJournal)
            .await,
        Err(ProvisionError::LeaseLost)
    );
    journal
        .complete(&replacement, input.source())
        .await
        .unwrap();
    let saved: (String, String, String, String, i64) = sqlx::query_as(
        "SELECT state,base_sha,head_sha,target_sha,version FROM proposals WHERE id=$1",
    )
    .bind(ticket.proposal_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        saved,
        (
            "DRAFT".into(),
            "1".repeat(40),
            "1".repeat(40),
            "1".repeat(40),
            2
        )
    );
    let result: serde_json::Value =
        sqlx::query_scalar("SELECT result FROM jobs WHERE id=$1 AND state='SUCCEEDED'")
            .bind(ticket.job_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(result["reconciled"], true);
    assert_eq!(result["phase"], "READY");
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE action='PROPOSAL_CREATED'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 1);
    assert!(journal.claim(Uuid::new_v4(), 60).await.unwrap().is_none());
}

#[sqlx::test(migrations = "../../migrations")]
async fn interrupted_preflight_is_safe_but_started_failures_remain_unknown(pool: PgPool) {
    use specspace_infrastructure::provisioning::{
        FailureCertainty, PgProvisioning, ProvisionError,
    };
    let f = fixture(&pool, "CONTRIBUTOR").await;
    let input = intent(&f, "change/phases");
    PgProposals::new(pool.clone())
        .reserve(f.actor, &input, &IdempotencyKey::parse("phases").unwrap())
        .await
        .unwrap();
    let journal = PgProvisioning::new(pool.clone());
    let preflight = journal.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    sqlx::query("UPDATE jobs SET leased_until=clock_timestamp()-interval '1 second' WHERE id=$1")
        .bind(preflight.id)
        .execute(&pool)
        .await
        .unwrap();
    let work = journal.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    assert!(!work.needs_reconciliation());
    assert_eq!(
        journal.begin_mutation(&preflight).await,
        Err(ProvisionError::LeaseLost)
    );
    journal.begin_mutation(&work).await.unwrap();
    assert!(
        journal
            .record_failure(
                &work,
                "CREATE_RESULT_UNKNOWN",
                Some(17),
                FailureCertainty::ObserveJournal
            )
            .await
            .unwrap()
    );
    assert!(journal.claim(Uuid::new_v4(), 60).await.unwrap().is_none());
    let delayed: bool = sqlx::query_scalar(
        "SELECT available_at>clock_timestamp()+interval '14 seconds' FROM jobs WHERE id=$1",
    )
    .bind(work.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(delayed);
    sqlx::query("UPDATE jobs SET available_at=clock_timestamp()-interval '1 second' WHERE id=$1")
        .bind(work.id)
        .execute(&pool)
        .await
        .unwrap();
    let recovery = journal.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    assert!(recovery.needs_reconciliation());
    // Even a definite read rejection cannot turn an UNKNOWN journal into a
    // fresh write attempt. A rejected mutation can be retried only from RUNNING.
    assert!(
        journal
            .record_failure(
                &recovery,
                "PROVIDER_RATE_LIMIT",
                Some(17),
                FailureCertainty::DefinitelyNotApplied
            )
            .await
            .unwrap()
    );
    let state: String = sqlx::query_scalar("SELECT state FROM git_operations")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state, "UNKNOWN");
    sqlx::query(
        "UPDATE jobs SET attempts=max_attempts,available_at=clock_timestamp()-interval '1 second'",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert!(journal.claim(Uuid::new_v4(), 60).await.unwrap().is_none());
    let state: (String, String) = sqlx::query_as(
        "SELECT j.state,o.state FROM jobs j JOIN git_operations o ON o.id=j.operation_id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(state, ("FAILED".into(), "UNKNOWN".into()));
    let result: serde_json::Value = sqlx::query_scalar("SELECT result FROM jobs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(result["error_code"], "CREATE_RESULT_UNKNOWN");
    assert_eq!(result["phase"], "RECONCILIATION_REQUIRED");
}

#[sqlx::test(migrations = "../../migrations")]
async fn provisioning_rechecks_revocation_before_write_and_before_publication(pool: PgPool) {
    use specspace_infrastructure::provisioning::{
        FailureCertainty, PgProvisioning, ProvisionError,
    };
    let f = fixture(&pool, "CONTRIBUTOR").await;
    let input = intent(&f, "change/revocation");
    PgProposals::new(pool.clone())
        .reserve(
            f.actor,
            &input,
            &IdempotencyKey::parse("revocation").unwrap(),
        )
        .await
        .unwrap();
    let journal = PgProvisioning::new(pool.clone());
    let work = journal.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    sqlx::query("UPDATE memberships SET role='READER' WHERE user_id=$1")
        .bind(f.actor)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        journal.begin_mutation(&work).await,
        Err(ProvisionError::AccessRevoked)
    );
    sqlx::query("UPDATE memberships SET role='CONTRIBUTOR' WHERE user_id=$1")
        .bind(f.actor)
        .execute(&pool)
        .await
        .unwrap();
    journal.begin_mutation(&work).await.unwrap();
    sqlx::query("UPDATE users SET status='DISABLED' WHERE id=$1")
        .bind(f.actor)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        journal.complete(&work, input.source()).await,
        Err(ProvisionError::AccessRevoked)
    );
    assert!(
        !journal
            .record_failure(
                &work,
                "PERMISSION_REVOKED",
                None,
                FailureCertainty::ObserveJournal
            )
            .await
            .unwrap()
    );
    let saved: (String,Option<String>,String) = sqlx::query_as("SELECT p.state,p.head_sha,o.state FROM proposals p JOIN git_operations o ON o.proposal_id=p.id").fetch_one(&pool).await.unwrap();
    assert_eq!(saved, ("PROVISIONING".into(), None, "UNKNOWN".into()));
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE action='PROPOSAL_CREATED'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn malformed_provisioning_payload_is_terminal_without_retargeting_the_plan(pool: PgPool) {
    use specspace_infrastructure::provisioning::{PgProvisioning, ProvisionError};
    let f = fixture(&pool, "CONTRIBUTOR").await;
    let input = intent(&f, "change/malformed");
    PgProposals::new(pool.clone())
        .reserve(
            f.actor,
            &input,
            &IdempotencyKey::parse("malformed").unwrap(),
        )
        .await
        .unwrap();
    sqlx::query("UPDATE jobs SET payload=jsonb_set(payload,'{repository_id}',to_jsonb($1::text))")
        .bind(Uuid::new_v4().to_string())
        .execute(&pool)
        .await
        .unwrap();
    let journal = PgProvisioning::new(pool.clone());
    assert!(matches!(
        journal.claim(Uuid::new_v4(), 60).await,
        Err(ProvisionError::InvalidJob)
    ));
    assert!(journal.claim(Uuid::new_v4(), 60).await.unwrap().is_none());
    let result: serde_json::Value =
        sqlx::query_scalar("SELECT result FROM jobs WHERE state='FAILED'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(result["error_code"], "INVALID_PROVISIONING_JOB");
    let state: String = sqlx::query_scalar("SELECT state FROM git_operations")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state, "PENDING");
}

struct WriteProvider {
    main: std::sync::atomic::AtomicU8,
    branch: std::sync::Mutex<Option<specspace_git_adapter::CommitSha>>,
    mode: std::sync::atomic::AtomicU8,
    requests: std::sync::atomic::AtomicUsize,
    entered: tokio::sync::Notify,
    pool: PgPool,
    actor: Uuid,
}
#[async_trait::async_trait]
impl specspace_git_adapter::GitProvider for WriteProvider {
    async fn main_head(
        &self,
    ) -> Result<specspace_git_adapter::CommitSha, specspace_git_adapter::GitError> {
        specspace_git_adapter::CommitSha::parse(
            if self.main.load(std::sync::atomic::Ordering::SeqCst) == 0 {
                "1".repeat(40)
            } else {
                "2".repeat(40)
            },
        )
    }
    async fn branch_head(
        &self,
        _: &specspace_domain::BranchName,
    ) -> Result<specspace_git_adapter::CommitSha, specspace_git_adapter::GitError> {
        let branch = self.branch.lock().unwrap().clone();
        if branch.is_some() && self.mode.load(std::sync::atomic::Ordering::SeqCst) == 8 {
            return Err(specspace_git_adapter::GitError::RateLimited {
                retry_after_seconds: Some(17),
            });
        }
        branch.ok_or(specspace_git_adapter::GitError::BranchMissing)
    }
    async fn create_branch(
        &self,
        _: &specspace_domain::BranchName,
        source: &specspace_git_adapter::CommitSha,
    ) -> Result<specspace_git_adapter::CommitSha, specspace_git_adapter::GitError> {
        use specspace_git_adapter::{CommitSha, GitError};
        use std::sync::atomic::Ordering;
        self.requests.fetch_add(1, Ordering::SeqCst);
        let mode = self.mode.load(Ordering::SeqCst);
        if mode == 2 {
            return Err(GitError::RateLimited {
                retry_after_seconds: Some(17),
            });
        }
        if mode == 9 {
            return Err(GitError::ResultUnknown);
        }
        assert!(
            self.branch.lock().unwrap().is_none(),
            "worker attempted a duplicate ref mutation"
        );
        *self.branch.lock().unwrap() = Some(if mode == 7 {
            CommitSha::parse("2".repeat(40)).unwrap()
        } else {
            source.clone()
        });
        if mode == 3 {
            sqlx::query("UPDATE users SET status='DISABLED' WHERE id=$1")
                .bind(self.actor)
                .execute(&self.pool)
                .await
                .unwrap();
        }
        if mode == 4 {
            self.entered.notify_one();
            return std::future::pending().await;
        }
        if mode == 1 || mode == 7 {
            return Err(GitError::ResultUnknown);
        }
        Ok(source.clone())
    }
    async fn commit(
        &self,
        _: specspace_git_adapter::CommitRequest<'_>,
    ) -> Result<specspace_git_adapter::CommitSha, specspace_git_adapter::GitError> {
        panic!("provisioning must not commit content")
    }
    async fn read_text(
        &self,
        _: &specspace_git_adapter::CommitSha,
        _: &specspace_domain::RepositoryPath,
    ) -> Result<String, specspace_git_adapter::GitError> {
        panic!("provisioning must not read book content")
    }
    async fn read_bytes(
        &self,
        _: &specspace_git_adapter::CommitSha,
        _: &specspace_domain::RepositoryPath,
        _: usize,
    ) -> Result<Vec<u8>, specspace_git_adapter::GitError> {
        panic!("provisioning must not read assets")
    }
    async fn list_files(
        &self,
        _: &specspace_git_adapter::CommitSha,
    ) -> Result<Vec<specspace_git_adapter::FileEntry>, specspace_git_adapter::GitError> {
        panic!("provisioning must not read trees")
    }
    async fn commit_receipt(
        &self,
        _: &specspace_git_adapter::CommitSha,
    ) -> Result<specspace_git_adapter::CommitReceipt, specspace_git_adapter::GitError> {
        Err(specspace_git_adapter::GitError::ProviderRejected)
    }
    async fn commit_metadata(
        &self,
        _: &specspace_git_adapter::CommitSha,
    ) -> Result<specspace_git_adapter::CommitMetadata, specspace_git_adapter::GitError> {
        panic!("provisioning must not read commit content")
    }
    async fn is_ancestor(
        &self,
        _: &specspace_git_adapter::CommitSha,
        _: &specspace_git_adapter::CommitSha,
    ) -> Result<bool, specspace_git_adapter::GitError> {
        panic!("creation is pinned to the displayed source")
    }
}
struct WriteFactory {
    repository: Uuid,
    provider: std::sync::Arc<WriteProvider>,
}
#[async_trait::async_trait]
impl specspace_application::reader::BookProviderFactory for WriteFactory {
    async fn provider(
        &self,
        repository: Uuid,
    ) -> Result<
        std::sync::Arc<dyn specspace_git_adapter::GitProvider>,
        specspace_application::book::BookError,
    > {
        assert_eq!(repository, self.repository);
        Ok(self.provider.clone())
    }
}
async fn worker_fixture(
    pool: &PgPool,
    mode: u8,
) -> (
    Fixture,
    specspace_infrastructure::proposals::CreationTicket,
    std::sync::Arc<WriteProvider>,
    std::sync::Arc<specspace_infrastructure::proposal_worker::ProposalWorker>,
) {
    use specspace_infrastructure::{proposal_worker::ProposalWorker, provisioning::PgProvisioning};
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicU8, AtomicUsize},
    };
    let f = fixture(pool, "CONTRIBUTOR").await;
    let ticket = PgProposals::new(pool.clone())
        .reserve(
            f.actor,
            &intent(&f, "change/worker"),
            &IdempotencyKey::parse("worker").unwrap(),
        )
        .await
        .unwrap();
    let provider = Arc::new(WriteProvider {
        main: AtomicU8::new(0),
        branch: Mutex::new(None),
        mode: AtomicU8::new(mode),
        requests: AtomicUsize::new(0),
        entered: tokio::sync::Notify::new(),
        pool: pool.clone(),
        actor: f.actor,
    });
    let worker = Arc::new(
        ProposalWorker::new(
            Arc::new(PgProvisioning::new(pool.clone())),
            Arc::new(WriteFactory {
                repository: f.repository,
                provider: provider.clone(),
            }),
            std::time::Duration::from_secs(30),
        )
        .unwrap(),
    );
    (f, ticket, provider, worker)
}
async fn advance(pool: &PgPool) {
    sqlx::query("UPDATE jobs SET available_at=clock_timestamp()-interval '1 second'")
        .execute(pool)
        .await
        .unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn worker_creates_one_empty_diff_branch_then_publishes_verified_identity(pool: PgPool) {
    use specspace_infrastructure::proposal_worker::ProvisionWork;
    let (_, ticket, provider, worker) = worker_fixture(&pool, 0).await;
    assert_eq!(
        worker.run_once().await.unwrap(),
        ProvisionWork::Completed(ticket.job_id)
    );
    assert_eq!(worker.run_once().await.unwrap(), ProvisionWork::Idle);
    assert_eq!(
        provider.requests.load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    let state: (String,String,String,String) = sqlx::query_as("SELECT p.state,p.base_sha,p.head_sha,j.state FROM proposals p JOIN git_operations o ON o.proposal_id=p.id JOIN jobs j ON j.operation_id=o.id").fetch_one(&pool).await.unwrap();
    assert_eq!(
        state,
        (
            "DRAFT".into(),
            "1".repeat(40),
            "1".repeat(40),
            "SUCCEEDED".into()
        )
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn worker_reconciles_unknown_success_without_another_create_request(pool: PgPool) {
    use specspace_infrastructure::proposal_worker::ProvisionWork;
    let (_, ticket, provider, worker) = worker_fixture(&pool, 1).await;
    assert_eq!(
        worker.run_once().await.unwrap(),
        ProvisionWork::Deferred(ticket.job_id)
    );
    advance(&pool).await;
    assert_eq!(
        worker.run_once().await.unwrap(),
        ProvisionWork::Completed(ticket.job_id)
    );
    assert_eq!(
        provider.requests.load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    let result: serde_json::Value = sqlx::query_scalar("SELECT result FROM jobs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(result["reconciled"], true);
}

#[sqlx::test(migrations = "../../migrations")]
async fn worker_honors_rate_limits_before_mutation_and_after_accepted_write(pool: PgPool) {
    use specspace_infrastructure::proposal_worker::ProvisionWork;
    use std::sync::atomic::Ordering;
    let (_, ticket, provider, worker) = worker_fixture(&pool, 2).await;
    assert_eq!(
        worker.run_once().await.unwrap(),
        ProvisionWork::Deferred(ticket.job_id)
    );
    let state: String = sqlx::query_scalar("SELECT state FROM git_operations")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state, "PENDING");
    let delayed: bool =
        sqlx::query_scalar("SELECT available_at>clock_timestamp()+interval '14 seconds' FROM jobs")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(delayed);
    assert_eq!(worker.run_once().await.unwrap(), ProvisionWork::Idle);
    provider.mode.store(8, Ordering::SeqCst);
    advance(&pool).await;
    assert_eq!(
        worker.run_once().await.unwrap(),
        ProvisionWork::Deferred(ticket.job_id)
    );
    let state: String = sqlx::query_scalar("SELECT state FROM git_operations")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state, "UNKNOWN");
    let delayed: bool =
        sqlx::query_scalar("SELECT available_at>clock_timestamp()+interval '14 seconds' FROM jobs")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(delayed);
    provider.mode.store(0, Ordering::SeqCst);
    advance(&pool).await;
    assert_eq!(
        worker.run_once().await.unwrap(),
        ProvisionWork::Completed(ticket.job_id)
    );
    assert_eq!(provider.requests.load(Ordering::SeqCst), 2); // one rejection, one accepted write
}

#[sqlx::test(migrations = "../../migrations")]
async fn worker_shutdown_after_git_write_recovers_through_observation_only(pool: PgPool) {
    use specspace_infrastructure::proposal_worker::ProvisionWork;
    use std::sync::atomic::Ordering;
    let (_, ticket, provider, worker) = worker_fixture(&pool, 4).await;
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let running = {
        let worker = worker.clone();
        tokio::spawn(async move { worker.run(stopped).await })
    };
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        provider.entered.notified(),
    )
    .await
    .unwrap();
    assert!(!running.is_finished());
    stop.send(true).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(1), running)
        .await
        .unwrap()
        .unwrap();
    sqlx::query("UPDATE jobs SET leased_until=clock_timestamp()-interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    provider.mode.store(0, Ordering::SeqCst);
    assert_eq!(
        worker.run_once().await.unwrap(),
        ProvisionWork::Completed(ticket.job_id)
    );
    assert_eq!(provider.requests.load(Ordering::SeqCst), 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn worker_rejects_moved_source_existing_refs_and_revocation_without_false_publication(
    pool: PgPool,
) {
    use specspace_infrastructure::proposal_worker::ProvisionWork;
    use std::sync::atomic::Ordering;
    let (f, ticket, provider, worker) = worker_fixture(&pool, 0).await;
    provider.main.store(1, Ordering::SeqCst);
    assert_eq!(
        worker.run_once().await.unwrap(),
        ProvisionWork::Failed(ticket.job_id)
    );
    assert_eq!(provider.requests.load(Ordering::SeqCst), 0);
    let result: serde_json::Value = sqlx::query_scalar("SELECT result FROM jobs WHERE id=$1")
        .bind(ticket.job_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(result["error_code"], "SOURCE_MOVED");
    let input = intent(&f, "change/another");
    let ticket = PgProposals::new(pool.clone())
        .reserve(f.actor, &input, &IdempotencyKey::parse("existing").unwrap())
        .await
        .unwrap();
    provider.main.store(0, Ordering::SeqCst);
    *provider.branch.lock().unwrap() = Some(input.source().clone());
    assert_eq!(
        worker.run_once().await.unwrap(),
        ProvisionWork::Failed(ticket.job_id)
    );
    assert_eq!(provider.requests.load(Ordering::SeqCst), 0);
    let ticket = PgProposals::new(pool.clone())
        .reserve(
            f.actor,
            &intent(&f, "change/revoke"),
            &IdempotencyKey::parse("revoke").unwrap(),
        )
        .await
        .unwrap();
    *provider.branch.lock().unwrap() = None;
    provider.mode.store(3, Ordering::SeqCst);
    assert_eq!(
        worker.run_once().await.unwrap(),
        ProvisionWork::Failed(ticket.job_id)
    );
    let state: (String,String) = sqlx::query_as("SELECT p.state,o.state FROM proposals p JOIN git_operations o ON o.proposal_id=p.id WHERE p.id=$1").bind(ticket.proposal_id).fetch_one(&pool).await.unwrap();
    assert_eq!(state, ("PROVISIONING".into(), "UNKNOWN".into()));
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE action='PROPOSAL_CREATED'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn worker_never_rewrites_a_diverged_or_unobserved_uncertain_ref(pool: PgPool) {
    use specspace_infrastructure::proposal_worker::ProvisionWork;
    let (_, ticket, provider, worker) = worker_fixture(&pool, 7).await;
    assert_eq!(
        worker.run_once().await.unwrap(),
        ProvisionWork::Deferred(ticket.job_id)
    );
    advance(&pool).await;
    assert_eq!(
        worker.run_once().await.unwrap(),
        ProvisionWork::Failed(ticket.job_id)
    );
    assert_eq!(
        provider.requests.load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    assert_eq!(
        provider.branch.lock().unwrap().as_ref().unwrap().as_str(),
        "2".repeat(40)
    );
    let (f, ticket, provider, worker) = worker_fixture(&pool, 9).await;
    // This second fixture shares the same DB but has an independent scoped source.
    for _ in 0..5 {
        let _ = worker.run_once().await.unwrap();
        advance(&pool).await;
    }
    assert_eq!(worker.run_once().await.unwrap(), ProvisionWork::Idle);
    assert_eq!(
        provider.requests.load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    let state: (String,String,String) = sqlx::query_as("SELECT p.state,o.state,j.state FROM proposals p JOIN git_operations o ON o.proposal_id=p.id JOIN jobs j ON j.operation_id=o.id WHERE p.id=$1 AND p.organization_id=$2")
        .bind(ticket.proposal_id).bind(f.organization).fetch_one(&pool).await.unwrap();
    assert_eq!(
        state,
        ("PROVISIONING".into(), "UNKNOWN".into(), "FAILED".into())
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn provisioning_never_retargets_a_reserved_job_after_repository_reconfiguration(
    pool: PgPool,
) {
    use specspace_infrastructure::{
        proposal_worker::ProvisionWork,
        provisioning::{PgProvisioning, ProvisionError},
    };
    let (f, ticket, provider, worker) = worker_fixture(&pool, 0).await;
    sqlx::query("UPDATE repositories SET remote_id='different-remote' WHERE id=$1")
        .bind(f.repository)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(worker.run_once().await, Err(ProvisionError::InvalidJob));
    assert_eq!(
        provider.requests.load(std::sync::atomic::Ordering::SeqCst),
        0
    );
    let state: String = sqlx::query_scalar("SELECT state FROM jobs WHERE id=$1")
        .bind(ticket.job_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state, "FAILED");
    assert_eq!(worker.run_once().await.unwrap(), ProvisionWork::Idle);
    let (f, ticket, _, _) = worker_fixture(&pool, 0).await;
    let journal = PgProvisioning::new(pool.clone());
    let lease = journal.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    journal.begin_mutation(&lease).await.unwrap();
    sqlx::query("UPDATE repositories SET installation_id='different-installation' WHERE id=$1")
        .bind(f.repository)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        journal.complete(&lease, lease.source()).await,
        Err(ProvisionError::AccessRevoked)
    );
    let state: String = sqlx::query_scalar("SELECT state FROM proposals WHERE id=$1")
        .bind(ticket.proposal_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state, "PROVISIONING");
}

#[sqlx::test(migrations = "../../migrations")]
async fn preview_numbers_share_the_proposal_sequence_and_are_consumed_once(pool: PgPool) {
    use specspace_infrastructure::creation_context::PgCreationContexts;
    let f = fixture(&pool, "CONTRIBUTOR").await;
    let contexts = PgCreationContexts::new(pool.clone());
    let store = PgProposals::new(pool.clone());
    let source = specspace_git_adapter::CommitSha::parse("1".repeat(40)).unwrap();
    let key = IdempotencyKey::parse("preview").unwrap();
    let (a, b) = tokio::join!(
        contexts.allocate(f.actor, f.repository, &source, &key),
        contexts.allocate(f.actor, f.repository, &source, &key)
    );
    let preview = a.unwrap();
    let repeated = b.unwrap();
    assert_eq!(preview.context_id, repeated.context_id);
    assert_eq!(preview.number, repeated.number);
    let second = contexts
        .allocate(
            f.actor,
            f.repository,
            &source,
            &IdempotencyKey::parse("second-preview").unwrap(),
        )
        .await
        .unwrap();
    assert!(second.number > preview.number);
    let legacy = store
        .reserve(
            f.actor,
            &intent(&f, "change/legacy"),
            &IdempotencyKey::parse("legacy").unwrap(),
        )
        .await
        .unwrap();
    assert!(legacy.number > second.number);
    let mut body = request(&f, &format!("change/{}-new-proposal", preview.number));
    body.creation_context_id = Some(preview.context_id);
    let input = ValidatedProposal::validate(body.clone(), f.actor).unwrap();
    let create_key = IdempotencyKey::parse("consume").unwrap();
    let ticket = store.reserve(f.actor, &input, &create_key).await.unwrap();
    assert_eq!(ticket.number, preview.number);
    sqlx::query(
        "UPDATE proposal_creation_contexts SET expires_at=clock_timestamp()-interval '1 second'",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        store.reserve(f.actor, &input, &create_key).await.unwrap(),
        ticket,
        "replay survives context expiry and consumption"
    );
    body.branch = "change/duplicate-context".into();
    assert_eq!(
        store
            .reserve(
                f.actor,
                &ValidatedProposal::validate(body, f.actor).unwrap(),
                &IdempotencyKey::parse("second-consumer").unwrap()
            )
            .await,
        Err(ProposalError::ContextUsed)
    );
    assert!(matches!(
        contexts
            .existing(
                f.actor,
                f.repository,
                &IdempotencyKey::parse("second-preview").unwrap()
            )
            .await,
        Err(ProposalError::ContextExpired)
    ));
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 2);
}

#[sqlx::test(migrations = "../../migrations")]
async fn preview_binding_rejects_different_actors_sources_configuration_and_expiry(pool: PgPool) {
    use specspace_infrastructure::creation_context::PgCreationContexts;
    let f = fixture(&pool, "CONTRIBUTOR").await;
    let foreign = fixture(&pool, "ADMIN").await;
    let contexts = PgCreationContexts::new(pool.clone());
    let store = PgProposals::new(pool.clone());
    let source = specspace_git_adapter::CommitSha::parse("1".repeat(40)).unwrap();
    let preview = contexts
        .allocate(
            f.actor,
            f.repository,
            &source,
            &IdempotencyKey::parse("binding").unwrap(),
        )
        .await
        .unwrap();
    let mut body = request(&foreign, "change/foreign-preview");
    body.creation_context_id = Some(preview.context_id);
    assert_eq!(
        store
            .check_context(
                foreign.actor,
                &ValidatedProposal::validate(body, foreign.actor).unwrap()
            )
            .await,
        Err(ProposalError::NotFound)
    );
    let mut body = request(&f, "change/moved-preview");
    body.creation_context_id = Some(preview.context_id);
    body.expected_source_sha = "2".repeat(40);
    assert_eq!(
        store
            .check_context(
                f.actor,
                &ValidatedProposal::validate(body, f.actor).unwrap()
            )
            .await,
        Err(ProposalError::ContextMismatch)
    );
    let mut body = request(&f, "change/expired-preview");
    body.creation_context_id = Some(preview.context_id);
    let input = ValidatedProposal::validate(body, f.actor).unwrap();
    sqlx::query("UPDATE repositories SET remote_id='changed' WHERE id=$1")
        .bind(f.repository)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        store.check_context(f.actor, &input).await,
        Err(ProposalError::ContextMismatch)
    );
    sqlx::query(
        "UPDATE proposal_creation_contexts SET expires_at=clock_timestamp()-interval '1 second'",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        store
            .reserve(
                f.actor,
                &input,
                &IdempotencyKey::parse("expired-create").unwrap()
            )
            .await,
        Err(ProposalError::ContextExpired)
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM proposals")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn different_creation_keys_cannot_consume_one_context_concurrently(pool: PgPool) {
    use specspace_infrastructure::creation_context::PgCreationContexts;
    let f = fixture(&pool, "CONTRIBUTOR").await;
    let source = specspace_git_adapter::CommitSha::parse("1".repeat(40)).unwrap();
    let preview = PgCreationContexts::new(pool.clone())
        .allocate(
            f.actor,
            f.repository,
            &source,
            &IdempotencyKey::parse("one-number").unwrap(),
        )
        .await
        .unwrap();
    let mut a = request(&f, "change/first-consumer");
    a.creation_context_id = Some(preview.context_id);
    let mut b = a.clone();
    b.branch = "change/second-consumer".into();
    let (a, b) = (
        ValidatedProposal::validate(a, f.actor).unwrap(),
        ValidatedProposal::validate(b, f.actor).unwrap(),
    );
    let store = PgProposals::new(pool.clone());
    let (ka, kb) = (
        IdempotencyKey::parse("consumer-a").unwrap(),
        IdempotencyKey::parse("consumer-b").unwrap(),
    );
    let (a, b) = tokio::join!(
        store.reserve(f.actor, &a, &ka),
        store.reserve(f.actor, &b, &kb)
    );
    let ticket = match (a, b) {
        (Ok(ticket), Err(ProposalError::ContextUsed))
        | (Err(ProposalError::ContextUsed), Ok(ticket)) => ticket,
        other => panic!("invalid context race: {other:?}"),
    };
    assert_eq!(ticket.number, preview.number);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM proposals")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}
