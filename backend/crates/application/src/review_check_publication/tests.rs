use super::*;
use crate::review_readiness::{ReadinessEvaluation, ReadinessIssue};

fn source() -> CheckPublicationSource {
    CheckPublicationSource {
        organization: Uuid::from_u128(1),
        actor: Uuid::from_u128(2),
        proposal: Uuid::from_u128(3),
        operation: Uuid::from_u128(4),
        snapshot: Uuid::from_u128(5),
        repository: RepositoryBinding {
            repository: Uuid::from_u128(6),
            remote_id: 42,
            installation_id: 8,
        },
        app_id: 7,
        branch: BranchName::parse("change/check-policy").unwrap(),
        head: CommitSha::parse("a".repeat(40)).unwrap(),
        target: CommitSha::parse("b".repeat(40)).unwrap(),
        merge_base: CommitSha::parse("b".repeat(40)).unwrap(),
        proposal_version: 2,
        configuration_version: 3,
        policy_version: 1,
        fingerprint: "c".repeat(64),
        evidence_digest: "d".repeat(64),
        authority_digest: "e".repeat(64),
    }
}
fn report() -> ReviewReadiness {
    let s = source();
    ReviewReadiness {
        actor_id: s.actor,
        proposal_id: s.proposal,
        snapshot_id: s.snapshot,
        fingerprint: s.fingerprint,
        head_sha: s.head.as_str().into(),
        target_sha: s.target.as_str().into(),
        policy_version: s.policy_version,
        configuration_version: s.configuration_version,
        current_for_metadata: true,
        provider_refs_verified: true,
        read_at: "2026-11-26T10:00:00Z".into(),
        evaluation: ReadinessEvaluation {
            conclusion: ReadinessConclusion::Success,
            issues: vec![],
        },
    }
}
fn intent() -> CheckPublicationIntent {
    CheckPublicationIntent::from_readiness(source(), &report()).unwrap()
}
fn run(id: u64, operation: Uuid, state: CheckState) -> CheckRun {
    CheckRun {
        id: CheckRunId::new(id).unwrap(),
        scope: intent().scope(),
        operation,
        state,
    }
}
fn own(state: CheckState) -> CheckRun {
    run(81, source().operation, state)
}
fn issue(conclusion: ReadinessConclusion) -> ReadinessIssue {
    ReadinessIssue {
        gate: ReadinessGate::BlockingQuestions,
        conclusion,
        code: "POLICY_EVIDENCE_PENDING".into(),
    }
}

#[test]
fn only_consistent_fresh_reports_can_supply_a_publication_result() {
    assert_eq!(intent().result(), CheckResult::Success);
    for (conclusion, expected) in [
        (ReadinessConclusion::Pending, CheckResult::Pending),
        (ReadinessConclusion::Failure, CheckResult::Failure),
    ] {
        let mut r = report();
        r.evaluation
            .issues
            .push(issue(if conclusion == ReadinessConclusion::Pending {
                ReadinessConclusion::Pending
            } else {
                ReadinessConclusion::Failure
            }));
        r.evaluation.conclusion = conclusion;
        assert_eq!(
            CheckPublicationIntent::from_readiness(source(), &r)
                .unwrap()
                .result(),
            expected
        );
    }
    let mut r = report();
    r.current_for_metadata = false;
    r.evaluation.conclusion = ReadinessConclusion::Failure;
    r.evaluation.issues.push(ReadinessIssue {
        gate: ReadinessGate::Snapshot,
        conclusion: ReadinessConclusion::Failure,
        code: "SNAPSHOT_NOT_CURRENT".into(),
    });
    assert_eq!(
        CheckPublicationIntent::from_readiness(source(), &r)
            .unwrap()
            .result(),
        CheckResult::Failure
    );
}

#[test]
fn unverified_foreign_inconsistent_or_ambiguous_reports_fail_closed() {
    for mode in 0..19 {
        let mut r = report();
        match mode {
            0 => r.actor_id = Uuid::new_v4(),
            1 => r.proposal_id = Uuid::new_v4(),
            2 => r.snapshot_id = Uuid::new_v4(),
            3 => r.fingerprint = "f".repeat(64),
            4 => r.head_sha = "f".repeat(40),
            5 => r.target_sha = "f".repeat(40),
            6 => r.policy_version += 1,
            7 => r.configuration_version += 1,
            8 => r.provider_refs_verified = false,
            9 => r.current_for_metadata = false,
            10 => r.evaluation.conclusion = ReadinessConclusion::Pending,
            11 => r.evaluation.conclusion = ReadinessConclusion::Failure,
            12 => r
                .evaluation
                .issues
                .push(issue(ReadinessConclusion::Success)),
            13 => {
                r.evaluation.conclusion = ReadinessConclusion::Pending;
                r.evaluation
                    .issues
                    .push(issue(ReadinessConclusion::Pending));
                r.evaluation
                    .issues
                    .push(issue(ReadinessConclusion::Pending));
            }
            14 => {
                r.evaluation.conclusion = ReadinessConclusion::Pending;
                let mut i = issue(ReadinessConclusion::Pending);
                i.code = "PROVIDER_REFS_UNVERIFIED".into();
                r.evaluation.issues.push(i);
            }
            15 => {
                r.evaluation.conclusion = ReadinessConclusion::Pending;
                let mut i = issue(ReadinessConclusion::Pending);
                i.code = "private text <script>".into();
                r.evaluation.issues.push(i);
            }
            16 => {
                r.evaluation.conclusion = ReadinessConclusion::Pending;
                r.evaluation.issues = (0..21)
                    .map(|n| {
                        let mut i = issue(ReadinessConclusion::Pending);
                        i.code = format!("PENDING_{n}");
                        i
                    })
                    .collect()
            }
            17 => r
                .evaluation
                .issues
                .push(issue(ReadinessConclusion::Pending)),
            _ => {
                r.evaluation.conclusion = ReadinessConclusion::Pending;
                let mut i = issue(ReadinessConclusion::Pending);
                i.code = "".into();
                r.evaluation.issues.push(i);
            }
        }
        assert!(
            CheckPublicationIntent::from_readiness(source(), &r).is_err(),
            "mode {mode}"
        );
    }
}

#[test]
fn intent_hash_binds_every_scope_version_proof_and_operation_field() {
    let original = intent();
    let restored = CheckPublicationIntent::restore(
        source(),
        original.purpose().clone(),
        original.result(),
        original.request_hash(),
    )
    .unwrap();
    assert_eq!(restored.request_hash(), original.request_hash());
    for mode in 0..20 {
        let mut s = source();
        match mode {
            0 => s.organization = Uuid::new_v4(),
            1 => s.actor = Uuid::new_v4(),
            2 => s.proposal = Uuid::new_v4(),
            3 => s.operation = Uuid::new_v4(),
            4 => s.snapshot = Uuid::new_v4(),
            5 => s.repository.repository = Uuid::new_v4(),
            6 => s.repository.remote_id += 1,
            7 => s.repository.installation_id += 1,
            8 => s.app_id += 1,
            9 => s.branch = BranchName::parse("change/other").unwrap(),
            10 => s.head = CommitSha::parse("f".repeat(40)).unwrap(),
            11 => s.target = CommitSha::parse("f".repeat(40)).unwrap(),
            12 => s.merge_base = CommitSha::parse("f".repeat(40)).unwrap(),
            13 => s.proposal_version += 1,
            14 => s.configuration_version += 1,
            15 => s.policy_version += 1,
            16 => s.fingerprint = "f".repeat(64),
            17 => s.evidence_digest = "f".repeat(64),
            18 => s.authority_digest = "f".repeat(64),
            _ => s.actor = Uuid::nil(),
        }
        assert!(
            CheckPublicationIntent::restore(
                s,
                original.purpose().clone(),
                original.result(),
                original.request_hash()
            )
            .is_err(),
            "mode {mode}"
        );
    }
    assert!(
        CheckPublicationIntent::restore(
            source(),
            original.purpose().clone(),
            CheckResult::Failure,
            original.request_hash()
        )
        .is_err()
    );
    let mut changed = report();
    changed.read_at = "2026-11-26T10:00:01Z".into();
    assert_ne!(
        CheckPublicationIntent::from_readiness(source(), &changed)
            .unwrap()
            .request_hash(),
        original.request_hash()
    );
}

#[test]
fn malformed_sources_and_restored_report_digests_are_rejected() {
    let i = intent();
    for mode in 0..9 {
        let mut s = source();
        match mode {
            0 => s.repository.installation_id = 0,
            1 => s.app_id = 0,
            2 => s.repository.remote_id = 0,
            3 => s.proposal_version = 0,
            4 => s.configuration_version = -1,
            5 => s.policy_version = 0,
            6 => s.fingerprint = "x".repeat(64),
            7 => s.authority_digest = "a".repeat(63),
            _ => s.evidence_digest = "A".repeat(64),
        }
        assert!(CheckPublicationIntent::from_readiness(s, &report()).is_err());
    }
    assert!(
        CheckPublicationIntent::restore(
            source(),
            CheckPurpose::Evaluation {
                report_digest: "forged".into()
            },
            i.result(),
            i.request_hash()
        )
        .is_err()
    );
}

#[test]
fn absence_never_restarts_a_write_once_its_journal_record_exists() {
    use CheckPublicationDecision as D;
    use CheckPublicationPhase as P;
    let i = intent();
    let id = CheckRunId::new(81).unwrap();
    assert_eq!(i.decide(&[], true, P::NotStarted), D::CreatePending);
    for phase in [
        P::NotStarted,
        P::CreateStarted,
        P::Created(id),
        P::UpdateStarted(id),
    ] {
        assert_eq!(i.decide(&[], false, phase), D::ObserveAgain);
    }
    for phase in [P::CreateStarted, P::Created(id), P::UpdateStarted(id)] {
        for _ in 0..10 {
            assert_eq!(i.decide(&[], true, phase), D::ObserveAgain);
        }
    }
    assert_eq!(
        i.decide(
            &[run(82, Uuid::new_v4(), CheckState::Success)],
            true,
            P::CreateStarted
        ),
        D::ObserveAgain
    );
}

#[test]
fn pending_creation_and_completion_require_their_exact_recorded_phases() {
    use CheckPublicationDecision as D;
    use CheckPublicationPhase as P;
    let i = intent();
    let id = CheckRunId::new(81).unwrap();
    assert_eq!(
        i.decide(&[own(CheckState::Pending)], true, P::NotStarted),
        D::Conflict
    );
    assert_eq!(
        i.decide(&[own(CheckState::Pending)], true, P::CreateStarted),
        D::RecordPending(id)
    );
    assert_eq!(
        i.decide(&[own(CheckState::Pending)], true, P::Created(id)),
        D::Update(id, CheckResult::Success)
    );
    assert_eq!(
        i.decide(&[own(CheckState::Success)], true, P::Created(id)),
        D::Conflict
    );
    assert_eq!(
        i.decide(&[own(CheckState::Success)], true, P::CreateStarted),
        D::Conflict
    );
    assert_eq!(
        i.decide(&[own(CheckState::Pending)], true, P::UpdateStarted(id)),
        D::ObserveAgain
    );
    assert_eq!(
        i.decide(&[own(CheckState::Other)], true, P::UpdateStarted(id)),
        D::ObserveAgain
    );
    assert_eq!(
        i.decide(&[own(CheckState::Success)], true, P::UpdateStarted(id)),
        D::Published(id)
    );
}

#[test]
fn duplicate_or_foreign_provider_identity_is_never_adopted() {
    use CheckPublicationDecision as D;
    use CheckPublicationPhase as P;
    let i = intent();
    let id = CheckRunId::new(81).unwrap();
    for mode in 0..8 {
        let mut r = own(CheckState::Pending);
        match mode {
            0 => r.scope = CheckScope::new(43, 7, source().head).unwrap(),
            1 => r.scope = CheckScope::new(42, 8, source().head).unwrap(),
            2 => r.scope = CheckScope::new(42, 7, source().target).unwrap(),
            3 => r.operation = Uuid::nil(),
            4 => r.id = CheckRunId::new(82).unwrap(),
            5 => {
                assert_eq!(
                    i.decide(&[r, own(CheckState::Pending)], true, P::CreateStarted),
                    D::Conflict
                );
                continue;
            }
            6 => {
                assert_eq!(
                    i.decide(
                        &[r, run(82, source().operation, CheckState::Pending)],
                        true,
                        P::CreateStarted
                    ),
                    D::Conflict
                );
                continue;
            }
            _ => {
                assert_eq!(
                    i.decide(
                        &[r, run(81, Uuid::new_v4(), CheckState::Success)],
                        true,
                        P::CreateStarted
                    ),
                    D::Conflict
                );
                continue;
            }
        }
        assert_eq!(
            i.decide(&[r], true, P::Created(id)),
            D::Conflict,
            "mode {mode}"
        );
    }
    assert_eq!(
        i.decide(
            &[
                run(82, Uuid::new_v4(), CheckState::Success),
                own(CheckState::Pending)
            ],
            true,
            P::CreateStarted
        ),
        D::RecordPending(id)
    );
}

#[test]
fn invalidation_targets_the_original_run_and_operation_but_never_creates_or_grants_success() {
    use CheckPublicationDecision as D;
    use CheckPublicationPhase as P;
    let target = Uuid::from_u128(9);
    let id = CheckRunId::new(81).unwrap();
    let i =
        CheckPublicationIntent::invalidate(source(), CheckInvalidation::TargetChanged, id, target)
            .unwrap();
    assert_eq!(i.result(), CheckResult::Failure);
    assert_eq!(i.check_operation(), target);
    assert_eq!(i.decide(&[], true, P::NotStarted), D::ObserveAgain);
    assert_eq!(
        i.decide(&[own(CheckState::Success)], true, P::NotStarted),
        D::ObserveAgain
    );
    assert_eq!(
        i.decide(&[run(81, target, CheckState::Success)], true, P::NotStarted),
        D::Update(id, CheckResult::Failure)
    );
    assert_eq!(
        i.decide(
            &[run(81, target, CheckState::Success)],
            true,
            P::UpdateStarted(id)
        ),
        D::ObserveAgain
    );
    assert_eq!(
        i.decide(
            &[run(81, target, CheckState::Failure)],
            true,
            P::UpdateStarted(id)
        ),
        D::Published(id)
    );
    assert_eq!(
        i.decide(&[run(82, target, CheckState::Success)], true, P::NotStarted),
        D::Conflict
    );
    assert_eq!(
        i.decide(
            &[run(81, target, CheckState::Pending)],
            true,
            P::CreateStarted
        ),
        D::Conflict
    );
    assert!(
        CheckPublicationIntent::restore(
            source(),
            i.purpose().clone(),
            CheckResult::Success,
            i.request_hash()
        )
        .is_err()
    );
    assert!(
        CheckPublicationIntent::invalidate(
            source(),
            CheckInvalidation::AuthorityChanged,
            id,
            source().operation
        )
        .is_err()
    );
    assert!(
        CheckPublicationIntent::invalidate(
            source(),
            CheckInvalidation::AuthorityChanged,
            id,
            Uuid::nil()
        )
        .is_err()
    );
    assert!(
        CheckPublicationIntent::restore(
            source(),
            i.purpose().clone(),
            CheckResult::Failure,
            i.request_hash()
        )
        .is_ok()
    );
    for purpose in [
        CheckPurpose::Invalidation {
            reason: CheckInvalidation::PolicyChanged,
            run: id,
            check_operation: target,
        },
        CheckPurpose::Invalidation {
            reason: CheckInvalidation::TargetChanged,
            run: CheckRunId::new(82).unwrap(),
            check_operation: target,
        },
        CheckPurpose::Invalidation {
            reason: CheckInvalidation::TargetChanged,
            run: id,
            check_operation: Uuid::new_v4(),
        },
    ] {
        assert!(
            CheckPublicationIntent::restore(
                source(),
                purpose,
                CheckResult::Failure,
                i.request_hash()
            )
            .is_err()
        );
    }
}

#[test]
fn discovery_cannot_exceed_the_provider_page_budget() {
    let i = intent();
    let scope = i.scope();
    let rows: Vec<_> = (1..=10001)
        .map(|n| CheckRun {
            id: CheckRunId::new(n).unwrap(),
            scope: scope.clone(),
            operation: Uuid::from_u128(n as u128 + 100),
            state: CheckState::Success,
        })
        .collect();
    assert_eq!(
        i.decide(&rows, true, CheckPublicationPhase::NotStarted),
        CheckPublicationDecision::Conflict
    );
}
