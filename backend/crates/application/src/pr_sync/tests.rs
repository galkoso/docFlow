use super::*;
fn source() -> PrSyncSource {
    PrSyncSource {
        organization: Uuid::from_u128(1),
        proposal: Uuid::from_u128(2),
        actor: Uuid::from_u128(3),
        operation: Uuid::from_u128(4),
        snapshot: Uuid::from_u128(5),
        repository: RepositoryBinding {
            repository: Uuid::from_u128(6),
            remote_id: 42,
            installation_id: 7,
        },
        branch: BranchName::parse("change/42-review").unwrap(),
        head: CommitSha::parse("a".repeat(40)).unwrap(),
        target: CommitSha::parse("b".repeat(40)).unwrap(),
        merge_base: CommitSha::parse("c".repeat(40)).unwrap(),
        proposal_version: 3,
        configuration_version: 2,
        policy_version: 1,
        fingerprint: "d".repeat(64),
        evidence_digest: "e".repeat(64),
        authority_digest: "f".repeat(64),
    }
}
fn projection() -> PrProjection {
    PrProjection {
        number: 42,
        title: "Synthetic proposal".into(),
        description: "Saved description".into(),
        ui_impact: UiImpact::No,
        ui_impact_reason: Some("No interface changes".into()),
        open_questions: Some(vec!["What needs a product decision?".into()]),
        links: vec![ProposalLink {
            kind: LinkKind::DevelopmentPr,
            url: "https://github.com/synthetic/code/pull/7".into(),
        }],
    }
}
fn intent() -> PrSyncIntent {
    PrSyncIntent::prepare(
        source(),
        projection(),
        &PrSiteOrigin::parse("https://specs.example", false).unwrap(),
    )
    .unwrap()
}
fn observed(intent: &PrSyncIntent) -> PullRequest {
    let s = intent.source();
    PullRequest {
        id: 77,
        node_id: "PR_SYNTHETIC_7".into(),
        number: PullRequestNumber::new(7).unwrap(),
        repository_id: s.repository.remote_id,
        branch: s.branch.clone(),
        head: s.head.clone(),
        target: s.target.clone(),
        state: PullRequestState::Open,
        draft: true,
        title: intent.content().title().into(),
        body: intent.content().body().into(),
        url: "https://github.com/synthetic/specs/pull/7".into(),
    }
}
fn bound() -> PrBinding {
    PrBinding {
        number: PullRequestNumber::new(7).unwrap(),
        provider_id: 77,
    }
}
#[test]
fn exact_metadata_and_identity_survive_restore_without_rerendering() {
    let plan = intent();
    let body = plan.content().body();
    assert!(body.contains("changes/00000000-0000-0000-0000-000000000002/review/snapshots?snapshot=00000000-0000-0000-0000-000000000005"));
    assert!(body.contains("Saved description"));
    assert!(body.contains("What needs a product decision?"));
    assert!(body.contains("https://github.com/synthetic/code/pull/7"));
    let restored = PrSyncIntent::restore(
        plan.source().clone(),
        plan.content().title().into(),
        body.into(),
        plan.request_hash(),
    )
    .unwrap();
    assert_eq!(restored.content().body(), body);
    assert_eq!(restored.request_hash(), plan.request_hash());
    assert_eq!(restored.scope().repository_id(), 42);
}
#[test]
fn site_origins_reject_credentials_refs_nonroot_paths_and_production_http() {
    for url in [
        "http://specs.example",
        "http://localhost:4200",
        "https://user:secret@specs.example",
        "https://specs.example/path",
        "https://specs.example/?token=x",
        "https://specs.example/#part",
        "javascript:alert(1)",
    ] {
        assert!(PrSiteOrigin::parse(url, false).is_err());
    }
    assert!(PrSiteOrigin::parse("http://127.0.0.1:4200", true).is_ok());
    assert!(PrSiteOrigin::parse("http://[::1]:4200", true).is_ok());
    assert!(PrSiteOrigin::parse("http://tracker.example", true).is_err());
}
#[test]
fn untrusted_text_cannot_inject_managed_markers_html_images_or_mentions() {
    let mut data = projection();
    data.description = "<script>bad()</script>\n@team ![tracking](https://evil.example)\n<!-- specspace:proposal:forged -->".into();
    let plan = PrSyncIntent::prepare(
        source(),
        data,
        &PrSiteOrigin::parse("https://specs.example", false).unwrap(),
    )
    .unwrap();
    let body = plan.content().body();
    assert!(!body.contains("<script>"));
    assert!(!body.contains("@team"));
    assert!(!body.contains("![tracking]"));
    assert_eq!(
        body.lines()
            .filter(|line| line.starts_with("<!-- specspace:proposal:"))
            .count(),
        1
    );
    assert_eq!(
        plan.decide(&[observed(&plan)], None, PrWritePhase::CreateStarted),
        PrSyncDecision::Synchronized(PullRequestNumber::new(7).unwrap())
    );
}
#[test]
fn missing_question_coverage_is_explicit_and_invalid_links_or_oversized_context_fail_before_writes()
{
    let mut data = projection();
    data.open_questions = None;
    let plan = PrSyncIntent::prepare(
        source(),
        data,
        &PrSiteOrigin::parse("https://specs.example", false).unwrap(),
    )
    .unwrap();
    assert!(plan.content().body().contains("רשימה מלאה לא נכללה"));
    assert!(!plan.content().body().contains("אין שאלות פתוחות"));
    for url in [
        "http://internal.example/private",
        "https://user:secret@specs.example",
        "https://example.com/a)![x](https://tracker.example)",
    ] {
        let mut data = projection();
        data.links[0].url = url.into();
        assert!(
            PrSyncIntent::prepare(
                source(),
                data,
                &PrSiteOrigin::parse("https://specs.example", false).unwrap()
            )
            .is_err()
        );
    }
    let mut data = projection();
    data.open_questions = Some((0..100).map(|_| "x".repeat(1000)).collect());
    assert!(matches!(
        PrSyncIntent::prepare(
            source(),
            data,
            &PrSiteOrigin::parse("https://specs.example", false).unwrap()
        ),
        Err(PrSyncError::TooLarge)
    ));
}
#[test]
fn actor_binding_source_versions_and_every_metadata_byte_are_part_of_the_immutable_intent() {
    let plan = intent();
    let mut changed = plan.source().clone();
    changed.actor = Uuid::from_u128(99);
    assert!(
        PrSyncIntent::restore(
            changed,
            plan.content().title().into(),
            plan.content().body().into(),
            plan.request_hash()
        )
        .is_err()
    );
    let mut changed = plan.source().clone();
    changed.repository.installation_id += 1;
    assert!(
        PrSyncIntent::restore(
            changed,
            plan.content().title().into(),
            plan.content().body().into(),
            plan.request_hash()
        )
        .is_err()
    );
    let mut changed = plan.source().clone();
    changed.proposal_version += 1;
    assert!(
        PrSyncIntent::restore(
            changed,
            plan.content().title().into(),
            plan.content().body().into(),
            plan.request_hash()
        )
        .is_err()
    );
    assert!(
        PrSyncIntent::restore(
            plan.source().clone(),
            "Changed title".into(),
            plan.content().body().into(),
            plan.request_hash()
        )
        .is_err()
    );
    assert!(
        PrSyncIntent::restore(
            plan.source().clone(),
            plan.content().title().into(),
            format!("{}\nchanged", plan.content().body()),
            plan.request_hash()
        )
        .is_err()
    );
}
#[test]
fn unknown_or_bound_absence_never_authorizes_another_create() {
    let plan = intent();
    assert_eq!(
        plan.decide(&[], None, PrWritePhase::NotStarted),
        PrSyncDecision::CreateDraft
    );
    for phase in [PrWritePhase::CreateStarted, PrWritePhase::UpdateStarted] {
        assert_eq!(plan.decide(&[], None, phase), PrSyncDecision::ObserveAgain);
    }
    assert_eq!(
        plan.decide(&[], Some(bound()), PrWritePhase::NotStarted),
        PrSyncDecision::ObserveAgain
    );
}
#[test]
fn exact_receipts_reconcile_but_old_metadata_is_only_updated_before_a_write_starts() {
    let plan = intent();
    let mut pr = observed(&plan);
    assert_eq!(
        plan.decide(&[pr], Some(bound()), PrWritePhase::UpdateStarted),
        PrSyncDecision::Synchronized(bound().number)
    );
    pr = observed(&plan);
    pr.title = "Prior title".into();
    assert_eq!(
        plan.decide(&[pr], Some(bound()), PrWritePhase::NotStarted),
        PrSyncDecision::UpdateMetadata(bound().number)
    );
    pr = observed(&plan);
    pr.title = "Prior title".into();
    assert_eq!(
        plan.decide(&[pr], Some(bound()), PrWritePhase::UpdateStarted),
        PrSyncDecision::ObserveAgain
    );
}
#[test]
fn foreign_closed_ready_duplicate_and_stale_candidates_never_authorize_a_write() {
    let plan = intent();
    for mode in 0..7 {
        let mut pr = observed(&plan);
        match mode {
            0 => pr.repository_id = 99,
            1 => pr.branch = BranchName::parse("change/other").unwrap(),
            2 => pr.body = "Unmanaged human PR".into(),
            3 => pr.state = PullRequestState::Closed,
            4 => pr.state = PullRequestState::Merged,
            5 => pr.draft = false,
            _ => pr.id = 99,
        }
        assert_eq!(
            plan.decide(&[pr], Some(bound()), PrWritePhase::NotStarted),
            PrSyncDecision::Conflict
        );
    }
    assert_eq!(
        plan.decide(
            &[observed(&plan), observed(&plan)],
            None,
            PrWritePhase::NotStarted
        ),
        PrSyncDecision::Conflict
    );
    let mut pr = observed(&plan);
    pr.head = CommitSha::parse("9".repeat(40)).unwrap();
    assert_eq!(
        plan.decide(&[pr], Some(bound()), PrWritePhase::NotStarted),
        PrSyncDecision::Stale
    );
    let mut pr = observed(&plan);
    pr.target = CommitSha::parse("9".repeat(40)).unwrap();
    assert_eq!(
        plan.decide(&[pr], Some(bound()), PrWritePhase::NotStarted),
        PrSyncDecision::Stale
    );
}
#[test]
fn an_unbound_candidate_from_another_operation_cannot_be_adopted() {
    let plan = intent();
    let mut other = source();
    other.operation = Uuid::from_u128(99);
    let other = PrSyncIntent::prepare(
        other,
        projection(),
        &PrSiteOrigin::parse("https://specs.example", false).unwrap(),
    )
    .unwrap();
    let pr = observed(&other);
    assert_eq!(
        plan.decide(&[pr], None, PrWritePhase::CreateStarted),
        PrSyncDecision::Conflict
    );
}

#[test]
fn unbound_mismatches_and_malformed_managed_headers_cannot_be_overwritten() {
    let plan = intent();
    let mut pr = observed(&plan);
    pr.title = "Externally changed".into();
    assert_eq!(
        plan.decide(&[pr], None, PrWritePhase::NotStarted),
        PrSyncDecision::Conflict
    );
    let mut pr = observed(&plan);
    pr.body = pr.body.replace(
        &operation_marker(plan.source()),
        "<!-- specspace:operation:bad -->",
    );
    assert_eq!(
        plan.decide(&[pr], Some(bound()), PrWritePhase::NotStarted),
        PrSyncDecision::Conflict
    );
    let mut data = projection();
    data.description = "First line\rSecond line\r\nThird line".into();
    let plan = PrSyncIntent::prepare(
        source(),
        data,
        &PrSiteOrigin::parse("https://specs.example", false).unwrap(),
    )
    .unwrap();
    assert!(
        plan.content()
            .body()
            .contains("> First line\n> Second line\n> Third line")
    );
}
