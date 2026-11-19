//! Discussion policy, independent of official review decisions. Callers must
//! load current membership/resource scope and lock the proposal and thread before
//! persisting a transition, together with its version check, audit and event.
use crate::policy::{self, Action, Membership, ResourceAccess, Role};
use uuid::Uuid;

pub const MAX_COMMENT_CHARACTERS: usize = 10_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentBody(String);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscussionError {
    Forbidden,
    IndependentReviewerRequired,
    EmptyComment,
    CommentTooLong,
    InvalidComment,
}

impl CommentBody {
    /// Count Unicode scalar values, matching PostgreSQL char_length. Preserve
    /// the submitted bytes; whitespace-only text and PostgreSQL-incompatible NUL
    /// are rejected before any database or provider work.
    pub fn new(body: &str) -> Result<Self, DiscussionError> {
        if body.len() > MAX_COMMENT_CHARACTERS * 4
            || body.chars().take(MAX_COMMENT_CHARACTERS + 1).count() > MAX_COMMENT_CHARACTERS
        {
            return Err(DiscussionError::CommentTooLong);
        }
        if body.contains('\0') {
            return Err(DiscussionError::InvalidComment);
        }
        if body.trim().is_empty() {
            return Err(DiscussionError::EmptyComment);
        }
        Ok(Self(body.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// These values come from current authorization and persisted thread state,
/// never a browser role, assignment claim or independently supplied author ID.
pub struct DiscussionContext<'a> {
    pub member: &'a Membership,
    pub resource: &'a ResourceAccess,
    pub proposal_author: Uuid,
}

impl DiscussionContext<'_> {
    pub fn authorize_comment(&self) -> Result<(), DiscussionError> {
        if policy::authorized(self.member, self.resource, Action::Comment) {
            Ok(())
        } else {
            Err(DiscussionError::Forbidden)
        }
    }

    /// Editing and tombstoning are restricted to the message's original author.
    /// This permission never grants authority to resolve its parent thread or
    /// modify/withdraw any official review decision.
    pub fn authorize_comment_change(&self, comment_author: Uuid) -> Result<(), DiscussionError> {
        self.authorize_comment()?;
        if self.member.user_id == comment_author {
            Ok(())
        } else {
            Err(DiscussionError::Forbidden)
        }
    }

    fn independent_reviewer(&self) -> bool {
        self.member.user_id != self.proposal_author
            && policy::authorized(self.member, self.resource, Action::Review)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadAction {
    Resolve,
    Reopen,
    SetBlocking(bool),
}

/// Persist all fields. independent_requirement is monotonic provenance: it must
/// survive role revocation, comment tombstones, new snapshots and reopening.
/// It is not an approval, and has no effect on official-review history/pointers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThreadState {
    pub creator: Uuid,
    pub blocking: bool,
    pub resolved: bool,
    pub independent_requirement: bool,
}

impl ThreadState {
    pub fn open(context: &DiscussionContext<'_>, blocking: bool) -> Result<Self, DiscussionError> {
        context.authorize_comment()?;
        Ok(Self {
            creator: context.member.user_id,
            blocking,
            resolved: false,
            independent_requirement: blocking && context.independent_reviewer(),
        })
    }

    pub fn transition(
        self,
        context: &DiscussionContext<'_>,
        action: ThreadAction,
    ) -> Result<Self, DiscussionError> {
        context.authorize_comment()?;
        let creator = context.member.user_id == self.creator;
        let reviewer = policy::authorized(context.member, context.resource, Action::Review);
        let maintainer = matches!(context.member.role, Role::Maintainer | Role::Admin);
        let author_cannot_clear =
            self.independent_requirement && context.member.user_id == context.proposal_author;
        let mut next = self;
        match action {
            ThreadAction::Resolve => {
                if !creator && !reviewer {
                    return Err(DiscussionError::Forbidden);
                }
                if author_cannot_clear {
                    return Err(DiscussionError::IndependentReviewerRequired);
                }
                next.resolved = true;
            }
            ThreadAction::Reopen => {
                if !creator && !reviewer {
                    return Err(DiscussionError::Forbidden);
                }
                next.resolved = false;
            }
            ThreadAction::SetBlocking(blocking) => {
                if !creator && !maintainer {
                    return Err(DiscussionError::Forbidden);
                }
                if !blocking && author_cannot_clear {
                    return Err(DiscussionError::IndependentReviewerRequired);
                }
                next.blocking = blocking;
                next.independent_requirement |= blocking && context.independent_reviewer();
            }
        }
        Ok(next)
    }

    /// Outdated content and deleted messages do not appear here intentionally:
    /// neither is evidence that an unresolved blocking requirement was removed.
    pub fn blocks_merge(self) -> bool {
        self.blocking && !self.resolved
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORG: Uuid = Uuid::from_u128(1);
    const AUTHOR: Uuid = Uuid::from_u128(2);
    const REVIEWER: Uuid = Uuid::from_u128(3);
    const OTHER: Uuid = Uuid::from_u128(4);

    fn member(user_id: Uuid, role: Role) -> Membership {
        Membership {
            organization_id: ORG,
            user_id,
            active: true,
            role,
            review_domains: vec![],
        }
    }

    fn resource() -> ResourceAccess {
        ResourceAccess {
            organization_id: ORG,
            repository_access: true,
            assigned_editor: false,
            export_allowed: false,
        }
    }

    fn context<'a>(member: &'a Membership, resource: &'a ResourceAccess) -> DiscussionContext<'a> {
        DiscussionContext {
            member,
            resource,
            proposal_author: AUTHOR,
        }
    }

    #[test]
    fn unicode_comment_limit_preserves_exact_body_and_rejects_empty_or_nul() {
        let exact = "🦀".repeat(MAX_COMMENT_CHARACTERS);
        assert_eq!(CommentBody::new(&exact).unwrap().as_str(), exact);
        assert_eq!(
            CommentBody::new(&format!("{exact}א")),
            Err(DiscussionError::CommentTooLong)
        );
        assert_eq!(
            CommentBody::new(&"a".repeat(MAX_COMMENT_CHARACTERS + 1)),
            Err(DiscussionError::CommentTooLong)
        );
        for empty in ["", " \t\r\n", "\u{2003}"] {
            assert_eq!(CommentBody::new(empty), Err(DiscussionError::EmptyComment));
        }
        assert_eq!(
            CommentBody::new("a\0b"),
            Err(DiscussionError::InvalidComment)
        );
        assert_eq!(
            CommentBody::new("  שָׁלוֹם\r\n🦀 ").unwrap().as_str(),
            "  שָׁלוֹם\r\n🦀 "
        );
    }

    #[test]
    fn any_current_non_reader_can_comment_without_an_editor_assignment() {
        let resource = resource();
        for role in [
            Role::Contributor,
            Role::Reviewer,
            Role::Maintainer,
            Role::Admin,
        ] {
            let member = member(OTHER, role);
            assert!(ThreadState::open(&context(&member, &resource), false).is_ok());
        }
        let reader = member(OTHER, Role::Reader);
        assert_eq!(
            ThreadState::open(&context(&reader, &resource), false),
            Err(DiscussionError::Forbidden)
        );
    }

    #[test]
    fn revoked_foreign_or_inaccessible_members_cannot_mutate_even_their_own_thread() {
        let valid = member(REVIEWER, Role::Reviewer);
        let resource = resource();
        let thread = ThreadState::open(&context(&valid, &resource), true).unwrap();
        let mut inactive = valid.clone();
        inactive.active = false;
        let mut foreign = valid.clone();
        foreign.organization_id = OTHER;
        let inaccessible = ResourceAccess {
            organization_id: ORG,
            repository_access: false,
            assigned_editor: false,
            export_allowed: false,
        };
        let reader = member(REVIEWER, Role::Reader);
        for denied in [
            context(&inactive, &resource),
            context(&foreign, &resource),
            context(&valid, &inaccessible),
            context(&reader, &resource),
        ] {
            assert_eq!(
                denied.authorize_comment_change(REVIEWER),
                Err(DiscussionError::Forbidden)
            );
            for action in [
                ThreadAction::Resolve,
                ThreadAction::Reopen,
                ThreadAction::SetBlocking(false),
            ] {
                assert_eq!(
                    thread.transition(&denied, action),
                    Err(DiscussionError::Forbidden)
                );
            }
        }
    }

    #[test]
    fn author_cannot_resolve_or_unmark_an_independent_blocker_even_as_admin() {
        let resource = resource();
        let reviewer = member(REVIEWER, Role::Reviewer);
        let thread = ThreadState::open(&context(&reviewer, &resource), true).unwrap();
        for role in [Role::Reviewer, Role::Maintainer, Role::Admin] {
            let author = member(AUTHOR, role);
            assert_eq!(
                thread.transition(&context(&author, &resource), ThreadAction::Resolve),
                Err(DiscussionError::IndependentReviewerRequired)
            );
            assert_eq!(
                thread.transition(
                    &context(&author, &resource),
                    ThreadAction::SetBlocking(false)
                ),
                Err(if role == Role::Reviewer {
                    DiscussionError::Forbidden
                } else {
                    DiscussionError::IndependentReviewerRequired
                })
            );
        }
        assert!(thread.blocks_merge());
    }

    #[test]
    fn independent_requirement_survives_role_change_resolution_and_reopening() {
        let resource = resource();
        let reviewer = member(REVIEWER, Role::Reviewer);
        let thread = ThreadState::open(&context(&reviewer, &resource), true).unwrap();
        let former_reviewer = member(REVIEWER, Role::Contributor);
        let resolved = thread
            .transition(&context(&former_reviewer, &resource), ThreadAction::Resolve)
            .unwrap();
        assert!(!resolved.blocks_merge());
        assert!(resolved.independent_requirement);
        let reopened = resolved
            .transition(&context(&former_reviewer, &resource), ThreadAction::Reopen)
            .unwrap();
        assert_eq!(reopened, thread);
        let author = member(AUTHOR, Role::Admin);
        assert_eq!(
            reopened.transition(&context(&author, &resource), ThreadAction::Resolve),
            Err(DiscussionError::IndependentReviewerRequired)
        );
    }

    #[test]
    fn marking_an_author_thread_blocking_records_the_independent_maintainer() {
        let resource = resource();
        let author = member(AUTHOR, Role::Contributor);
        let thread = ThreadState::open(&context(&author, &resource), false).unwrap();
        let maintainer = member(OTHER, Role::Maintainer);
        let blocked = thread
            .transition(
                &context(&maintainer, &resource),
                ThreadAction::SetBlocking(true),
            )
            .unwrap();
        assert!(blocked.blocks_merge());
        assert!(blocked.independent_requirement);
        assert_eq!(
            blocked.transition(&context(&author, &resource), ThreadAction::Resolve),
            Err(DiscussionError::IndependentReviewerRequired)
        );
        assert_eq!(
            blocked.transition(
                &context(&author, &resource),
                ThreadAction::SetBlocking(false)
            ),
            Err(DiscussionError::IndependentReviewerRequired)
        );
    }

    #[test]
    fn independent_reviewer_can_withdraw_blocker_without_erasing_its_origin() {
        let resource = resource();
        let reviewer = member(REVIEWER, Role::Reviewer);
        let thread = ThreadState::open(&context(&reviewer, &resource), true).unwrap();
        let unblocked = thread
            .transition(
                &context(&reviewer, &resource),
                ThreadAction::SetBlocking(false),
            )
            .unwrap();
        assert!(!unblocked.blocks_merge());
        assert!(unblocked.independent_requirement);
        assert!(
            unblocked
                .transition(
                    &context(&reviewer, &resource),
                    ThreadAction::SetBlocking(true)
                )
                .unwrap()
                .blocks_merge()
        );
    }

    #[test]
    fn resolution_and_blocking_have_distinct_authority() {
        let resource = resource();
        let creator = member(OTHER, Role::Contributor);
        let thread = ThreadState::open(&context(&creator, &resource), true).unwrap();
        let stranger = member(AUTHOR, Role::Contributor);
        assert_eq!(
            thread.transition(&context(&stranger, &resource), ThreadAction::Resolve),
            Err(DiscussionError::Forbidden)
        );
        let reviewer = member(REVIEWER, Role::Reviewer);
        assert!(
            thread
                .transition(&context(&reviewer, &resource), ThreadAction::Resolve)
                .unwrap()
                .resolved
        );
        assert_eq!(
            thread.transition(
                &context(&reviewer, &resource),
                ThreadAction::SetBlocking(false)
            ),
            Err(DiscussionError::Forbidden)
        );
        assert!(
            thread
                .transition(&context(&creator, &resource), ThreadAction::Resolve)
                .is_ok()
        );
        assert!(!thread.independent_requirement);
    }

    #[test]
    fn authors_can_resolve_their_own_blockers_and_only_edit_their_own_messages() {
        let resource = resource();
        let author = member(AUTHOR, Role::Reviewer);
        let own_context = context(&author, &resource);
        let thread = ThreadState::open(&own_context, true).unwrap();
        assert!(!thread.independent_requirement);
        assert!(
            !thread
                .transition(&own_context, ThreadAction::Resolve)
                .unwrap()
                .blocks_merge()
        );
        assert!(own_context.authorize_comment_change(AUTHOR).is_ok());
        assert_eq!(
            own_context.authorize_comment_change(REVIEWER),
            Err(DiscussionError::Forbidden)
        );
        let admin = member(OTHER, Role::Admin);
        assert_eq!(
            context(&admin, &resource).authorize_comment_change(AUTHOR),
            Err(DiscussionError::Forbidden)
        );
    }
}
