use super::*;
use specspace_application::discussion::read::*;
type Timestamp = sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>;

#[derive(FromRow)]
struct ReadScope {
    #[sqlx(flatten)]
    scope: Scope,
    write_available: bool,
    target_branch_changed: bool,
}
impl ReadScope {
    fn can_comment(&self, actor: Uuid) -> bool {
        self.write_available && self.scope.policy(actor, |c| c.authorize_comment()).is_ok()
    }
}
async fn read_scope(
    conn: &mut PgConnection,
    actor: Uuid,
    proposal: Uuid,
) -> Result<ReadScope, DiscussionStoreError> {
    sqlx::query_as::<_,ReadScope>("SELECT p.organization_id,p.repository_id,p.author_id,m.role,r.remote_id,r.installation_id,p.branch,p.head_sha,p.base_sha,p.target_sha,p.version,p.state,(r.default_branch='main' AND p.state<>'PROVISIONING' AND p.head_sha IS NOT NULL AND p.base_sha IS NOT NULL AND p.target_sha IS NOT NULL) AS write_available,r.default_branch<>'main' AS target_branch_changed FROM proposals p JOIN repositories r ON r.id=p.repository_id AND r.organization_id=p.organization_id JOIN organizations o ON o.id=p.organization_id JOIN memberships m ON m.organization_id=p.organization_id JOIN users u ON u.id=m.user_id WHERE p.id=$1 AND u.id=$2 AND u.status='ACTIVE' FOR SHARE OF p,r,o,m,u")
        .bind(proposal).bind(actor).fetch_optional(conn).await?.ok_or(DiscussionStoreError::NotFound)
}

#[derive(FromRow)]
struct Header {
    id: Uuid,
    proposal_id: Uuid,
    creator_id: Uuid,
    creator_name: String,
    created_at: Timestamp,
    repository_id: Uuid,
    remote_id: String,
    installation_id: String,
    branch: String,
    snapshot_id: Option<Uuid>,
    fingerprint: Option<String>,
    head_sha: String,
    target_sha: String,
    merge_base_sha: Option<String>,
    proposal_version: i64,
    anchor: Value,
    snapshot_recorded_base_sha: Option<String>,
    version: i64,
    blocking: bool,
    resolved: bool,
    independent_requirement: bool,
    last_activity_at: Timestamp,
    comment_count: i64,
    resolution_kind: Option<String>,
    resolver_id: Option<Uuid>,
    resolver_name: Option<String>,
    resolved_at: Option<Timestamp>,
}
const HEADERS: &str = "SELECT t.id,t.proposal_id,t.creator_id,left(u.display_name,200) AS creator_name,t.created_at,t.repository_id,t.remote_id,t.installation_id,t.branch,t.snapshot_id,t.fingerprint,t.head_sha,t.target_sha,t.merge_base_sha,t.proposal_version,t.anchor,h.recorded_base_sha AS snapshot_recorded_base_sha,r.version,r.blocking,r.resolved,r.independent_requirement,r.created_at AS last_activity_at,c.comment_count,z.kind AS resolution_kind,z.actor_id AS resolver_id,left(v.display_name,200) AS resolver_name,z.created_at AS resolved_at FROM discussion_threads t JOIN users u ON u.id=t.creator_id LEFT JOIN review_snapshots h ON h.id=t.snapshot_id AND h.organization_id=t.organization_id AND h.proposal_id=t.proposal_id JOIN LATERAL (SELECT version,blocking,resolved,independent_requirement,created_at FROM discussion_revisions WHERE organization_id=t.organization_id AND proposal_id=t.proposal_id AND thread_id=t.id ORDER BY version DESC LIMIT 1) r ON true JOIN LATERAL (SELECT count(*) AS comment_count FROM discussion_revisions WHERE organization_id=t.organization_id AND proposal_id=t.proposal_id AND thread_id=t.id AND kind IN ('OPEN','REPLY')) c ON true LEFT JOIN LATERAL (SELECT kind,actor_id,created_at FROM discussion_revisions WHERE organization_id=t.organization_id AND proposal_id=t.proposal_id AND thread_id=t.id AND kind IN ('RESOLVE','REOPEN') ORDER BY version DESC LIMIT 1) z ON true LEFT JOIN users v ON v.id=z.actor_id WHERE t.organization_id=$1 AND t.proposal_id=$2";

impl Header {
    fn dto(
        self,
        access: &ReadScope,
        actor: Uuid,
    ) -> Result<DiscussionThread, DiscussionStoreError> {
        let scope = &access.scope;
        let state = ThreadState {
            creator: self.creator_id,
            blocking: self.blocking,
            resolved: self.resolved,
            independent_requirement: self.independent_requirement,
        };
        let can_comment = access.can_comment(actor);
        let can_resolve = can_comment
            && !state.resolved
            && scope
                .policy(actor, |c| state.transition(c, ThreadAction::Resolve))
                .is_ok();
        let can_reopen = can_comment
            && state.resolved
            && scope
                .policy(actor, |c| state.transition(c, ThreadAction::Reopen))
                .is_ok();
        let can_toggle_blocking = can_comment
            && scope
                .policy(actor, |c| {
                    state.transition(c, ThreadAction::SetBlocking(!state.blocking))
                })
                .is_ok();
        let binding_changed = self.repository_id != scope.repository_id
            || self.remote_id != scope.remote_id
            || self.installation_id != scope.installation_id
            || self.branch != scope.branch
            || access.target_branch_changed;
        let outdated_context = binding_changed
            || Some(&self.head_sha) != scope.head_sha.as_ref()
            || Some(&self.target_sha) != scope.target_sha.as_ref()
            || self.snapshot_id.is_some()
                && self.snapshot_recorded_base_sha.as_ref() != scope.base_sha.as_ref();
        let resolution = if self.resolved {
            if self.resolution_kind.as_deref() != Some("RESOLVE") {
                return Err(DiscussionStoreError::Unavailable);
            }
            Some(DiscussionResolution {
                actor: DiscussionActor {
                    id: self.resolver_id.ok_or(DiscussionStoreError::Unavailable)?,
                    name: self
                        .resolver_name
                        .ok_or(DiscussionStoreError::Unavailable)?,
                },
                recorded_at: self
                    .resolved_at
                    .ok_or(DiscussionStoreError::Unavailable)?
                    .to_rfc3339(),
            })
        } else {
            None
        };
        Ok(DiscussionThread {
            id: self.id,
            proposal_id: self.proposal_id,
            creator: DiscussionActor {
                id: self.creator_id,
                name: self.creator_name,
            },
            created_at: self.created_at.to_rfc3339(),
            origin: DiscussionOrigin {
                repository_id: self.repository_id,
                branch: self.branch,
                snapshot_id: self.snapshot_id,
                fingerprint: self.fingerprint,
                head_sha: self.head_sha,
                target_sha: self.target_sha,
                merge_base_sha: self.merge_base_sha,
                proposal_version: self.proposal_version,
                anchor: serde_json::from_value(self.anchor)
                    .map_err(|_| DiscussionStoreError::Unavailable)?,
            },
            version: self.version,
            blocking: self.blocking,
            resolved: self.resolved,
            independent_requirement: self.independent_requirement,
            resolution,
            last_activity_at: self.last_activity_at.to_rfc3339(),
            comment_count: self.comment_count,
            outdated_context,
            repository_binding_changed: binding_changed,
            permissions: DiscussionPermissions {
                can_comment,
                can_resolve,
                can_reopen,
                can_toggle_blocking,
            },
        })
    }
}

#[derive(FromRow)]
struct Comment {
    comment_id: Uuid,
    comment_author_id: Uuid,
    author_name: String,
    comment_version: i64,
    created_at: Timestamp,
    changed_at: Timestamp,
    kind: String,
    body: Option<String>,
    reply_to_id: Option<Uuid>,
}
impl Comment {
    fn dto(
        self,
        access: &ReadScope,
        actor: Uuid,
    ) -> Result<DiscussionComment, DiscussionStoreError> {
        let tombstoned = self.kind == "TOMBSTONE";
        if tombstoned != self.body.is_none() {
            return Err(DiscussionStoreError::Unavailable);
        }
        let can_change = !tombstoned
            && access.write_available
            && access
                .scope
                .policy(actor, |c| {
                    c.authorize_comment_change(self.comment_author_id)
                })
                .is_ok();
        Ok(DiscussionComment {
            id: self.comment_id,
            author: DiscussionActor {
                id: self.comment_author_id,
                name: self.author_name,
            },
            version: self.comment_version,
            created_at: self.created_at.to_rfc3339(),
            edited_at: (self.kind == "EDIT").then(|| self.changed_at.to_rfc3339()),
            tombstoned_at: tombstoned.then(|| self.changed_at.to_rfc3339()),
            reply_to_id: self.reply_to_id,
            body: self.body,
            can_edit: can_change,
            can_tombstone: can_change,
        })
    }
}

impl PgDiscussions {
    pub async fn threads(
        &self,
        actor: Uuid,
        proposal: Uuid,
        after: Option<Uuid>,
    ) -> Result<DiscussionThreads, DiscussionStoreError> {
        if actor.is_nil() || proposal.is_nil() || after.is_some_and(|id| id.is_nil()) {
            return Err(DiscussionStoreError::Invalid);
        }
        let mut tx = self.transaction().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
            .execute(&mut *tx)
            .await?;
        let access = read_scope(&mut tx, actor, proposal).await?;
        let org = access.scope.organization_id;
        let before = if let Some(id) = after {
            sqlx::query_scalar::<_,i64>("SELECT sequence FROM discussion_threads WHERE organization_id=$1 AND proposal_id=$2 AND id=$3").bind(org).bind(proposal).bind(id).fetch_optional(&mut *tx).await?.ok_or(DiscussionStoreError::Invalid)?
        } else {
            i64::MAX
        };
        let query = format!("{HEADERS} AND t.sequence<$3 ORDER BY t.sequence DESC LIMIT 21");
        let mut headers = sqlx::query_as::<_, Header>(&query)
            .bind(org)
            .bind(proposal)
            .bind(before)
            .fetch_all(&mut *tx)
            .await?;
        let more = headers.len() > 20;
        headers.truncate(20);
        let next_after = if more {
            headers.last().map(|r| r.id)
        } else {
            None
        };
        let unresolved_blockers:i64=sqlx::query_scalar("SELECT count(*) FROM discussion_threads t JOIN LATERAL (SELECT blocking,resolved FROM discussion_revisions WHERE organization_id=t.organization_id AND proposal_id=t.proposal_id AND thread_id=t.id ORDER BY version DESC LIMIT 1) r ON true WHERE t.organization_id=$1 AND t.proposal_id=$2 AND r.blocking AND NOT r.resolved").bind(org).bind(proposal).fetch_one(&mut *tx).await?;
        let threads = headers
            .into_iter()
            .map(|r| r.dto(&access, actor))
            .collect::<Result<Vec<_>, _>>()?;
        let now: Timestamp = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(DiscussionThreads {
            proposal_id: proposal,
            actor_id: actor,
            proposal_version: access.scope.version,
            recorded_head_sha: access.scope.head_sha.clone(),
            recorded_target_sha: access.scope.target_sha.clone(),
            can_comment: access.can_comment(actor),
            unresolved_blockers,
            read_at: now.to_rfc3339(),
            threads,
            next_after,
        })
    }

    pub async fn comments(
        &self,
        actor: Uuid,
        proposal: Uuid,
        thread: Uuid,
        after: Option<Uuid>,
        expected_thread_version: Option<i64>,
    ) -> Result<DiscussionComments, DiscussionStoreError> {
        if [actor, proposal, thread].iter().any(Uuid::is_nil)
            || after.is_some_and(|id| id.is_nil())
            || expected_thread_version.is_some_and(|v| v < 1)
            || after.is_some() && expected_thread_version.is_none()
        {
            return Err(DiscussionStoreError::Invalid);
        }
        let mut tx = self.transaction().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
            .execute(&mut *tx)
            .await?;
        let access = read_scope(&mut tx, actor, proposal).await?;
        let org = access.scope.organization_id;
        let query = format!("{HEADERS} AND t.id=$3");
        let header = sqlx::query_as::<_, Header>(&query)
            .bind(org)
            .bind(proposal)
            .bind(thread)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(DiscussionStoreError::NotFound)?;
        if expected_thread_version.is_some_and(|v| v != header.version) {
            return Err(DiscussionStoreError::Stale);
        }
        let after_version = if let Some(id) = after {
            sqlx::query_scalar::<_,i64>("SELECT version FROM discussion_revisions WHERE organization_id=$1 AND proposal_id=$2 AND thread_id=$3 AND comment_id=$4 AND kind IN ('OPEN','REPLY')").bind(org).bind(proposal).bind(thread).bind(id).fetch_optional(&mut *tx).await?.ok_or(DiscussionStoreError::Invalid)?
        } else {
            0
        };
        let mut rows=sqlx::query_as::<_,Comment>("SELECT i.comment_id,i.comment_author_id,left(u.display_name,200) AS author_name,r.comment_version,i.created_at,r.created_at AS changed_at,r.kind,r.body,r.reply_to_id FROM (SELECT comment_id,comment_author_id,created_at,version FROM discussion_revisions WHERE organization_id=$1 AND proposal_id=$2 AND thread_id=$3 AND kind IN ('OPEN','REPLY') AND version>$4 ORDER BY version LIMIT 21) i JOIN LATERAL (SELECT comment_version,created_at,kind,body,reply_to_id FROM discussion_revisions WHERE organization_id=$1 AND proposal_id=$2 AND thread_id=$3 AND comment_id=i.comment_id ORDER BY comment_version DESC LIMIT 1) r ON true JOIN users u ON u.id=i.comment_author_id ORDER BY i.version")
            .bind(org).bind(proposal).bind(thread).bind(after_version).fetch_all(&mut *tx).await?;
        let more = rows.len() > 20;
        rows.truncate(20);
        let next_after = if more {
            rows.last().map(|r| r.comment_id)
        } else {
            None
        };
        let comments = rows
            .into_iter()
            .map(|r| r.dto(&access, actor))
            .collect::<Result<Vec<_>, _>>()?;
        let thread = header.dto(&access, actor)?;
        let now: Timestamp = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(DiscussionComments {
            proposal_id: proposal,
            actor_id: actor,
            thread,
            comments,
            next_after,
            read_at: now.to_rfc3339(),
        })
    }
}
