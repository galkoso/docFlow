//! Product questions are proposal-level business decisions, separate from
//! snapshot review approvals. Human and imported answers start as proposals;
//! only an explicit action by current Product authority decides them.
use crate::policy::{self, Action, Membership, ProposalState, ResourceAccess, ReviewDomain};
use uuid::Uuid;

pub const MAX_TEXT_CHARACTERS: usize = 10_000;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionText(String);
impl QuestionText {
    pub fn new(value: &str) -> Result<Self, QuestionError> {
        if value.len() > MAX_TEXT_CHARACTERS * 4
            || value.trim().is_empty()
            || value.contains('\0')
            || value.chars().take(MAX_TEXT_CHARACTERS + 1).count() > MAX_TEXT_CHARACTERS
        {
            return Err(QuestionError::InvalidText);
        }
        Ok(Self(value.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestionError {
    Forbidden,
    ProductRequired,
    NotEditable,
    InvalidText,
    InvalidState,
    MergeInProgress,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestionPhase {
    Open,
    Proposed,
    Decided,
}
pub struct QuestionContext<'a> {
    pub member: &'a Membership,
    pub resource: &'a ResourceAccess,
    pub proposal_state: ProposalState,
}
impl QuestionContext<'_> {
    fn authorize(&self) -> Result<(), QuestionError> {
        if self.member.user_id.is_nil()
            || self.member.organization_id.is_nil()
            || !policy::authorized(self.member, self.resource, Action::Comment)
        {
            return Err(QuestionError::Forbidden);
        }
        if matches!(
            self.proposal_state,
            ProposalState::Provisioning | ProposalState::Closed | ProposalState::Merged
        ) {
            return Err(QuestionError::NotEditable);
        }
        Ok(())
    }
    fn product(&self) -> bool {
        policy::authorized(self.member, self.resource, Action::Review)
            && self.member.review_domains.contains(&ReviewDomain::Product)
    }
    fn merging(&self, was_blocking: bool, next: &QuestionState) -> Result<(), QuestionError> {
        if self.proposal_state == ProposalState::Merging && !was_blocking && next.blocks_merge() {
            return Err(QuestionError::MergeInProgress);
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionState {
    pub creator: Uuid,
    pub phase: QuestionPhase,
    pub blocking: bool,
    pub answer: Option<QuestionText>,
    pub decided_by: Option<Uuid>,
}
#[derive(Debug, Clone)]
pub enum QuestionAction {
    Propose(QuestionText),
    Decide(QuestionText),
    Reopen,
    SetBlocking(bool),
}
impl QuestionState {
    /// Use for human and imported questions alike. Even a supplied answer from a
    /// Product-qualified caller is only proposed until a separate Decide action.
    pub fn open(
        context: &QuestionContext<'_>,
        blocking: bool,
        answer: Option<QuestionText>,
    ) -> Result<Self, QuestionError> {
        context.authorize()?;
        let next = Self {
            creator: context.member.user_id,
            phase: if answer.is_some() {
                QuestionPhase::Proposed
            } else {
                QuestionPhase::Open
            },
            blocking,
            answer,
            decided_by: None,
        };
        context.merging(false, &next)?;
        Ok(next)
    }
    pub fn blocks_merge(&self) -> bool {
        self.blocking && self.phase != QuestionPhase::Decided
    }
    pub fn transition(
        &self,
        context: &QuestionContext<'_>,
        action: QuestionAction,
    ) -> Result<Self, QuestionError> {
        context.authorize()?;
        if self.creator.is_nil()
            || !matches!(
                (self.phase, self.answer.is_some(), self.decided_by),
                (QuestionPhase::Open, false, None)
                    | (QuestionPhase::Proposed, true, None)
                    | (QuestionPhase::Decided, true, Some(_))
            )
            || self.decided_by.is_some_and(|id| id.is_nil())
        {
            return Err(QuestionError::InvalidState);
        }
        let product = context.product();
        let creator = context.member.user_id == self.creator;
        let mut next = self.clone();
        match action {
            QuestionAction::Propose(answer) => {
                if self.phase == QuestionPhase::Decided {
                    return Err(QuestionError::InvalidState);
                }
                next.phase = QuestionPhase::Proposed;
                next.answer = Some(answer);
                next.decided_by = None;
            }
            QuestionAction::Decide(answer) => {
                if !product {
                    return Err(QuestionError::ProductRequired);
                }
                next.phase = QuestionPhase::Decided;
                next.answer = Some(answer);
                next.decided_by = Some(context.member.user_id);
            }
            QuestionAction::Reopen => {
                if !creator && !product {
                    return Err(QuestionError::Forbidden);
                }
                if self.phase == QuestionPhase::Open {
                    return Err(QuestionError::InvalidState);
                }
                next.phase = QuestionPhase::Open;
                next.answer = None;
                next.decided_by = None;
            }
            QuestionAction::SetBlocking(blocking) => {
                if self.blocking == blocking {
                    return Err(QuestionError::InvalidState);
                }
                if !blocking && !product {
                    return Err(QuestionError::ProductRequired);
                }
                if blocking && !creator && !product {
                    return Err(QuestionError::Forbidden);
                }
                next.blocking = blocking;
            }
        }
        context.merging(self.blocks_merge(), &next)?;
        Ok(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::Role;
    fn fixture(role: Role, domains: Vec<ReviewDomain>) -> (Membership, ResourceAccess) {
        let org = Uuid::new_v4();
        (
            Membership {
                organization_id: org,
                user_id: Uuid::new_v4(),
                active: true,
                role,
                review_domains: domains,
            },
            ResourceAccess {
                organization_id: org,
                repository_access: true,
                assigned_editor: false,
                export_allowed: false,
            },
        )
    }
    fn context<'a>(member: &'a Membership, resource: &'a ResourceAccess) -> QuestionContext<'a> {
        QuestionContext {
            member,
            resource,
            proposal_state: ProposalState::InReview,
        }
    }
    fn answer() -> QuestionText {
        QuestionText::new("תשובה מפורשת 🦀\r\n").unwrap()
    }
    #[test]
    fn supplied_answers_never_become_product_decisions_without_an_explicit_qualified_action() {
        for role in [
            Role::Contributor,
            Role::Reviewer,
            Role::Maintainer,
            Role::Admin,
        ] {
            for domains in [
                vec![],
                vec![ReviewDomain::Development],
                vec![ReviewDomain::Product],
            ] {
                let (member, resource) = fixture(role, domains);
                let c = context(&member, &resource);
                let proposed = QuestionState::open(&c, true, Some(answer())).unwrap();
                assert_eq!(proposed.phase, QuestionPhase::Proposed);
                assert!(proposed.decided_by.is_none() && proposed.blocks_merge());
                let decided = proposed.transition(&c, QuestionAction::Decide(answer()));
                assert_eq!(
                    decided.is_ok(),
                    role != Role::Contributor
                        && member.review_domains.contains(&ReviewDomain::Product)
                );
                if let Ok(decided) = decided {
                    assert!(!decided.blocks_merge());
                    assert_eq!(decided.decided_by, Some(member.user_id));
                }
            }
        }
    }
    #[test]
    fn creator_cannot_clear_a_blocker_but_can_reopen_without_erasing_the_prior_state() {
        let (member, resource) = fixture(Role::Contributor, vec![]);
        let c = context(&member, &resource);
        let question = QuestionState::open(&c, true, None).unwrap();
        assert_eq!(
            question.transition(&c, QuestionAction::SetBlocking(false)),
            Err(QuestionError::ProductRequired)
        );
        let mut product = member.clone();
        product.role = Role::Reviewer;
        product.review_domains = vec![ReviewDomain::Product];
        product.user_id = Uuid::new_v4();
        let decided = question
            .transition(
                &context(&product, &resource),
                QuestionAction::Decide(answer()),
            )
            .unwrap();
        assert_eq!(
            decided.transition(&c, QuestionAction::Propose(answer())),
            Err(QuestionError::InvalidState)
        );
        let reopened = decided.transition(&c, QuestionAction::Reopen).unwrap();
        assert!(
            reopened.blocks_merge() && reopened.answer.is_none() && reopened.decided_by.is_none()
        );
        assert_eq!(decided.phase, QuestionPhase::Decided);
        assert!(decided.answer.is_some());
    }
    #[test]
    fn merging_rejects_new_blockers_but_allows_nonblocking_questions_and_safe_decisions() {
        let (member, resource) = fixture(Role::Reviewer, vec![ReviewDomain::Product]);
        let c = context(&member, &resource);
        let decided = QuestionState::open(&c, true, None)
            .unwrap()
            .transition(&c, QuestionAction::Decide(answer()))
            .unwrap();
        let merging = QuestionContext {
            proposal_state: ProposalState::Merging,
            ..c
        };
        assert_eq!(
            QuestionState::open(&merging, true, Some(answer())),
            Err(QuestionError::MergeInProgress)
        );
        assert_eq!(
            decided.transition(&merging, QuestionAction::Reopen),
            Err(QuestionError::MergeInProgress)
        );
        let nonblocking = QuestionState::open(&merging, false, None).unwrap();
        assert_eq!(
            nonblocking.transition(&merging, QuestionAction::SetBlocking(true)),
            Err(QuestionError::MergeInProgress)
        );
        assert!(
            nonblocking
                .transition(&merging, QuestionAction::Decide(answer()))
                .is_ok()
        );
    }
    #[test]
    fn current_scope_activity_and_proposal_state_are_required_for_every_action() {
        for fault in 0..7 {
            let (mut member, mut resource) = fixture(Role::Reviewer, vec![ReviewDomain::Product]);
            let mut state = ProposalState::InReview;
            match fault {
                0 => member.active = false,
                1 => member.role = Role::Reader,
                2 => resource.organization_id = Uuid::new_v4(),
                3 => resource.repository_access = false,
                4 => state = ProposalState::Closed,
                5 => state = ProposalState::Merged,
                _ => state = ProposalState::Provisioning,
            }
            let c = QuestionContext {
                member: &member,
                resource: &resource,
                proposal_state: state,
            };
            assert!(QuestionState::open(&c, false, None).is_err());
        }
    }
    #[test]
    fn text_is_bounded_unicode_and_preserves_original_bytes() {
        for text in ["", " \t\r\n", "\u{2003}", "answer\0text"] {
            assert_eq!(QuestionText::new(text), Err(QuestionError::InvalidText));
        }
        let text = "🦀".repeat(10_000);
        assert_eq!(QuestionText::new(&text).unwrap().as_str(), text);
        assert!(QuestionText::new(&format!("{text}a")).is_err());
        assert_eq!(answer().as_str(), "תשובה מפורשת 🦀\r\n");
    }
}
