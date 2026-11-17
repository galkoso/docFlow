use super::*;
use crate::{
    review_context::{ReviewAuthorization, ReviewContextError, ReviewExpectation, ReviewScope},
    review_preparation::{ReviewPreparationError, ReviewPreparer},
};
mod media;
type RecheckHook = Mutex<Option<(usize, Box<dyn FnOnce() + Send>)>>;
struct ReviewAuth {
    scope: Arc<Mutex<ReviewScope>>,
    calls: AtomicUsize,
    hook: RecheckHook,
    blocked: AtomicBool,
    started: Notify,
}
#[async_trait]
impl ReviewAuthorization for ReviewAuth {
    async fn resolve(
        &self,
        actor: Uuid,
        proposal: Uuid,
        _: &ReviewExpectation,
    ) -> Result<ReviewScope, ReviewContextError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let hook = {
            let mut hook = self.hook.lock().unwrap();
            if hook.as_ref().is_some_and(|(at, _)| *at == call) {
                hook.take().map(|(_, hook)| hook)
            } else {
                None
            }
        };
        if let Some(hook) = hook {
            hook();
        }
        let scope = self.scope.lock().unwrap().clone();
        if scope.comparison.actor != actor || scope.comparison.proposal != proposal {
            return Err(ReviewContextError::Forbidden);
        }
        self.started.notify_one();
        if self.blocked.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        Ok(scope)
    }
}
fn prepared_fixture(f: &Fixture) -> (Arc<ReviewAuth>, Arc<ReviewPreparer>, ReviewExpectation) {
    let review = context(f);
    let expectation = ReviewExpectation {
        head: review.expected_head.clone(),
        target: review.expected_target.clone(),
        proposal_version: review.expected_version,
        configuration_version: review.configuration_version,
        policy_version: review.policy_version,
    };
    let auth = Arc::new(ReviewAuth {
        scope: Arc::new(Mutex::new(ReviewScope {
            comparison: f.auth.context.lock().unwrap().clone(),
            review,
        })),
        calls: AtomicUsize::new(0),
        hook: Mutex::new(None),
        blocked: AtomicBool::new(false),
        started: Notify::new(),
    });
    let preparer = Arc::new(
        ReviewPreparer::new(
            auth.clone(),
            f.reader.clone(),
            f.factory.clone(),
            Duration::from_secs(900),
        )
        .unwrap(),
    );
    (auth, preparer, expectation)
}

#[test]
fn authority_digest_is_canonical_but_detects_actor_metadata_and_qualification_changes() {
    let f = fixture();
    let (auth, _, _) = prepared_fixture(&f);
    let original = auth.scope.lock().unwrap().clone();
    let hash = original.authority_digest();
    let mut ordered = original.clone();
    ordered.review.assignments.reverse();
    ordered.review.reviewer_memberships.reverse();
    assert_eq!(hash, ordered.authority_digest());
    for field in 0..6 {
        let mut changed = original.clone();
        match field {
            0 => changed.review.actor.role = Role::Admin,
            1 => changed.review.description.push_str(" updated"),
            2 => changed.review.reviewer_memberships[0].active = false,
            3 => changed.comparison.connection.installation_id += 1,
            4 => changed.review.configuration_version += 1,
            _ => changed.comparison.state = "IN_REVIEW".into(),
        }
        assert_ne!(hash, changed.authority_digest());
    }
}

#[tokio::test]
async fn orchestration_prepares_only_verified_saved_content_and_rechecks_without_writing() {
    let f = fixture();
    let (auth, preparer, expectation) = prepared_fixture(&f);
    let candidate = preparer
        .prepare(f.actor, f.proposal, expectation)
        .await
        .unwrap();
    assert_eq!(candidate.plan().summary().head_sha, HEAD);
    assert_eq!(candidate.plan().summary().target_sha, MAIN);
    assert_eq!(candidate.plan().summary().changed_files, 1);
    assert_eq!(
        candidate.comparison().id(),
        candidate.plan().summary().comparison_content_id
    );
    assert!(candidate.plan().validation().ready());
    assert_eq!(candidate.scope().review.configuration_version, 1);
    assert_eq!(auth.calls.load(Ordering::SeqCst), 5);
    assert_eq!(f.factory.calls.load(Ordering::SeqCst), 2);
    let reads = f.provider.text_reads.lock().unwrap().len();
    preparer.recheck(&candidate).await.unwrap();
    assert_eq!(f.provider.text_reads.lock().unwrap().len(), reads);
    assert_eq!(f.factory.calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn malformed_authority_never_connects_to_a_provider() {
    for changed in 0..6 {
        let f = fixture();
        let (auth, preparer, expectation) = prepared_fixture(&f);
        {
            let mut scope = auth.scope.lock().unwrap();
            match changed {
                0 => scope.review.actor.role = Role::Reader,
                1 => scope.review.assigned_editor = false,
                2 => scope.review.repository = Uuid::new_v4(),
                3 => scope.review.configuration_version += 1,
                4 => scope.review.expected_target = sha(OLD),
                _ => scope.comparison.state = "CLOSED".into(),
            }
        }
        assert!(matches!(
            preparer.prepare(f.actor, f.proposal, expectation).await,
            Err(ReviewPreparationError::ContextChanged)
        ));
        assert_eq!(f.factory.calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn metadata_revocation_after_load_cpu_connection_and_ref_reads_discards_the_candidate() {
    for boundary in 2..=5 {
        let f = fixture();
        let (auth, preparer, expectation) = prepared_fixture(&f);
        let scope = auth.scope.clone();
        *auth.hook.lock().unwrap() = Some((
            boundary,
            Box::new(move || {
                scope.lock().unwrap().review.reviewer_memberships[0].active = false;
            }),
        ));
        assert!(
            matches!(
                preparer.prepare(f.actor, f.proposal, expectation).await,
                Err(ReviewPreparationError::ContextChanged)
            ),
            "boundary {boundary}"
        );
    }
}

#[tokio::test]
async fn revocation_after_cpu_takes_precedence_over_private_validation_diagnostics() {
    let f = fixture_body("[Missing](#SEC-NOT-THERE)");
    let (auth, preparer, expectation) = prepared_fixture(&f);
    let scope = auth.scope.clone();
    *auth.hook.lock().unwrap() = Some((
        3,
        Box::new(move || {
            scope.lock().unwrap().review.assigned_editor = false;
        }),
    ));
    assert!(matches!(
        preparer.prepare(f.actor, f.proposal, expectation).await,
        Err(ReviewPreparationError::ContextChanged)
    ));
    assert_eq!(f.factory.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn head_and_main_advancement_after_comparison_load_cannot_return_a_current_candidate() {
    for target in [false, true] {
        let f = fixture();
        let (auth, preparer, expectation) = prepared_fixture(&f);
        let provider = f.provider.clone();
        *auth.hook.lock().unwrap() = Some((
            3,
            Box::new(move || {
                if target {
                    *provider.target.lock().unwrap() = NEXT.into();
                } else {
                    *provider.branch.lock().unwrap() = Some(LATER.into());
                }
            }),
        ));
        let error = preparer
            .prepare(f.actor, f.proposal, expectation)
            .await
            .err()
            .unwrap();
        assert!(if target {
            matches!(error, ReviewPreparationError::TargetChanged)
        } else {
            matches!(error, ReviewPreparationError::HeadChanged)
        });
    }
}

#[tokio::test]
async fn a_later_recheck_rejects_stale_refs_and_reviewer_qualification_without_reading_source_again()
 {
    let f = fixture();
    let (auth, preparer, expectation) = prepared_fixture(&f);
    let candidate = preparer
        .prepare(f.actor, f.proposal, expectation)
        .await
        .unwrap();
    let reads = f.provider.text_reads.lock().unwrap().len();
    *f.provider.target.lock().unwrap() = NEXT.into();
    assert!(matches!(
        preparer.recheck(&candidate).await,
        Err(ReviewPreparationError::TargetChanged)
    ));
    *f.provider.target.lock().unwrap() = MAIN.into();
    auth.scope.lock().unwrap().review.reviewer_memberships[0]
        .review_domains
        .clear();
    assert!(matches!(
        preparer.recheck(&candidate).await,
        Err(ReviewPreparationError::ContextChanged)
    ));
    assert_eq!(f.provider.text_reads.lock().unwrap().len(), reads);
}

#[tokio::test]
async fn bounded_capacity_cancellation_and_retry_never_repeat_git_mutations() {
    let f = fixture();
    let (auth, preparer, expectation) = prepared_fixture(&f);
    auth.blocked.store(true, Ordering::SeqCst);
    let actor = f.actor;
    let proposal = f.proposal;
    let first_preparer = preparer.clone();
    let first_expected = expectation.clone();
    let first = tokio::spawn(async move {
        first_preparer
            .prepare(actor, proposal, first_expected)
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), auth.started.notified())
        .await
        .unwrap();
    let second_preparer = preparer.clone();
    let second_expected = expectation.clone();
    let second = tokio::spawn(async move {
        second_preparer
            .prepare(actor, proposal, second_expected)
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), auth.started.notified())
        .await
        .unwrap();
    assert!(matches!(
        preparer.prepare(actor, proposal, expectation.clone()).await,
        Err(ReviewPreparationError::Busy)
    ));
    let scoped = preparer.with_authorization(auth.clone());
    assert!(matches!(
        tokio::time::timeout(
            Duration::from_secs(2),
            scoped.prepare(actor, proposal, expectation.clone())
        )
        .await
        .unwrap(),
        Err(ReviewPreparationError::Busy)
    ));
    first.abort();
    second.abort();
    assert!(matches!(first.await,Err(error) if error.is_cancelled()));
    assert!(matches!(second.await,Err(error) if error.is_cancelled()));
    auth.blocked.store(false, Ordering::SeqCst);
    assert!(preparer.prepare(actor, proposal, expectation).await.is_ok());
}

#[tokio::test(start_paused = true)]
async fn an_expired_preparation_releases_capacity_and_does_not_retry_itself() {
    let f = fixture();
    let (auth, _, expectation) = prepared_fixture(&f);
    let preparer = Arc::new(
        ReviewPreparer::new(
            auth.clone(),
            f.reader.clone(),
            f.factory.clone(),
            Duration::from_secs(30),
        )
        .unwrap(),
    );
    auth.blocked.store(true, Ordering::SeqCst);
    let worker = preparer.clone();
    let expected = expectation.clone();
    let actor = f.actor;
    let proposal = f.proposal;
    let task = tokio::spawn(async move { worker.prepare(actor, proposal, expected).await });
    auth.started.notified().await;
    tokio::time::advance(Duration::from_secs(31)).await;
    assert!(matches!(
        task.await.unwrap(),
        Err(ReviewPreparationError::TimedOut)
    ));
    assert_eq!(auth.calls.load(Ordering::SeqCst), 1);
    assert_eq!(f.factory.calls.load(Ordering::SeqCst), 0);
    tokio::time::resume();
    auth.blocked.store(false, Ordering::SeqCst);
    assert!(preparer.prepare(actor, proposal, expectation).await.is_ok());
}
