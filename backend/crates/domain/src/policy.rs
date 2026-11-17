//! Authorization and approval rules. Callers must load fresh membership data,
//! assignments and provider refs before applying these rules to a mutation.
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Reader,
    Contributor,
    Reviewer,
    Maintainer,
    Admin,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ReviewDomain {
    Product,
    Development,
    Design,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Membership {
    pub organization_id: Uuid,
    pub user_id: Uuid,
    pub active: bool,
    pub role: Role,
    pub review_domains: Vec<ReviewDomain>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Read,
    CreateProposal,
    Edit,
    Export,
    Comment,
    Review,
    Merge,
    ManagePolicy,
}

pub struct ResourceAccess {
    pub organization_id: Uuid,
    pub repository_access: bool,
    pub assigned_editor: bool,
    pub export_allowed: bool,
}

pub fn authorized(member: &Membership, resource: &ResourceAccess, action: Action) -> bool {
    if !member.active
        || member.organization_id != resource.organization_id
        || !resource.repository_access
    {
        return false;
    }
    match action {
        Action::Read => true,
        Action::CreateProposal | Action::Comment => member.role != Role::Reader,
        Action::Edit => {
            member.role != Role::Reader && (resource.assigned_editor || member.role == Role::Admin)
        }
        Action::Export => {
            member.role != Role::Reader
                && resource.export_allowed
                && (resource.assigned_editor
                    || matches!(member.role, Role::Reviewer | Role::Maintainer | Role::Admin))
        }
        Action::Review => matches!(member.role, Role::Reviewer | Role::Maintainer | Role::Admin),
        Action::Merge => matches!(member.role, Role::Maintainer | Role::Admin),
        Action::ManagePolicy => member.role == Role::Admin,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProposalState {
    Provisioning,
    Draft,
    InReview,
    ChangesRequested,
    Approved,
    Merging,
    Merged,
    Closed,
}

impl ProposalState {
    pub fn after_content_commit(self) -> Result<Self, &'static str> {
        match self {
            Self::Draft | Self::InReview | Self::ChangesRequested => Ok(self),
            Self::Approved => Ok(Self::InReview),
            _ => Err("PROPOSAL_NOT_EDITABLE"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub id: Uuid,
    pub head_sha: String,
    pub target_sha: String,
    pub policy_version: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Approve,
    RequestChanges,
    Comment,
}

pub struct Review {
    pub snapshot_id: Uuid,
    pub reviewer_id: Uuid,
    pub domain: ReviewDomain,
    pub decision: Decision,
}

pub struct Assignment {
    pub reviewer_id: Uuid,
    pub domain: ReviewDomain,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiImpact {
    Yes,
    No,
    Undecided,
}

pub struct ApprovalContext<'a> {
    pub organization_id: Uuid,
    pub author_id: Uuid,
    pub snapshot: &'a Snapshot,
    pub current_head: &'a str,
    pub current_target: &'a str,
    pub policy_version: u64,
    pub ui_impact: UiImpact,
    pub memberships: &'a [Membership],
    pub assignments: &'a [Assignment],
    /// At most one current decision per reviewer/domain, enforced by persistence.
    pub reviews: &'a [Review],
}

/// Determines approval eligibility only. Merge additionally requires provider
/// checks, no blockers, a nonempty validated diff and a target serialization lock.
pub fn approvals_satisfied(context: &ApprovalContext<'_>) -> Result<(), &'static str> {
    let snapshot = context.snapshot;
    if snapshot.head_sha != context.current_head
        || snapshot.target_sha != context.current_target
        || snapshot.policy_version != context.policy_version
    {
        return Err("APPROVAL_STALE");
    }
    if context.ui_impact == UiImpact::Undecided {
        return Err("UI_IMPACT_UNDECIDED");
    }
    let eligible = |review: &&Review| {
        review.snapshot_id == snapshot.id
            && review.reviewer_id != context.author_id
            && context.memberships.iter().any(|member| {
                member.user_id == review.reviewer_id
                    && member.active
                    && member.organization_id == context.organization_id
                    && matches!(member.role, Role::Reviewer | Role::Maintainer | Role::Admin)
                    && member.review_domains.contains(&review.domain)
            })
            && context.assignments.iter().any(|assignment| {
                assignment.reviewer_id == review.reviewer_id && assignment.domain == review.domain
            })
    };
    if context
        .reviews
        .iter()
        .filter(eligible)
        .any(|review| review.decision == Decision::RequestChanges)
    {
        return Err("CHANGES_REQUESTED");
    }
    let approved: Vec<_> = context
        .reviews
        .iter()
        .filter(eligible)
        .filter(|review| review.decision == Decision::Approve)
        .collect();
    let independent_pair = approved.iter().any(|product| {
        product.domain == ReviewDomain::Product
            && approved.iter().any(|development| {
                development.domain == ReviewDomain::Development
                    && development.reviewer_id != product.reviewer_id
            })
    });
    if !independent_pair {
        return Err("INDEPENDENT_APPROVALS_REQUIRED");
    }
    if context.ui_impact == UiImpact::Yes
        && !approved
            .iter()
            .any(|review| review.domain == ReviewDomain::Design)
    {
        return Err("DESIGN_APPROVAL_REQUIRED");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(id: Uuid, org: Uuid, domains: Vec<ReviewDomain>) -> Membership {
        Membership {
            organization_id: org,
            user_id: id,
            active: true,
            role: Role::Reviewer,
            review_domains: domains,
        }
    }

    #[test]
    fn authorization_requires_active_membership_and_repository_scope() {
        let org = Uuid::new_v4();
        let mut user = member(Uuid::new_v4(), org, vec![]);
        let mut access = ResourceAccess {
            organization_id: org,
            repository_access: true,
            assigned_editor: false,
            export_allowed: true,
        };
        assert!(authorized(&user, &access, Action::Read));
        assert!(!authorized(&user, &access, Action::Edit));
        assert!(!authorized(&user, &access, Action::Merge));
        user.role = Role::Maintainer;
        assert!(authorized(&user, &access, Action::Merge));
        assert!(!authorized(&user, &access, Action::Edit));
        access.assigned_editor = true;
        assert!(authorized(&user, &access, Action::Edit));
        access.export_allowed = false;
        assert!(!authorized(&user, &access, Action::Export));
        user.active = false;
        assert!(!authorized(&user, &access, Action::Read));
        user.active = true;
        access.organization_id = Uuid::new_v4();
        assert!(!authorized(&user, &access, Action::Read));
        access.organization_id = org;
        access.repository_access = false;
        assert!(!authorized(&user, &access, Action::Read));
    }

    #[test]
    fn new_content_invalidates_approved_state_and_final_branches_reject_edits() {
        assert_eq!(
            ProposalState::Approved.after_content_commit(),
            Ok(ProposalState::InReview)
        );
        for state in [
            ProposalState::Merged,
            ProposalState::Closed,
            ProposalState::Merging,
            ProposalState::Provisioning,
        ] {
            assert!(state.after_content_commit().is_err());
        }
    }

    #[test]
    fn approval_requires_independent_active_qualified_assigned_current_reviewers() {
        let org = Uuid::new_v4();
        let author = Uuid::new_v4();
        let product = Uuid::new_v4();
        let development = Uuid::new_v4();
        let snapshot = Snapshot {
            id: Uuid::new_v4(),
            head_sha: "head".into(),
            target_sha: "target".into(),
            policy_version: 1,
        };
        let members = vec![
            member(product, org, vec![ReviewDomain::Product]),
            member(development, org, vec![ReviewDomain::Development]),
        ];
        let assignments = vec![
            Assignment {
                reviewer_id: product,
                domain: ReviewDomain::Product,
            },
            Assignment {
                reviewer_id: development,
                domain: ReviewDomain::Development,
            },
        ];
        let reviews = vec![
            Review {
                snapshot_id: snapshot.id,
                reviewer_id: product,
                domain: ReviewDomain::Product,
                decision: Decision::Approve,
            },
            Review {
                snapshot_id: snapshot.id,
                reviewer_id: development,
                domain: ReviewDomain::Development,
                decision: Decision::Approve,
            },
        ];
        let context = |members| ApprovalContext {
            organization_id: org,
            author_id: author,
            snapshot: &snapshot,
            current_head: "head",
            current_target: "target",
            policy_version: 1,
            ui_impact: UiImpact::No,
            memberships: members,
            assignments: &assignments,
            reviews: &reviews,
        };
        assert_eq!(approvals_satisfied(&context(&members)), Ok(()));
        let mut revoked = members.clone();
        revoked[0].active = false;
        assert_eq!(
            approvals_satisfied(&context(&revoked)),
            Err("INDEPENDENT_APPROVALS_REQUIRED")
        );
        let mut unqualified = members.clone();
        unqualified[0].review_domains.clear();
        assert!(approvals_satisfied(&context(&unqualified)).is_err());
        let mut ctx = context(&members);
        ctx.author_id = product;
        assert!(approvals_satisfied(&ctx).is_err());
        ctx.author_id = author;
        ctx.current_head = "new-head";
        assert_eq!(approvals_satisfied(&ctx), Err("APPROVAL_STALE"));
        ctx.current_head = "head";
        ctx.current_target = "new-target";
        assert_eq!(approvals_satisfied(&ctx), Err("APPROVAL_STALE"));
        ctx.current_target = "target";
        ctx.policy_version = 2;
        assert_eq!(approvals_satisfied(&ctx), Err("APPROVAL_STALE"));
        ctx.policy_version = 1;
        ctx.assignments = &[];
        assert!(approvals_satisfied(&ctx).is_err());
        ctx.assignments = &assignments;
        ctx.ui_impact = UiImpact::Yes;
        assert_eq!(approvals_satisfied(&ctx), Err("DESIGN_APPROVAL_REQUIRED"));
        ctx.ui_impact = UiImpact::Undecided;
        assert_eq!(approvals_satisfied(&ctx), Err("UI_IMPACT_UNDECIDED"));
    }

    #[test]
    fn one_person_with_two_qualifications_cannot_supply_both_required_approvals() {
        let org = Uuid::new_v4();
        let user = Uuid::new_v4();
        let snapshot = Snapshot {
            id: Uuid::new_v4(),
            head_sha: "head".into(),
            target_sha: "target".into(),
            policy_version: 1,
        };
        let domains = [ReviewDomain::Product, ReviewDomain::Development];
        let members = [member(user, org, domains.to_vec())];
        let assignments: Vec<_> = domains
            .iter()
            .map(|&domain| Assignment {
                reviewer_id: user,
                domain,
            })
            .collect();
        let reviews: Vec<_> = domains
            .iter()
            .map(|&domain| Review {
                snapshot_id: snapshot.id,
                reviewer_id: user,
                domain,
                decision: Decision::Approve,
            })
            .collect();
        let ctx = ApprovalContext {
            organization_id: org,
            author_id: Uuid::new_v4(),
            snapshot: &snapshot,
            current_head: "head",
            current_target: "target",
            policy_version: 1,
            ui_impact: UiImpact::No,
            memberships: &members,
            assignments: &assignments,
            reviews: &reviews,
        };
        assert_eq!(
            approvals_satisfied(&ctx),
            Err("INDEPENDENT_APPROVALS_REQUIRED")
        );
    }
}
