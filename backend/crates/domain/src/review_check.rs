//! Calculation for the head-bound required site-policy check. Callers must load
//! current server/provider evidence; this module neither publishes a check nor
//! authorizes merge. External required checks belong to the separate merge gate
//! and must not recursively include this check in its own prerequisites.
use crate::policy::{self, ApprovalContext, Assignment, Decision, ProposalState, Role};
use std::collections::HashSet;

pub const CHECK_NAME: &str = "specspace/review-policy";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verification {
    Unknown,
    Pending,
    Failed,
    Passed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Conclusion {
    Pending,
    Failure,
    Success,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gate {
    Evidence,
    ProposalState,
    Snapshot,
    Approvals,
    RequiredReviewer,
    BaseUpdate,
    ContentValidation,
    MediaValidation,
    SectionCoverage,
    BlockingThreads,
    BlockingQuestions,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Issue {
    pub gate: Gate,
    pub conclusion: Conclusion,
    pub code: &'static str,
}
#[derive(Debug, PartialEq, Eq)]
pub struct Evaluation {
    pub conclusion: Conclusion,
    pub issues: Vec<Issue>,
}
/// These are trusted read inputs, not a browser request. Required assignments
/// come from the frozen, currently applicable review configuration. A Passed
/// coverage value must include all mandatory changes for every counted reviewer.
pub struct Context<'a> {
    pub approvals: &'a ApprovalContext<'a>,
    pub required_assignments: &'a [Assignment],
    pub proposal_state: ProposalState,
    pub active_snapshot: bool,
    pub base_updated: Verification,
    pub content_validation: Verification,
    pub media_validation: Verification,
    pub section_coverage: Verification,
    pub blocking_threads_resolved: Verification,
    pub blocking_questions_resolved: Verification,
}
fn oid(value: &str) -> bool {
    (40..=128).contains(&value.len())
        && value.len().is_multiple_of(2)
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub fn evaluate(context: &Context<'_>) -> Evaluation {
    let a = context.approvals;
    let mut issues = Vec::new();
    let mut add = |gate, conclusion, code| {
        issues.push(Issue {
            gate,
            conclusion,
            code,
        })
    };
    // Ambiguous or malformed authority must not be interpreted as no blockers.
    let mut memberships = HashSet::new();
    let mut assignments = HashSet::new();
    let mut reviews = HashSet::new();
    let mut required = HashSet::new();
    let valid = !a.organization_id.is_nil()
        && !a.author_id.is_nil()
        && !a.snapshot.id.is_nil()
        && a.policy_version > 0
        && a.snapshot.policy_version > 0
        && [
            a.current_head,
            a.current_target,
            &a.snapshot.head_sha,
            &a.snapshot.target_sha,
        ]
        .into_iter()
        .all(oid)
        && a.memberships
            .iter()
            .filter(|m| m.organization_id == a.organization_id)
            .all(|m| !m.user_id.is_nil() && memberships.insert(m.user_id))
        && a.assignments
            .iter()
            .all(|v| !v.reviewer_id.is_nil() && assignments.insert((v.reviewer_id, v.domain)))
        && a.reviews
            .iter()
            .filter(|r| r.snapshot_id == a.snapshot.id)
            .all(|r| reviews.insert((r.reviewer_id, r.domain)))
        && context.required_assignments.iter().all(|v| {
            required.insert((v.reviewer_id, v.domain))
                && assignments.contains(&(v.reviewer_id, v.domain))
        });
    if !valid {
        add(
            Gate::Evidence,
            Conclusion::Failure,
            "REVIEW_EVIDENCE_INVALID",
        );
    }
    if !matches!(
        context.proposal_state,
        ProposalState::InReview | ProposalState::ChangesRequested | ProposalState::Approved
    ) {
        add(
            Gate::ProposalState,
            Conclusion::Failure,
            "PROPOSAL_NOT_REVIEWABLE",
        );
    }
    if !context.active_snapshot {
        add(Gate::Snapshot, Conclusion::Failure, "SNAPSHOT_NOT_CURRENT");
    }
    if let Err(code) = policy::approvals_satisfied(a) {
        let result = match code {
            "INDEPENDENT_APPROVALS_REQUIRED" | "DESIGN_APPROVAL_REQUIRED" => Conclusion::Pending,
            _ => Conclusion::Failure,
        };
        add(Gate::Approvals, result, code);
    }
    let approved = |v: &&Assignment| {
        v.reviewer_id != a.author_id
            && a.memberships.iter().any(|m| {
                m.organization_id == a.organization_id
                    && m.user_id == v.reviewer_id
                    && m.active
                    && matches!(m.role, Role::Reviewer | Role::Maintainer | Role::Admin)
                    && m.review_domains.contains(&v.domain)
            })
            && a.reviews.iter().any(|r| {
                r.snapshot_id == a.snapshot.id
                    && r.reviewer_id == v.reviewer_id
                    && r.domain == v.domain
                    && r.decision == Decision::Approve
            })
    };
    if context.required_assignments.iter().any(|v| !approved(&v)) {
        add(
            Gate::RequiredReviewer,
            Conclusion::Pending,
            "REQUIRED_REVIEWER_APPROVAL_MISSING",
        );
    }
    for (gate, value) in [
        (Gate::BaseUpdate, context.base_updated),
        (Gate::ContentValidation, context.content_validation),
        (Gate::MediaValidation, context.media_validation),
        (Gate::SectionCoverage, context.section_coverage),
        (Gate::BlockingThreads, context.blocking_threads_resolved),
        (Gate::BlockingQuestions, context.blocking_questions_resolved),
    ] {
        match value {
            Verification::Passed => (),
            Verification::Unknown => add(gate, Conclusion::Pending, "POLICY_EVIDENCE_UNAVAILABLE"),
            Verification::Pending => add(gate, Conclusion::Pending, "POLICY_EVIDENCE_PENDING"),
            Verification::Failed => add(gate, Conclusion::Failure, "POLICY_GATE_FAILED"),
        }
    }
    let conclusion = if issues.iter().any(|v| v.conclusion == Conclusion::Failure) {
        Conclusion::Failure
    } else if issues.is_empty() {
        Conclusion::Success
    } else {
        Conclusion::Pending
    };
    Evaluation { conclusion, issues }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{Membership, Review, ReviewDomain, Snapshot, UiImpact};
    use uuid::Uuid;

    struct Fixture {
        org: Uuid,
        author: Uuid,
        snapshot: Snapshot,
        members: Vec<Membership>,
        assignments: Vec<Assignment>,
        reviews: Vec<Review>,
    }
    impl Fixture {
        fn new() -> Self {
            let org = Uuid::new_v4();
            let snapshot = Snapshot {
                id: Uuid::new_v4(),
                head_sha: "a".repeat(40),
                target_sha: "b".repeat(40),
                policy_version: 1,
            };
            let members: Vec<_> = [ReviewDomain::Product, ReviewDomain::Development]
                .into_iter()
                .map(|domain| Membership {
                    organization_id: org,
                    user_id: Uuid::new_v4(),
                    active: true,
                    role: Role::Reviewer,
                    review_domains: vec![domain],
                })
                .collect();
            let assignments = members
                .iter()
                .map(|m| Assignment {
                    reviewer_id: m.user_id,
                    domain: m.review_domains[0],
                })
                .collect();
            let reviews = members
                .iter()
                .map(|m| Review {
                    snapshot_id: snapshot.id,
                    reviewer_id: m.user_id,
                    domain: m.review_domains[0],
                    decision: Decision::Approve,
                })
                .collect();
            Self {
                org,
                author: Uuid::new_v4(),
                snapshot,
                members,
                assignments,
                reviews,
            }
        }
        fn approvals(&self) -> ApprovalContext<'_> {
            ApprovalContext {
                organization_id: self.org,
                author_id: self.author,
                snapshot: &self.snapshot,
                current_head: &self.snapshot.head_sha,
                current_target: &self.snapshot.target_sha,
                policy_version: 1,
                ui_impact: UiImpact::No,
                memberships: &self.members,
                assignments: &self.assignments,
                reviews: &self.reviews,
            }
        }
    }
    fn context<'a>(approvals: &'a ApprovalContext<'a>) -> Context<'a> {
        Context {
            approvals,
            required_assignments: approvals.assignments,
            proposal_state: ProposalState::InReview,
            active_snapshot: true,
            base_updated: Verification::Passed,
            content_validation: Verification::Passed,
            media_validation: Verification::Passed,
            section_coverage: Verification::Passed,
            blocking_threads_resolved: Verification::Passed,
            blocking_questions_resolved: Verification::Passed,
        }
    }
    #[test]
    fn qualified_independent_approvals_and_all_current_evidence_produce_success() {
        let f = Fixture::new();
        let a = f.approvals();
        assert_eq!(
            evaluate(&context(&a)),
            Evaluation {
                conclusion: Conclusion::Success,
                issues: vec![]
            }
        );
        assert_eq!(CHECK_NAME, "specspace/review-policy");
    }
    #[test]
    fn every_unknown_or_pending_gate_blocks_success_and_every_failed_gate_fails() {
        let f = Fixture::new();
        let a = f.approvals();
        for gate in 0..6 {
            for state in [
                Verification::Unknown,
                Verification::Pending,
                Verification::Failed,
            ] {
                let mut c = context(&a);
                let field = match gate {
                    0 => &mut c.base_updated,
                    1 => &mut c.content_validation,
                    2 => &mut c.media_validation,
                    3 => &mut c.section_coverage,
                    4 => &mut c.blocking_threads_resolved,
                    _ => &mut c.blocking_questions_resolved,
                };
                *field = state;
                assert_eq!(
                    evaluate(&c).conclusion,
                    if state == Verification::Failed {
                        Conclusion::Failure
                    } else {
                        Conclusion::Pending
                    }
                );
            }
        }
    }
    #[test]
    fn new_head_target_policy_or_inactive_snapshot_cannot_reuse_approvals() {
        let f = Fixture::new();
        for mode in 0..4 {
            let mut a = f.approvals();
            let changed = "c".repeat(40);
            match mode {
                0 => a.current_head = &changed,
                1 => a.current_target = &changed,
                2 => a.policy_version = 2,
                _ => (),
            };
            let mut c = context(&a);
            if mode == 3 {
                c.active_snapshot = false;
            }
            assert_eq!(evaluate(&c).conclusion, Conclusion::Failure);
        }
    }
    #[test]
    fn revoked_foreign_unqualified_or_self_approval_never_counts() {
        for mode in 0..4 {
            let mut f = Fixture::new();
            match mode {
                0 => f.members[0].active = false,
                1 => f.members[0].organization_id = Uuid::new_v4(),
                2 => f.members[0].review_domains.clear(),
                _ => f.author = f.members[0].user_id,
            };
            let a = f.approvals();
            assert_eq!(evaluate(&context(&a)).conclusion, Conclusion::Pending);
        }
    }
    #[test]
    fn independent_pair_does_not_replace_an_additional_required_reviewer() {
        let mut f = Fixture::new();
        let reviewer = Uuid::new_v4();
        f.assignments.push(Assignment {
            reviewer_id: reviewer,
            domain: ReviewDomain::Development,
        });
        let a = f.approvals();
        let result = evaluate(&context(&a));
        assert_eq!(result.conclusion, Conclusion::Pending);
        assert!(
            result
                .issues
                .iter()
                .any(|v| v.gate == Gate::RequiredReviewer)
        );
    }
    #[test]
    fn changes_requested_is_a_failure_but_a_comment_is_not_a_replacement_approval() {
        let mut f = Fixture::new();
        f.reviews[0].decision = Decision::RequestChanges;
        let a = f.approvals();
        assert_eq!(evaluate(&context(&a)).conclusion, Conclusion::Failure);
        f.reviews[0].decision = Decision::Comment;
        let a = f.approvals();
        assert_eq!(evaluate(&context(&a)).conclusion, Conclusion::Pending);
    }
    #[test]
    fn duplicate_current_decisions_or_membership_records_fail_as_ambiguous_evidence() {
        let mut f = Fixture::new();
        f.reviews.push(Review {
            snapshot_id: f.snapshot.id,
            reviewer_id: f.reviews[0].reviewer_id,
            domain: ReviewDomain::Product,
            decision: Decision::Comment,
        });
        let a = f.approvals();
        assert_eq!(evaluate(&context(&a)).conclusion, Conclusion::Failure);
        f.reviews.pop();
        f.members.push(f.members[0].clone());
        let a = f.approvals();
        assert_eq!(evaluate(&context(&a)).conclusion, Conclusion::Failure);
    }
    #[test]
    fn malformed_refs_and_unreviewable_proposal_states_cannot_pass() {
        let mut f = Fixture::new();
        f.snapshot.head_sha = "unbound-head".into();
        let a = f.approvals();
        assert_eq!(evaluate(&context(&a)).conclusion, Conclusion::Failure);
        let f = Fixture::new();
        let a = f.approvals();
        for state in [
            ProposalState::Provisioning,
            ProposalState::Draft,
            ProposalState::Closed,
            ProposalState::Merging,
            ProposalState::Merged,
        ] {
            let mut c = context(&a);
            c.proposal_state = state;
            assert_eq!(evaluate(&c).conclusion, Conclusion::Failure);
        }
    }
}
