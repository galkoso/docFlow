use super::*;
use crate::review::{PreparedReview, ReviewAssignment, ReviewContext};
use specspace_domain::policy::{Membership, ReviewDomain, Role, UiImpact};
mod preparation;
fn context(f: &Fixture) -> ReviewContext {
    let org = f.auth.context.lock().unwrap().organization;
    let product = Uuid::new_v4();
    let development = Uuid::new_v4();
    let member = |user, domain| Membership {
        organization_id: org,
        user_id: user,
        active: true,
        role: Role::Reviewer,
        review_domains: vec![domain],
    };
    ReviewContext {
        actor: Membership {
            organization_id: org,
            user_id: f.actor,
            active: true,
            role: Role::Contributor,
            review_domains: vec![],
        },
        assigned_editor: true,
        repository_access: true,
        proposal: f.proposal,
        repository: f.factory.binding.repository,
        author: Uuid::new_v4(),
        owner: f.actor,
        team: Uuid::new_v4(),
        description: "Synthetic change requiring independent review".into(),
        expected_head: sha(HEAD),
        expected_target: sha(MAIN),
        expected_version: 3,
        policy_version: 1,
        configuration_version: 1,
        ui_impact: UiImpact::No,
        ui_impact_reason: Some("No user interface change".into()),
        assignments: vec![
            ReviewAssignment {
                reviewer: product,
                domain: ReviewDomain::Product,
                required: true,
            },
            ReviewAssignment {
                reviewer: development,
                domain: ReviewDomain::Development,
                required: true,
            },
        ],
        reviewer_memberships: vec![
            member(product, ReviewDomain::Product),
            member(development, ReviewDomain::Development),
        ],
    }
}
async fn loaded(f: &Fixture) -> LoadedComparison {
    let actor = f.auth.context.lock().unwrap().actor;
    f.reader
        .load(actor, f.proposal, Selection::default())
        .await
        .unwrap()
}
#[tokio::test]
async fn complete_manifest_binds_saved_git_pair_and_configuration_without_publishing_or_claiming_approval()
 {
    let f = fixture();
    let source = loaded(&f).await;
    let ctx = context(&f);
    let plan = PreparedReview::prepare(ctx, &source).unwrap();
    let summary = plan.summary();
    assert_eq!(summary.head_sha, HEAD);
    assert_eq!(summary.target_sha, MAIN);
    assert_eq!(summary.merge_base_sha, OLD);
    assert!(summary.base_outdated);
    assert_eq!(summary.fingerprint.len(), 64);
    assert_eq!(summary.expected_proposal_version, 3);
    assert_eq!(plan.files().len(), source.inventory().changes.len());
    assert_eq!(plan.files(), source.inventory().changes.as_slice());
    assert_eq!(plan.assignments().len(), 2);
    assert!(plan.required_media().is_empty());
    assert!(plan.validation().ready());
    assert!(!plan.required_units().is_empty());
    let json = serde_json::to_string(summary).unwrap();
    assert!(!json.contains("proposal edit"));
    assert!(!json.contains("approved"));
}
#[tokio::test]
async fn bounded_review_requirements_cover_every_changed_file_without_dropping_large_sections() {
    let mut body = String::new();
    for n in 0..10 {
        body.push_str(&format!(
            "<!-- ss:section:SEC-{n} -->\n# Heading\n\nChanged text\n"
        ));
    }
    let f = fixture_body(&body);
    let source = loaded(&f).await;
    let units = crate::review_units::prepare_with_limit(&source, 1).unwrap();
    assert_eq!(units.len(), 1);
    assert_eq!(units[0].file_id, source.inventory().changes[0].id);
    assert!(matches!(
        units[0].requirement,
        crate::review_units::Requirement::File {
            reason: crate::review_units::FileRequirement::ManifestLimit
        }
    ));
    assert_eq!(
        units[0]
            .after
            .as_ref()
            .unwrap()
            .range
            .as_ref()
            .unwrap()
            .end_byte,
        source.after().source("docs/a.md").unwrap().len()
    );
    assert!(crate::review_units::prepare_with_limit(&source, 0).is_err());
    let plan = PreparedReview::prepare(context(&f), &source).unwrap();
    assert!(plan.required_units().len() > 1);
}
#[tokio::test]
async fn complete_book_manifest_retains_unchanged_documents_in_catalog_order_without_inventing_changes()
 {
    let f = fixture_book_documents("proposal edit", vec![], true);
    let source = loaded(&f).await;
    let ctx = context(&f);
    let plan = PreparedReview::prepare(ctx.clone(), &source).unwrap();
    assert_eq!(plan.files().len(), 1);
    assert_eq!(plan.files()[0].after.as_ref().unwrap().path, "docs/a.md");
    let documents = plan.book_documents();
    assert_eq!(documents.len(), 2);
    assert_eq!(documents[0].source.document_id.as_deref(), Some("DOC-B"));
    assert_eq!(documents[0].source.catalog_position, Some(0));
    assert_eq!(documents[1].source.document_id.as_deref(), Some("DOC-A"));
    for document in documents {
        assert_eq!(document.id.len(), 64);
        assert!(document.source.sha256.is_some());
        assert_eq!(
            document.source.blob_oid,
            specspace_git_adapter::blob::oid(
                source
                    .after()
                    .source(&document.source.path)
                    .unwrap()
                    .as_bytes(),
                40
            )
            .unwrap()
            .as_str()
        );
    }
    let encoded = serde_json::to_string(documents).unwrap();
    assert!(!encoded.contains("Original unchanged text"));
    assert!(!encoded.contains("proposal edit"));
    let mut ctx = ctx;
    ctx.assignments.reverse();
    let reordered = PreparedReview::prepare(ctx, &source).unwrap();
    assert_eq!(documents, reordered.book_documents());
    assert_eq!(plan.summary().fingerprint, reordered.summary().fingerprint);
}
#[tokio::test]
async fn canonical_fingerprint_is_shared_and_ignores_list_order_actor_and_state_only_version_changes()
 {
    let f = fixture();
    let source = loaded(&f).await;
    let ctx = context(&f);
    let fingerprint = PreparedReview::prepare(ctx.clone(), &source)
        .unwrap()
        .summary()
        .fingerprint
        .clone();
    let mut reordered = ctx.clone();
    reordered.assignments.reverse();
    reordered.reviewer_memberships.reverse();
    assert_eq!(
        PreparedReview::prepare(reordered, &source)
            .unwrap()
            .summary()
            .fingerprint,
        fingerprint
    );
    {
        let mut c = f.auth.context.lock().unwrap();
        c.state = "IN_REVIEW".into();
        c.version = 4;
    }
    let newer = loaded(&f).await;
    let mut next = ctx.clone();
    next.expected_version = 4;
    assert_ne!(source.id(), newer.id());
    assert_eq!(
        PreparedReview::prepare(next.clone(), &newer)
            .unwrap()
            .summary()
            .fingerprint,
        fingerprint
    );
    let another = Uuid::new_v4();
    f.auth.context.lock().unwrap().actor = another;
    next.actor.user_id = another;
    let other = loaded(&f).await;
    assert_eq!(
        PreparedReview::prepare(next, &other)
            .unwrap()
            .summary()
            .fingerprint,
        fingerprint
    );
}
#[tokio::test]
async fn policy_assignment_ui_scope_and_metadata_changes_cannot_reuse_a_fingerprint() {
    let f = fixture();
    let source = loaded(&f).await;
    let ctx = context(&f);
    let fingerprint = PreparedReview::prepare(ctx.clone(), &source)
        .unwrap()
        .summary()
        .fingerprint
        .clone();
    for change in 0..6 {
        let mut c = ctx.clone();
        match change {
            0 => c.policy_version += 1,
            1 => c.configuration_version += 1,
            2 => c.description.push_str(" changed"),
            3 => c.ui_impact = UiImpact::Undecided,
            4 => c.assignments[0].required = false,
            _ => c.ui_impact_reason = Some("Another UI explanation".into()),
        }
        assert_ne!(
            PreparedReview::prepare(c, &source)
                .unwrap()
                .summary()
                .fingerprint,
            fingerprint
        );
    }
}
#[tokio::test]
async fn current_actor_scope_roles_assignment_and_expected_versions_are_mandatory() {
    let f = fixture();
    let source = loaded(&f).await;
    let ctx = context(&f);
    for change in 0..10 {
        let mut c = ctx.clone();
        match change {
            0 => c.actor.user_id = Uuid::new_v4(),
            1 => c.actor.organization_id = Uuid::new_v4(),
            2 => c.actor.active = false,
            3 => c.actor.role = Role::Reader,
            4 => c.assigned_editor = false,
            5 => c.repository_access = false,
            6 => c.proposal = Uuid::new_v4(),
            7 => c.repository = Uuid::new_v4(),
            8 => c.expected_head = sha(OLD),
            _ => c.expected_version += 1,
        }
        assert!(PreparedReview::prepare(c, &source).is_err());
    }
    let mut admin = ctx;
    admin.actor.role = Role::Admin;
    admin.assigned_editor = false;
    assert!(PreparedReview::prepare(admin, &source).is_ok());
}
#[tokio::test]
async fn reviewers_must_be_independent_active_qualified_assigned_and_unambiguous() {
    let f = fixture();
    let source = loaded(&f).await;
    let ctx = context(&f);
    for change in 0..8 {
        let mut c = ctx.clone();
        match change {
            0 => c.assignments.clear(),
            1 => c.author = c.assignments[0].reviewer,
            2 => c.reviewer_memberships[0].active = false,
            3 => c.reviewer_memberships[0].role = Role::Contributor,
            4 => c.reviewer_memberships[0].review_domains.clear(),
            5 => c.reviewer_memberships[0].organization_id = Uuid::new_v4(),
            6 => c.assignments.push(c.assignments[0].clone()),
            _ => c
                .reviewer_memberships
                .push(c.reviewer_memberships[0].clone()),
        }
        assert!(PreparedReview::prepare(c, &source).is_err());
    }
    let mut one = ctx;
    let user = one.assignments[0].reviewer;
    one.assignments[1].reviewer = user;
    one.reviewer_memberships.remove(1);
    one.reviewer_memberships[0]
        .review_domains
        .push(ReviewDomain::Development);
    assert_eq!(
        PreparedReview::prepare(one, &source).err().unwrap().code(),
        "INDEPENDENT_REVIEWERS_REQUIRED"
    );
}
#[tokio::test]
async fn design_is_required_for_ui_changes_and_undecided_ui_remains_explicit() {
    let f = fixture();
    let source = loaded(&f).await;
    let mut ctx = context(&f);
    ctx.ui_impact = UiImpact::Yes;
    assert_eq!(
        PreparedReview::prepare(ctx.clone(), &source)
            .err()
            .unwrap()
            .code(),
        "DESIGN_REVIEWER_REQUIRED"
    );
    let design = Uuid::new_v4();
    ctx.assignments.push(ReviewAssignment {
        reviewer: design,
        domain: ReviewDomain::Design,
        required: true,
    });
    ctx.reviewer_memberships.push(Membership {
        organization_id: ctx.actor.organization_id,
        user_id: design,
        active: true,
        role: Role::Reviewer,
        review_domains: vec![ReviewDomain::Design],
    });
    assert_eq!(
        PreparedReview::prepare(ctx.clone(), &source)
            .unwrap()
            .summary()
            .ui_impact,
        "YES"
    );
    ctx.ui_impact = UiImpact::Undecided;
    assert_eq!(
        PreparedReview::prepare(ctx, &source)
            .unwrap()
            .summary()
            .ui_impact,
        "UNDECIDED"
    );
}
#[tokio::test]
async fn historical_missing_moved_and_final_proposal_sources_never_prepare_a_review() {
    let f = fixture();
    let historical = f
        .reader
        .load(
            f.actor,
            f.proposal,
            Selection {
                head: Some(sha(OLD)),
                target: None,
            },
        )
        .await
        .unwrap();
    assert!(PreparedReview::prepare(context(&f), &historical).is_err());
    *f.provider.branch.lock().unwrap() = Some(LATER.into());
    let moved = loaded(&f).await;
    assert_eq!(
        PreparedReview::prepare(context(&f), &moved)
            .err()
            .unwrap()
            .code(),
        "REVIEW_BRANCH_CHANGED"
    );
    *f.provider.branch.lock().unwrap() = None;
    let missing = loaded(&f).await;
    assert_eq!(
        PreparedReview::prepare(context(&f), &missing)
            .err()
            .unwrap()
            .code(),
        "REVIEW_BRANCH_CHANGED"
    );
    *f.provider.branch.lock().unwrap() = Some(HEAD.into());
    for state in ["CLOSED", "MERGED", "MERGING"] {
        f.auth.context.lock().unwrap().state = state.into();
        let source = loaded(&f).await;
        assert_eq!(
            PreparedReview::prepare(context(&f), &source)
                .err()
                .unwrap()
                .code(),
            "PROPOSAL_NOT_REVIEWABLE"
        );
    }
}
#[tokio::test]
async fn no_change_old_target_invalid_description_and_missing_ui_reason_are_explicit() {
    let f = fixture();
    let source = loaded(&f).await;
    let ctx = context(&f);
    for change in 0..4 {
        let mut c = ctx.clone();
        match change {
            0 => c.description = "short".into(),
            1 => c.ui_impact_reason = None,
            2 => c.policy_version = 0,
            _ => c.team = Uuid::nil(),
        };
        assert!(PreparedReview::prepare(c, &source).is_err());
    }
    let old = f
        .reader
        .load(
            f.actor,
            f.proposal,
            Selection {
                head: None,
                target: Some(sha(OLD)),
            },
        )
        .await
        .unwrap();
    let mut c = context(&f);
    c.expected_target = sha(OLD);
    assert_eq!(
        PreparedReview::prepare(c, &old).err().unwrap().code(),
        "REVIEW_TARGET_CHANGED"
    );
    f.auth.context.lock().unwrap().recorded_head = sha(OLD);
    *f.provider.branch.lock().unwrap() = Some(OLD.into());
    let same = loaded(&f).await;
    let mut c = context(&f);
    c.expected_head = sha(OLD);
    assert_eq!(
        PreparedReview::prepare(c, &same).err().unwrap().code(),
        "REVIEW_NO_CHANGES"
    );
}
#[tokio::test]
async fn whole_candidate_validation_catches_unchanged_references_and_keeps_warnings_visible() {
    let invalid = fixture_body("[broken](missing.md#SEC-MISSING)");
    let source = loaded(&invalid).await;
    assert_eq!(
        PreparedReview::prepare(context(&invalid), &source)
            .err()
            .unwrap()
            .code(),
        "REVIEW_VALIDATION_FAILED"
    );
    let warning = fixture_body("<script>inert()</script>");
    let source = loaded(&warning).await;
    let plan = PreparedReview::prepare(context(&warning), &source).unwrap();
    assert!(plan.summary().content_findings_total > 0);
    assert!(
        plan.validation()
            .findings
            .iter()
            .any(|finding| finding.code == "UNSUPPORTED_HTML")
    );
}

#[tokio::test]
async fn media_requirements_cover_new_and_referenced_assets_once_without_claiming_decode_verification()
 {
    use sha2::{Digest, Sha256};
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../fixtures/repository/assets");
    let bytes = std::fs::read(
        std::fs::read_dir(root)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path(),
    )
    .unwrap();
    let path = format!("assets/{:x}.png", Sha256::digest(&bytes));
    let body = format!("![Descriptive alt]({path})");
    let f = fixture_asset(&body, Some((path.clone(), bytes.clone())));
    let source = loaded(&f).await;
    let plan = PreparedReview::prepare(context(&f), &source).unwrap();
    assert_eq!(plan.required_media().len(), 1);
    assert_eq!(plan.summary().media_checks_required, 1);
    assert_eq!(plan.required_media()[0].path, path);
    assert_eq!(plan.required_media()[0].bytes, bytes.len() as u64);
    assert_eq!(
        plan.required_media()[0].blob_oid,
        blob::oid(&bytes, 40).unwrap().as_str()
    );
    assert_eq!(plan.files().len(), 2); // The changed document plus the actual added asset.
    let unused = fixture_asset("No image reference", Some((path, bytes)));
    let source = loaded(&unused).await;
    assert_eq!(
        PreparedReview::prepare(context(&unused), &source)
            .unwrap()
            .required_media()
            .len(),
        1
    );
    // This pure plan creates work without reading or certifying any binary.
    assert!(f.provider.binary_reads.lock().unwrap().is_empty());
    assert!(unused.provider.binary_reads.lock().unwrap().is_empty());
}
#[tokio::test]
async fn unsupported_new_assets_and_missing_image_alt_do_not_disappear_from_review_requirements() {
    let invalid = fixture_asset(
        "Changed body",
        Some(("assets/unsupported.svg".into(), b"<svg/>".to_vec())),
    );
    let source = loaded(&invalid).await;
    assert_eq!(
        PreparedReview::prepare(context(&invalid), &source)
            .err()
            .unwrap()
            .code(),
        "REVIEW_MEDIA_UNSUPPORTED"
    );
    use sha2::{Digest, Sha256};
    let bytes = b"Requires later decoder verification".to_vec();
    let path = format!("assets/{:x}.png", Sha256::digest(&bytes));
    let missing_alt = fixture_asset(&format!("![]({path})"), Some((path, bytes)));
    let source = loaded(&missing_alt).await;
    let plan = PreparedReview::prepare(context(&missing_alt), &source).unwrap();
    assert_eq!(plan.required_media().len(), 1);
    assert!(
        plan.validation()
            .findings
            .iter()
            .any(|finding| finding.code == "IMAGE_ALT_REQUIRED")
    );
    assert!(plan.summary().content_findings_total > 0);
}
#[tokio::test]
async fn truncated_diagnostics_keep_full_finding_counts_and_never_turn_into_clean_review_content() {
    let body = (0..1100)
        .map(|n| format!("<script>inert{n}()</script>\n\n"))
        .collect::<String>();
    let f = fixture_body(&body);
    let source = loaded(&f).await;
    let plan = PreparedReview::prepare(context(&f), &source).unwrap();
    assert!(plan.validation().findings_truncated);
    assert_eq!(plan.validation().findings.len(), 1000);
    assert!(plan.summary().content_findings_total > 1000);
}
