use specspace_application::{
    comparison_reader::ComparisonReadError,
    proposal::UiImpact,
    review_configuration::{
        ReviewArea, ReviewConfigurationRequest, ReviewSettings, ReviewerSelection,
    },
    review_context::{ReviewAuthorization, ReviewContextError, ReviewExpectation},
};
use specspace_domain::policy::{ReviewDomain, Role};
use specspace_git_adapter::CommitSha;
use specspace_infrastructure::{
    review_authorization::PgReviewAuthorization, review_configuration::PgReviewConfiguration,
};
use sqlx::PgPool;
use uuid::Uuid;
#[path = "review_authorization_postgres/product_questions.rs"]
mod product_questions;
#[path = "review_authorization_postgres/snapshots.rs"]
mod snapshots;
#[path = "review_authorization_postgres/submissions.rs"]
mod submissions;

struct Fixture {
    org: Uuid,
    repository: Uuid,
    proposal: Uuid,
    owner: Uuid,
    product: Uuid,
    development: Uuid,
}
async fn user(pool: &PgPool, org: Uuid, role: &str, domains: Vec<&str>) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO users(id,issuer,external_subject,display_name) VALUES($1,'review-scope',$1::text,'Synthetic reviewer')").bind(id).execute(pool).await.unwrap();
    sqlx::query(
        "INSERT INTO memberships(organization_id,user_id,role,review_domains) VALUES($1,$2,$3,$4)",
    )
    .bind(org)
    .bind(id)
    .bind(role)
    .bind(domains)
    .execute(pool)
    .await
    .unwrap();
    id
}
fn expectation() -> ReviewExpectation {
    ReviewExpectation {
        head: CommitSha::parse("1".repeat(40)).unwrap(),
        target: CommitSha::parse("2".repeat(40)).unwrap(),
        proposal_version: 2,
        configuration_version: 2,
        policy_version: 1,
    }
}
async fn fixture(pool: &PgPool) -> Fixture {
    let (org, repository, proposal, team) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    sqlx::query("INSERT INTO organizations(id,name) VALUES($1,'Synthetic review scope')")
        .bind(org)
        .execute(pool)
        .await
        .unwrap();
    let owner = user(pool, org, "CONTRIBUTOR", vec![]).await;
    let product = user(pool, org, "REVIEWER", vec!["PRODUCT"]).await;
    let development = user(pool, org, "REVIEWER", vec!["DEVELOPMENT"]).await;
    sqlx::query("INSERT INTO teams(id,organization_id,name) VALUES($1,$2,'Synthetic team')")
        .bind(team)
        .bind(org)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO repositories(id,organization_id,provider,remote_id,installation_id) VALUES($1,$2,'GITHUB','42','7')").bind(repository).bind(org).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO proposals(id,organization_id,repository_id,title,branch,owner_id,author_id,team_id,state,description,ui_impact,base_sha,head_sha,target_sha) VALUES($1,$2,$3,'Synthetic review scope','change/review-scope',$4,$4,$5,'DRAFT','Synthetic proposal for scope checks','UNDECIDED',$6,$6,$6)")
        .bind(proposal).bind(org).bind(repository).bind(owner).bind(team).bind("1".repeat(40)).execute(pool).await.unwrap();
    PgReviewConfiguration::new(pool.clone())
        .update(
            owner,
            proposal,
            ReviewConfigurationRequest {
                request_id: Uuid::new_v4(),
                expected_head_sha: "1".repeat(40),
                expected_proposal_version: 1,
                expected_configuration_version: 1,
                settings: ReviewSettings {
                    ui_impact: UiImpact::No,
                    ui_impact_reason: Some("No UI change".into()),
                    assignments: vec![
                        ReviewerSelection {
                            reviewer_id: product,
                            domain: ReviewArea::Product,
                            required: true,
                        },
                        ReviewerSelection {
                            reviewer_id: development,
                            domain: ReviewArea::Development,
                            required: true,
                        },
                    ],
                },
            },
        )
        .await
        .unwrap();
    Fixture {
        org,
        repository,
        proposal,
        owner,
        product,
        development,
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn scope_resolves_fresh_authoritative_metadata_and_exact_repository_without_writes(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    let auth = PgReviewAuthorization::new(pool.clone());
    let scope = auth
        .resolve(f.owner, f.proposal, &expectation())
        .await
        .unwrap();
    assert_eq!(scope.comparison.actor, f.owner);
    assert_eq!(scope.comparison.organization, f.org);
    assert_eq!(scope.comparison.connection.repository, f.repository);
    assert_eq!(scope.comparison.connection.remote_id, 42);
    assert_eq!(scope.comparison.connection.installation_id, 7);
    assert_eq!(scope.review.actor.role, Role::Contributor);
    assert!(
        scope.review.actor.active && scope.review.assigned_editor && scope.review.repository_access
    );
    assert_eq!(scope.review.author, f.owner);
    assert_eq!(
        scope.review.ui_impact,
        specspace_domain::policy::UiImpact::No
    );
    assert_eq!(
        scope.review.ui_impact_reason.as_deref(),
        Some("No UI change")
    );
    assert_eq!(scope.review.configuration_version, 2);
    assert_eq!(scope.review.assignments.len(), 2);
    assert_eq!(scope.review.reviewer_memberships.len(), 2);
    assert_eq!(
        scope.review.assignments[0].domain,
        ReviewDomain::Development
    );
    // An expected target remains a selection, not a claim that Git verified it.
    assert_eq!(scope.review.expected_target, expectation().target);
    let again = auth
        .resolve(f.owner, f.proposal, &expectation())
        .await
        .unwrap();
    assert!(scope == again);
    let (jobs, operations, audits, events): (i64,i64,i64,i64) = sqlx::query_as("SELECT (SELECT count(*) FROM jobs),(SELECT count(*) FROM git_operations),(SELECT count(*) FROM audit_events),(SELECT count(*) FROM outbox)").fetch_one(&pool).await.unwrap();
    assert_eq!((jobs, operations, audits, events), (0, 0, 1, 1));
}

#[sqlx::test(migrations = "../../migrations")]
async fn edit_authority_is_current_and_admin_does_not_bypass_organization_scope(pool: PgPool) {
    let f = fixture(&pool).await;
    let auth = PgReviewAuthorization::new(pool.clone());
    for role in ["READER", "CONTRIBUTOR", "REVIEWER", "MAINTAINER"] {
        let actor = user(&pool, f.org, role, vec!["PRODUCT"]).await;
        assert!(matches!(
            auth.resolve(actor, f.proposal, &expectation()).await,
            Err(ReviewContextError::Forbidden)
        ));
        sqlx::query("INSERT INTO proposal_collaborators(organization_id,proposal_id,user_id,access) VALUES($1,$2,$3,'EDIT')").bind(f.org).bind(f.proposal).bind(actor).execute(&pool).await.unwrap();
        if role == "READER" {
            assert!(matches!(
                auth.resolve(actor, f.proposal, &expectation()).await,
                Err(ReviewContextError::Forbidden)
            ));
        } else {
            assert!(
                auth.resolve(actor, f.proposal, &expectation())
                    .await
                    .is_ok()
            );
        }
        sqlx::query(
            "UPDATE proposal_collaborators SET access='READ' WHERE proposal_id=$1 AND user_id=$2",
        )
        .bind(f.proposal)
        .bind(actor)
        .execute(&pool)
        .await
        .unwrap();
        assert!(matches!(
            auth.resolve(actor, f.proposal, &expectation()).await,
            Err(ReviewContextError::Forbidden)
        ));
    }
    let admin = user(&pool, f.org, "ADMIN", vec![]).await;
    let scope = auth
        .resolve(admin, f.proposal, &expectation())
        .await
        .unwrap();
    assert!(!scope.review.assigned_editor);
    let other = fixture(&pool).await;
    let foreign_admin = user(&pool, other.org, "ADMIN", vec![]).await;
    assert!(matches!(
        auth.resolve(foreign_admin, f.proposal, &expectation())
            .await,
        Err(ReviewContextError::Read(ComparisonReadError::NotFound))
    ));
    sqlx::query("UPDATE users SET status='DISABLED' WHERE id=$1")
        .bind(f.owner)
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        auth.resolve(f.owner, f.proposal, &expectation()).await,
        Err(ReviewContextError::Read(ComparisonReadError::NotFound))
    ));
}

#[sqlx::test(migrations = "../../migrations")]
async fn head_and_all_versions_are_independent_compare_and_swap_gates(pool: PgPool) {
    let f = fixture(&pool).await;
    let auth = PgReviewAuthorization::new(pool.clone());
    for changed in 0..4 {
        let mut expected = expectation();
        match changed {
            0 => expected.head = expected.target.clone(),
            1 => expected.proposal_version += 1,
            2 => expected.configuration_version += 1,
            _ => expected.policy_version += 1,
        }
        assert!(matches!(
            auth.resolve(f.owner, f.proposal, &expected).await,
            Err(ReviewContextError::Stale)
        ));
    }
    let mut invalid = expectation();
    invalid.policy_version = 0;
    assert!(matches!(
        auth.resolve(f.owner, f.proposal, &invalid).await,
        Err(ReviewContextError::InvalidConfiguration)
    ));
    sqlx::query("UPDATE organizations SET policy_version=2 WHERE id=$1")
        .bind(f.org)
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        auth.resolve(f.owner, f.proposal, &expectation()).await,
        Err(ReviewContextError::Stale)
    ));
    let mut expected = expectation();
    expected.policy_version = 2;
    assert!(auth.resolve(f.owner, f.proposal, &expected).await.is_ok());
}

#[sqlx::test(migrations = "../../migrations")]
async fn reviewer_rechecks_capture_revocation_without_inventing_active_memberships(pool: PgPool) {
    let f = fixture(&pool).await;
    let auth = PgReviewAuthorization::new(pool.clone());
    let original = auth
        .resolve(f.owner, f.proposal, &expectation())
        .await
        .unwrap();
    sqlx::query("UPDATE memberships SET review_domains=ARRAY['DEVELOPMENT','PRODUCT','DEVELOPMENT'] WHERE organization_id=$1 AND user_id=$2").bind(f.org).bind(f.development).execute(&pool).await.unwrap();
    let qualified = auth
        .resolve(f.owner, f.proposal, &expectation())
        .await
        .unwrap();
    assert!(original != qualified);
    sqlx::query("UPDATE memberships SET review_domains=ARRAY['PRODUCT','DEVELOPMENT'] WHERE organization_id=$1 AND user_id=$2").bind(f.org).bind(f.development).execute(&pool).await.unwrap();
    assert!(
        qualified
            == auth
                .resolve(f.owner, f.proposal, &expectation())
                .await
                .unwrap()
    );
    sqlx::query("DELETE FROM memberships WHERE organization_id=$1 AND user_id=$2")
        .bind(f.org)
        .bind(f.product)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE users SET status='DISABLED' WHERE id=$1")
        .bind(f.development)
        .execute(&pool)
        .await
        .unwrap();
    let revoked = auth
        .resolve(f.owner, f.proposal, &expectation())
        .await
        .unwrap();
    assert!(revoked != qualified);
    assert_eq!(revoked.review.assignments.len(), 2);
    assert_eq!(revoked.review.reviewer_memberships.len(), 1);
    assert_eq!(
        revoked.review.reviewer_memberships[0].user_id,
        f.development
    );
    assert!(!revoked.review.reviewer_memberships[0].active);
    assert!(
        revoked
            .review
            .reviewer_memberships
            .iter()
            .all(|member| member.user_id != f.product)
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn unresolved_mutations_final_states_and_invalid_repository_bindings_fail_closed(
    pool: PgPool,
) {
    let f = fixture(&pool).await;
    let auth = PgReviewAuthorization::new(pool.clone());
    let operation = Uuid::new_v4();
    sqlx::query("INSERT INTO git_operations(id,organization_id,proposal_id,actor_id,kind,idempotency_key,request_hash,state) VALUES($1,$2,$3,$4,'COMMIT','scope-test','scope-test','UNKNOWN')").bind(operation).bind(f.org).bind(f.proposal).bind(f.owner).execute(&pool).await.unwrap();
    assert!(matches!(
        auth.resolve(f.owner, f.proposal, &expectation()).await,
        Err(ReviewContextError::Busy)
    ));
    sqlx::query("UPDATE git_operations SET state='FAILED' WHERE id=$1")
        .bind(operation)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        auth.resolve(f.owner, f.proposal, &expectation())
            .await
            .is_ok()
    );
    for state in ["MERGING", "MERGED", "CLOSED"] {
        sqlx::query("UPDATE proposals SET state=$2 WHERE id=$1")
            .bind(f.proposal)
            .bind(state)
            .execute(&pool)
            .await
            .unwrap();
        assert!(matches!(
            auth.resolve(f.owner, f.proposal, &expectation()).await,
            Err(ReviewContextError::StateChanged)
        ));
    }
    sqlx::query("UPDATE proposals SET state='DRAFT' WHERE id=$1")
        .bind(f.proposal)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE repositories SET remote_id='invalid' WHERE id=$1")
        .bind(f.repository)
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        auth.resolve(f.owner, f.proposal, &expectation()).await,
        Err(ReviewContextError::Read(
            ComparisonReadError::InvalidConfiguration
        ))
    ));
    sqlx::query("UPDATE repositories SET remote_id='42',default_branch='another' WHERE id=$1")
        .bind(f.repository)
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        auth.resolve(f.owner, f.proposal, &expectation()).await,
        Err(ReviewContextError::Read(
            ComparisonReadError::InvalidConfiguration
        ))
    ));
}

#[sqlx::test(migrations = "../../migrations")]
async fn admin_cannot_submit_a_proposal_whose_owner_has_lost_eligibility(pool: PgPool) {
    let f = fixture(&pool).await;
    let admin = user(&pool, f.org, "ADMIN", vec![]).await;
    let auth = PgReviewAuthorization::new(pool.clone());
    sqlx::query("UPDATE users SET status='DISABLED' WHERE id=$1")
        .bind(f.owner)
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        auth.resolve(admin, f.proposal, &expectation()).await,
        Err(ReviewContextError::OwnerNotEligible)
    ));
    sqlx::query("UPDATE users SET status='ACTIVE' WHERE id=$1")
        .bind(f.owner)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE memberships SET role='READER' WHERE organization_id=$1 AND user_id=$2")
        .bind(f.org)
        .bind(f.owner)
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        auth.resolve(admin, f.proposal, &expectation()).await,
        Err(ReviewContextError::OwnerNotEligible)
    ));
    sqlx::query(
        "UPDATE memberships SET role='CONTRIBUTOR' WHERE organization_id=$1 AND user_id=$2",
    )
    .bind(f.org)
    .bind(f.owner)
    .execute(&pool)
    .await
    .unwrap();
    assert!(
        auth.resolve(admin, f.proposal, &expectation())
            .await
            .is_ok()
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_waiting_scope_reads_the_committed_revision_and_old_expectations_are_stale(pool: PgPool) {
    let f = fixture(&pool).await;
    let mut writer = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM proposals WHERE id=$1 FOR UPDATE")
        .bind(f.proposal)
        .execute(&mut *writer)
        .await
        .unwrap();
    sqlx::query("INSERT INTO proposal_review_configurations(organization_id,proposal_id,version,ui_impact,ui_impact_reason,assignments,created_by) SELECT organization_id,proposal_id,3,ui_impact,'Updated reason',assignments,created_by FROM proposal_review_configurations WHERE proposal_id=$1 AND version=2").bind(f.proposal).execute(&mut *writer).await.unwrap();
    sqlx::query("INSERT INTO proposal_review_assignments(organization_id,proposal_id,configuration_version,reviewer_id,domain,required) SELECT organization_id,proposal_id,3,reviewer_id,domain,required FROM proposal_review_assignments WHERE proposal_id=$1 AND configuration_version=2").bind(f.proposal).execute(&mut *writer).await.unwrap();
    sqlx::query("UPDATE proposals SET version=3,review_configuration_version=3,ui_impact_reason='Updated reason' WHERE id=$1").bind(f.proposal).execute(&mut *writer).await.unwrap();
    let actor = f.owner;
    let proposal = f.proposal;
    let fresh_pool = pool.clone();
    let fresh = tokio::spawn(async move {
        let mut expected = expectation();
        expected.proposal_version = 3;
        expected.configuration_version = 3;
        PgReviewAuthorization::new(fresh_pool)
            .resolve(actor, proposal, &expected)
            .await
    });
    let stale_pool = pool.clone();
    let stale = tokio::spawn(async move {
        PgReviewAuthorization::new(stale_pool)
            .resolve(actor, proposal, &expectation())
            .await
    });
    // Observe real PostgreSQL lock waits, rather than assuming spawned reads
    // reached the lock before allowing the writer to commit.
    let wait = async {
        loop {
            let blocked:i64=sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock'").fetch_one(&pool).await.unwrap();
            if blocked >= 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(3), wait)
        .await
        .unwrap();
    writer.commit().await.unwrap();
    let scope = fresh.await.unwrap().unwrap();
    assert_eq!(scope.comparison.version, 3);
    assert_eq!(scope.review.configuration_version, 3);
    assert_eq!(
        scope.review.ui_impact_reason.as_deref(),
        Some("Updated reason")
    );
    assert_eq!(scope.review.assignments.len(), 2);
    assert!(matches!(
        stale.await.unwrap(),
        Err(ReviewContextError::Stale)
    ));
}
