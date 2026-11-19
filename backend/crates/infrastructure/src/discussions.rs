//! Versioned, audited discussion mutations. Admission commits before immutable
//! source resolution; completion loads and locks current authority again.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use specspace_application::{
    comparison_file::discussion::{AnchorSide, TextSelection, VerifiedFileAnchor},
    discussion::{
        DiscussionRecorded, OpenThreadRequest, SnapshotAnchor, ThreadMutation,
        ThreadMutationRequest, ThreadOrigin,
    },
};
use specspace_domain::{
    discussion::{DiscussionContext, DiscussionError, ThreadAction, ThreadState},
    policy::{Membership, ResourceAccess, Role},
};
use sqlx::{FromRow, PgConnection, PgPool, Postgres, Transaction};
use uuid::Uuid;
mod history;
mod source;

#[derive(Debug, PartialEq, Eq)]
pub enum DiscussionStoreError {
    NotFound,
    Forbidden,
    IndependentReviewerRequired,
    Invalid,
    NotReady,
    Stale,
    Tombstoned,
    IdempotencyMismatch,
    Quota,
    ProviderRateLimited(u32),
    SourceUnavailable,
    InvalidSource,
    Capacity,
    Unavailable,
}
impl From<sqlx::Error> for DiscussionStoreError {
    fn from(error: sqlx::Error) -> Self {
        match error.as_database_error().map(|e| e.message()) {
            Some("discussion quota exceeded") => Self::Quota,
            Some("discussion request mismatch") => Self::IdempotencyMismatch,
            _ => Self::Unavailable,
        }
    }
}
impl From<DiscussionError> for DiscussionStoreError {
    fn from(error: DiscussionError) -> Self {
        match error {
            DiscussionError::Forbidden => Self::Forbidden,
            DiscussionError::IndependentReviewerRequired => Self::IndependentReviewerRequired,
            _ => Self::Invalid,
        }
    }
}

#[derive(FromRow, PartialEq, Eq)]
struct Scope {
    organization_id: Uuid,
    repository_id: Uuid,
    author_id: Uuid,
    role: String,
    remote_id: String,
    installation_id: String,
    branch: String,
    head_sha: Option<String>,
    base_sha: Option<String>,
    target_sha: Option<String>,
    version: i64,
    state: String,
}
impl Scope {
    fn allows_blocker_change(&self, previous: Option<ThreadState>, next: ThreadState) -> bool {
        self.state != "MERGING"
            || !next.blocks_merge()
            || previous.is_some_and(ThreadState::blocks_merge)
    }
    fn policy<T>(
        &self,
        actor: Uuid,
        apply: impl FnOnce(&DiscussionContext<'_>) -> Result<T, DiscussionError>,
    ) -> Result<T, DiscussionStoreError> {
        let member = Membership {
            organization_id: self.organization_id,
            user_id: actor,
            active: true,
            role: match self.role.as_str() {
                "READER" => Role::Reader,
                "CONTRIBUTOR" => Role::Contributor,
                "REVIEWER" => Role::Reviewer,
                "MAINTAINER" => Role::Maintainer,
                "ADMIN" => Role::Admin,
                _ => return Err(DiscussionStoreError::Unavailable),
            },
            review_domains: vec![],
        };
        let resource = ResourceAccess {
            organization_id: self.organization_id,
            repository_access: true,
            assigned_editor: false,
            export_allowed: false,
        };
        Ok(apply(&DiscussionContext {
            member: &member,
            resource: &resource,
            proposal_author: self.author_id,
        })?)
    }
}
async fn scope(
    conn: &mut PgConnection,
    actor: Uuid,
    proposal: Uuid,
) -> Result<Scope, DiscussionStoreError> {
    let row=sqlx::query_as::<_,Scope>("SELECT p.organization_id,p.repository_id,p.author_id,m.role,r.remote_id,r.installation_id,p.branch,p.head_sha,p.base_sha,p.target_sha,p.version,p.state FROM proposals p JOIN repositories r ON r.id=p.repository_id AND r.organization_id=p.organization_id JOIN organizations o ON o.id=p.organization_id JOIN memberships m ON m.organization_id=p.organization_id JOIN users u ON u.id=m.user_id WHERE p.id=$1 AND u.id=$2 AND u.status='ACTIVE' AND r.provider='GITHUB' AND r.default_branch='main' FOR UPDATE OF p FOR SHARE OF r,o,m,u")
        .bind(proposal).bind(actor).fetch_optional(conn).await?.ok_or(DiscussionStoreError::NotFound)?;
    row.policy(actor, |c| c.authorize_comment())?;
    if row.state == "PROVISIONING"
        || row.head_sha.is_none()
        || row.target_sha.is_none()
        || row.base_sha.is_none()
    {
        return Err(DiscussionStoreError::NotReady);
    }
    Ok(row)
}

#[derive(FromRow, PartialEq, Eq)]
struct Snapshot {
    id: Uuid,
    fingerprint: String,
    repository_id: Uuid,
    remote_id: String,
    installation_id: String,
    branch: String,
    head_sha: String,
    target_sha: String,
    merge_base_sha: String,
}
async fn origin(
    conn: &mut PgConnection,
    scope: &Scope,
    proposal: Uuid,
    request: &OpenThreadRequest,
) -> Result<Option<Snapshot>, DiscussionStoreError> {
    match &request.origin {
        ThreadOrigin::Proposal {
            expected_proposal_version,
        } => {
            if *expected_proposal_version != scope.version {
                return Err(DiscussionStoreError::Stale);
            }
            Ok(None)
        }
        ThreadOrigin::Snapshot {
            snapshot_id,
            expected_fingerprint,
            ..
        } => {
            let row=sqlx::query_as::<_,Snapshot>("SELECT id,fingerprint,repository_id,remote_id,installation_id,branch,head_sha,target_sha,merge_base_sha FROM review_snapshots WHERE organization_id=$1 AND proposal_id=$2 AND id=$3 AND state='READY' FOR SHARE")
                .bind(scope.organization_id).bind(proposal).bind(snapshot_id).fetch_optional(conn).await?.ok_or(DiscussionStoreError::NotFound)?;
            if row.fingerprint != *expected_fingerprint
                || row.repository_id != scope.repository_id
                || row.remote_id != scope.remote_id
                || row.installation_id != scope.installation_id
                || row.branch != scope.branch
            {
                return Err(DiscussionStoreError::Stale);
            }
            Ok(Some(row))
        }
    }
}

struct Identity {
    organization: Uuid,
    proposal: Uuid,
    actor: Uuid,
    request: Uuid,
    hash: String,
    attempt: Uuid,
}
#[derive(FromRow)]
struct Recorded {
    id: Uuid,
    proposal_id: Uuid,
    thread_id: Uuid,
    version: i64,
    actor_id: Uuid,
    request_id: Uuid,
    request_hash: String,
    comment_id: Option<Uuid>,
    comment_version: Option<i64>,
    created_at: sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>,
}
impl Recorded {
    fn dto(self, replayed: bool) -> DiscussionRecorded {
        DiscussionRecorded {
            id: self.id,
            request_id: self.request_id,
            actor_id: self.actor_id,
            proposal_id: self.proposal_id,
            thread_id: self.thread_id,
            thread_version: self.version,
            comment_id: self.comment_id,
            comment_version: self.comment_version,
            recorded_at: self.created_at.to_rfc3339(),
            replayed,
        }
    }
}
async fn replay(
    conn: &mut PgConnection,
    identity: &Identity,
) -> Result<Option<DiscussionRecorded>, DiscussionStoreError> {
    let old = sqlx::query_as::<_, Recorded>(
        "SELECT * FROM discussion_revisions WHERE organization_id=$1 AND request_id=$2",
    )
    .bind(identity.organization)
    .bind(identity.request)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some(old) = old {
        if old.actor_id != identity.actor
            || old.proposal_id != identity.proposal
            || old.request_hash != identity.hash
        {
            return Err(DiscussionStoreError::IdempotencyMismatch);
        }
        return Ok(Some(old.dto(true)));
    }
    let mismatch: bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM discussion_attempts WHERE organization_id=$1 AND request_id=$2 AND (actor_id<>$3 OR proposal_id<>$4 OR request_hash<>$5))")
        .bind(identity.organization).bind(identity.request).bind(identity.actor).bind(identity.proposal).bind(&identity.hash).fetch_one(conn).await?;
    if mismatch {
        return Err(DiscussionStoreError::IdempotencyMismatch);
    }
    Ok(None)
}
async fn admit(conn: &mut PgConnection, identity: &Identity) -> Result<(), DiscussionStoreError> {
    sqlx::query("INSERT INTO discussion_attempts(id,organization_id,proposal_id,actor_id,request_id,request_hash) VALUES($1,$2,$3,$4,$5,$6)")
        .bind(identity.attempt).bind(identity.organization).bind(identity.proposal).bind(identity.actor).bind(identity.request).bind(&identity.hash).execute(conn).await?;
    Ok(())
}
fn hash(value: Value) -> String {
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&value).expect("bounded discussion intent"))
    )
}

/// This capability is created only after current write authorization and durable
/// admission. It carries no database connection across provider/CPU awaits.
pub struct OpenAdmission {
    scope: Scope,
    snapshot: Option<Snapshot>,
    identity: Identity,
    request: OpenThreadRequest,
}
impl OpenAdmission {
    pub fn actor(&self) -> Uuid {
        self.identity.actor
    }
    pub fn proposal(&self) -> Uuid {
        self.identity.proposal
    }
    pub fn request(&self) -> &OpenThreadRequest {
        &self.request
    }
}
pub enum OpenAdmissionResult {
    Ready(Box<OpenAdmission>),
    Recorded(DiscussionRecorded),
}
pub struct PgDiscussions {
    pool: PgPool,
}
impl PgDiscussions {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
    async fn transaction(&self) -> Result<Transaction<'_, Postgres>, DiscussionStoreError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SET LOCAL lock_timeout='5s'")
            .execute(&mut *tx)
            .await?;
        sqlx::query("SET LOCAL statement_timeout='10s'")
            .execute(&mut *tx)
            .await?;
        Ok(tx)
    }
    pub async fn admit_open(
        &self,
        actor: Uuid,
        proposal: Uuid,
        request: OpenThreadRequest,
    ) -> Result<OpenAdmissionResult, DiscussionStoreError> {
        request
            .validate()
            .map_err(|_| DiscussionStoreError::Invalid)?;
        if actor.is_nil() || proposal.is_nil() {
            return Err(DiscussionStoreError::Invalid);
        }
        let mut tx = self.transaction().await?;
        let scope = scope(&mut tx, actor, proposal).await?;
        let identity = Identity {
            organization: scope.organization_id,
            proposal,
            actor,
            request: request.request_id,
            hash: hash(json!(["discussion-open-v1", actor, proposal, request])),
            attempt: Uuid::new_v4(),
        };
        if let Some(old) = replay(&mut tx, &identity).await? {
            tx.commit().await?;
            return Ok(OpenAdmissionResult::Recorded(old));
        }
        let snapshot = origin(&mut tx, &scope, proposal, &request).await?;
        let initial = scope.policy(actor, |c| ThreadState::open(c, request.blocking))?;
        if !scope.allows_blocker_change(None, initial) {
            return Err(DiscussionStoreError::NotReady);
        }
        admit(&mut tx, &identity).await?;
        tx.commit().await?;
        Ok(OpenAdmissionResult::Ready(Box::new(OpenAdmission {
            scope,
            snapshot,
            identity,
            request,
        })))
    }
    pub async fn complete_open(
        &self,
        admission: &OpenAdmission,
        proof: Option<&VerifiedFileAnchor>,
    ) -> Result<DiscussionRecorded, DiscussionStoreError> {
        let identity = &admission.identity;
        let mut tx = self.transaction().await?;
        let current = scope(&mut tx, identity.actor, identity.proposal).await?;
        if let Some(old) = replay(&mut tx, identity).await? {
            tx.commit().await?;
            return Ok(old);
        }
        if current != admission.scope
            || origin(&mut tx, &current, identity.proposal, &admission.request).await?
                != admission.snapshot
        {
            return Err(DiscussionStoreError::Stale);
        }
        let anchor = verified_anchor(admission, proof)?;
        let state = current.policy(identity.actor, |c| {
            ThreadState::open(c, admission.request.blocking)
        })?;
        if !current.allows_blocker_change(None, state) {
            return Err(DiscussionStoreError::NotReady);
        }
        let revision = NewRevision {
            id: Uuid::new_v4(),
            thread: Uuid::new_v4(),
            version: 1,
            kind: "OPEN",
            state,
            comment: Some(Uuid::new_v4()),
            comment_version: Some(1),
            comment_author: Some(identity.actor),
            reply_to: None,
            body: Some(admission.request.body.clone()),
        };
        let snapshot = &admission.snapshot;
        sqlx::query("INSERT INTO discussion_threads(id,organization_id,proposal_id,creator_id,snapshot_id,fingerprint,repository_id,remote_id,installation_id,branch,head_sha,target_sha,merge_base_sha,proposal_version,anchor,opening_revision) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16)")
            .bind(revision.thread).bind(identity.organization).bind(identity.proposal).bind(identity.actor).bind(snapshot.as_ref().map(|h|h.id)).bind(snapshot.as_ref().map(|h|&h.fingerprint))
            .bind(current.repository_id).bind(&current.remote_id).bind(&current.installation_id).bind(&current.branch)
            .bind(snapshot.as_ref().map(|h|&h.head_sha).or(current.head_sha.as_ref())).bind(snapshot.as_ref().map(|h|&h.target_sha).or(current.target_sha.as_ref())).bind(snapshot.as_ref().map(|h|&h.merge_base_sha))
            .bind(current.version).bind(anchor).bind(revision.id).execute(&mut *tx).await?;
        let result = append(&mut tx, identity, &revision).await?;
        tx.commit().await?;
        Ok(result)
    }

    pub async fn mutate(
        &self,
        actor: Uuid,
        proposal: Uuid,
        thread: Uuid,
        request: ThreadMutationRequest,
    ) -> Result<DiscussionRecorded, DiscussionStoreError> {
        request
            .validate()
            .map_err(|_| DiscussionStoreError::Invalid)?;
        if [actor, proposal, thread].iter().any(Uuid::is_nil) {
            return Err(DiscussionStoreError::Invalid);
        }
        let mut tx = self.transaction().await?;
        let scope = scope(&mut tx, actor, proposal).await?;
        let identity = Identity {
            organization: scope.organization_id,
            proposal,
            actor,
            request: request.request_id,
            hash: hash(json!([
                "discussion-mutate-v1",
                actor,
                proposal,
                thread,
                request
            ])),
            attempt: Uuid::new_v4(),
        };
        if let Some(old) = replay(&mut tx, &identity).await? {
            tx.commit().await?;
            return Ok(old);
        }
        let row=sqlx::query_as::<_,Thread>("SELECT t.creator_id,r.version,r.blocking,r.resolved,r.independent_requirement FROM discussion_threads t JOIN LATERAL (SELECT version,blocking,resolved,independent_requirement FROM discussion_revisions WHERE thread_id=t.id ORDER BY version DESC LIMIT 1) r ON true WHERE t.organization_id=$1 AND t.proposal_id=$2 AND t.id=$3")
            .bind(scope.organization_id).bind(proposal).bind(thread).fetch_optional(&mut *tx).await?.ok_or(DiscussionStoreError::NotFound)?;
        if row.version != request.expected_thread_version {
            return Err(DiscussionStoreError::Stale);
        }
        let mut revision = NewRevision {
            id: Uuid::new_v4(),
            thread,
            version: row.version + 1,
            kind: "REPLY",
            state: ThreadState {
                creator: row.creator_id,
                blocking: row.blocking,
                resolved: row.resolved,
                independent_requirement: row.independent_requirement,
            },
            comment: None,
            comment_version: None,
            comment_author: None,
            reply_to: None,
            body: None,
        };
        let previous_state = revision.state;
        match &request.mutation {
            ThreadMutation::Reply { body, reply_to_id } => {
                if let Some(reply) = reply_to_id {
                    let exists: bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM discussion_revisions WHERE thread_id=$1 AND comment_id=$2 AND kind IN ('OPEN','REPLY'))").bind(thread).bind(reply).fetch_one(&mut *tx).await?;
                    if !exists {
                        return Err(DiscussionStoreError::NotFound);
                    }
                }
                revision.comment = Some(Uuid::new_v4());
                revision.comment_version = Some(1);
                revision.comment_author = Some(actor);
                revision.reply_to = *reply_to_id;
                revision.body = Some(body.clone());
            }
            ThreadMutation::Edit {
                comment_id,
                expected_comment_version,
                ..
            }
            | ThreadMutation::Tombstone {
                comment_id,
                expected_comment_version,
            } => {
                let message=sqlx::query_as::<_,Message>("SELECT kind,comment_author_id,comment_version,reply_to_id FROM discussion_revisions WHERE thread_id=$1 AND comment_id=$2 ORDER BY comment_version DESC LIMIT 1")
                    .bind(thread).bind(comment_id).fetch_optional(&mut *tx).await?.ok_or(DiscussionStoreError::NotFound)?;
                scope.policy(actor, |c| {
                    c.authorize_comment_change(message.comment_author_id)
                })?;
                if message.kind == "TOMBSTONE" {
                    return Err(DiscussionStoreError::Tombstoned);
                }
                if message.comment_version != *expected_comment_version {
                    return Err(DiscussionStoreError::Stale);
                }
                revision.comment = Some(*comment_id);
                revision.comment_version = Some(message.comment_version + 1);
                revision.comment_author = Some(message.comment_author_id);
                revision.reply_to = message.reply_to_id;
                if let ThreadMutation::Edit { body, .. } = &request.mutation {
                    revision.kind = "EDIT";
                    revision.body = Some(body.clone());
                } else {
                    revision.kind = "TOMBSTONE";
                }
            }
            ThreadMutation::Resolve {} => {
                revision.kind = "RESOLVE";
                revision.state = scope.policy(actor, |c| {
                    revision.state.transition(c, ThreadAction::Resolve)
                })?;
            }
            ThreadMutation::Reopen {} => {
                revision.kind = "REOPEN";
                revision.state = scope.policy(actor, |c| {
                    revision.state.transition(c, ThreadAction::Reopen)
                })?;
            }
            ThreadMutation::SetBlocking { blocking } => {
                revision.kind = "SET_BLOCKING";
                revision.state = scope.policy(actor, |c| {
                    revision
                        .state
                        .transition(c, ThreadAction::SetBlocking(*blocking))
                })?;
            }
        }
        if !scope.allows_blocker_change(Some(previous_state), revision.state) {
            return Err(DiscussionStoreError::NotReady);
        }
        admit(&mut tx, &identity).await?;
        let result = append(&mut tx, &identity, &revision).await?;
        tx.commit().await?;
        Ok(result)
    }
}

#[derive(FromRow)]
struct Thread {
    creator_id: Uuid,
    version: i64,
    blocking: bool,
    resolved: bool,
    independent_requirement: bool,
}
#[derive(FromRow)]
struct Message {
    kind: String,
    comment_author_id: Uuid,
    comment_version: i64,
    reply_to_id: Option<Uuid>,
}
struct NewRevision {
    id: Uuid,
    thread: Uuid,
    version: i64,
    kind: &'static str,
    state: ThreadState,
    comment: Option<Uuid>,
    comment_version: Option<i64>,
    comment_author: Option<Uuid>,
    reply_to: Option<Uuid>,
    body: Option<String>,
}
async fn append(
    conn: &mut PgConnection,
    identity: &Identity,
    revision: &NewRevision,
) -> Result<DiscussionRecorded, DiscussionStoreError> {
    let (audit, event) = (Uuid::new_v4(), Uuid::new_v4());
    let recorded=sqlx::query_as::<_,Recorded>("INSERT INTO discussion_revisions(id,organization_id,proposal_id,thread_id,version,actor_id,request_id,request_hash,attempt_id,kind,blocking,resolved,independent_requirement,comment_id,comment_version,comment_author_id,reply_to_id,body,audit_id,event_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20) RETURNING *")
        .bind(revision.id).bind(identity.organization).bind(identity.proposal).bind(revision.thread).bind(revision.version).bind(identity.actor).bind(identity.request).bind(&identity.hash).bind(identity.attempt)
        .bind(revision.kind).bind(revision.state.blocking).bind(revision.state.resolved).bind(revision.state.independent_requirement).bind(revision.comment).bind(revision.comment_version).bind(revision.comment_author).bind(revision.reply_to).bind(&revision.body).bind(audit).bind(event).fetch_one(&mut *conn).await?;
    sqlx::query("INSERT INTO audit_events(id,organization_id,actor_id,action,entity_id,after_ref,result) VALUES($1,$2,$3,$4,$5,$6,'RECORDED')")
        .bind(audit).bind(identity.organization).bind(identity.actor).bind(format!("DISCUSSION_{}",revision.kind)).bind(revision.thread).bind(revision.id.to_string()).execute(&mut *conn).await?;
    sqlx::query("INSERT INTO outbox(id,organization_id,event_type,entity_id,payload) VALUES($1,$2,'DISCUSSION_CHANGED',$3,$4)")
        .bind(event).bind(identity.organization).bind(identity.proposal).bind(json!({"thread_id":revision.thread,"revision_id":revision.id,"version":revision.version,"kind":revision.kind})).execute(conn).await?;
    Ok(recorded.dto(false))
}

fn verified_anchor(
    admission: &OpenAdmission,
    proof: Option<&VerifiedFileAnchor>,
) -> Result<Value, DiscussionStoreError> {
    let expected = match &admission.request.origin {
        ThreadOrigin::Snapshot {
            anchor:
                SnapshotAnchor::File {
                    file_id,
                    side,
                    selection,
                },
            ..
        } => Some((file_id, side, selection)),
        _ => None,
    };
    let Some((file_id, side, selection)) = expected else {
        return if proof.is_none() {
            Ok(json!({"kind":"proposal"}))
        } else {
            Err(DiscussionStoreError::Invalid)
        };
    };
    let proof = proof.ok_or(DiscussionStoreError::Invalid)?;
    let scope = &admission.scope;
    let snapshot = admission
        .snapshot
        .as_ref()
        .ok_or(DiscussionStoreError::Invalid)?;
    let ctx = proof.context();
    if ctx.scope.actor != admission.identity.actor
        || ctx.scope.organization != scope.organization_id
        || ctx.scope.proposal != admission.identity.proposal
        || ctx.scope.connection.repository != scope.repository_id
        || ctx.scope.connection.remote_id.to_string() != scope.remote_id
        || ctx.scope.connection.installation_id.to_string() != scope.installation_id
        || ctx.scope.branch.as_str() != scope.branch
        || ctx.scope.version != scope.version
        || ctx.scope.state != scope.state
        || Some(ctx.scope.recorded_head.as_str()) != scope.head_sha.as_deref()
        || Some(ctx.scope.recorded_base.as_str()) != scope.base_sha.as_deref()
        || ctx.snapshot != snapshot.id
        || ctx.head.as_str() != snapshot.head_sha
        || ctx.merge_base.as_str() != snapshot.merge_base_sha
        || ctx.change.id != *file_id
        || proof.side() != *side
    {
        return Err(DiscussionStoreError::Stale);
    }
    use specspace_document_engine::discussion_anchor::Selection;
    let matches = match (selection, proof.text().selection()) {
        (TextSelection::Document {}, Selection::Document { .. }) => true,
        (
            TextSelection::Section {
                section_id: expected,
            },
            Selection::Section { section_id, .. },
        ) => expected == section_id,
        (
            TextSelection::Lines {
                start_line,
                end_line,
            },
            Selection::Lines,
        ) => {
            proof.text().line_range().start == *start_line
                && proof.text().line_range().end.checked_sub(1) == Some(*end_line)
        }
        _ => false,
    };
    if !matches {
        return Err(DiscussionStoreError::Invalid);
    }
    let side = match side {
        AnchorSide::Before => "before",
        AnchorSide::After => "after",
    };
    Ok(
        json!({"kind":"file","file_id":file_id,"side":side,"source":proof.source(),"text":proof.text()}),
    )
}
