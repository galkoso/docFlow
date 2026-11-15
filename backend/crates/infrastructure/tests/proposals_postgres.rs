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
