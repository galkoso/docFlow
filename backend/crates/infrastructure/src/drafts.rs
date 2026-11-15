//! User-private recovery only. No provider calls, Git publication or review
//! mutation occurs here; even stale/closed buffers remain distinguishable from Git.
use specspace_application::draft::{
    DraftBuffer, DraftContext, DraftError, DraftRetention, DraftSaved, DraftSnapshot,
    MAX_RECOVERY_BYTES, MAX_RECOVERY_DOCUMENTS, ValidatedDraft,
};
use sqlx::{FromRow, PgPool, Postgres, Transaction};
use uuid::Uuid;

#[derive(FromRow)]
struct BufferRow {
    pub document_id: String,
    pub base_head_sha: String,
    pub version: i64,
    pub updated_at: String,
    pub expires_at: String,
    pub content_utf8: Option<String>,
    pub base_is_current: bool,
}
impl From<BufferRow> for DraftBuffer {
    fn from(row: BufferRow) -> Self {
        Self {
            document_id: row.document_id,
            base_head_sha: row.base_head_sha,
            version: row.version,
            updated_at: row.updated_at,
            expires_at: row.expires_at,
            content_utf8: row.content_utf8,
            base_is_current: row.base_is_current,
        }
    }
}

#[derive(FromRow)]
struct Scope {
    organization_id: Uuid,
    owner_id: Uuid,
    state: String,
    head_sha: Option<String>,
    version: i64,
    role: String,
}
#[derive(FromRow)]
struct Existing {
    version: i64,
    live: bool,
    last_save_id: Option<Uuid>,
    last_request_hash: Option<String>,
}

pub struct PgDrafts {
    pool: PgPool,
    retention: Option<DraftRetention>,
}
impl PgDrafts {
    pub fn new(pool: PgPool, retention: Option<DraftRetention>) -> Self {
        Self { pool, retention }
    }

    async fn transaction(&self) -> Result<Transaction<'_, Postgres>, DraftError> {
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        sqlx::query("SET LOCAL lock_timeout='2s'")
            .execute(&mut *tx)
            .await
            .map_err(unavailable)?;
        sqlx::query("SET LOCAL statement_timeout='5s'")
            .execute(&mut *tx)
            .await
            .map_err(unavailable)?;
        Ok(tx)
    }

    async fn scope(
        tx: &mut Transaction<'_, Postgres>,
        actor: Uuid,
        proposal: Uuid,
        write: bool,
    ) -> Result<Scope, DraftError> {
        let lock = if write {
            "FOR UPDATE OF p FOR SHARE OF m,u"
        } else {
            "FOR SHARE OF p,m,u"
        };
        sqlx::query_as(&format!("SELECT p.organization_id,p.owner_id,p.state,p.head_sha,p.version,m.role FROM proposals p JOIN repositories r ON r.id=p.repository_id AND r.organization_id=p.organization_id JOIN memberships m ON m.organization_id=p.organization_id JOIN users u ON u.id=m.user_id WHERE p.id=$1 AND u.id=$2 AND u.status='ACTIVE' {lock}"))
            .bind(proposal).bind(actor).fetch_optional(&mut **tx).await.map_err(unavailable)?.ok_or(DraftError::NotFound)
    }

    async fn context(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        scope: &Scope,
        actor: Uuid,
        proposal: Uuid,
    ) -> Result<(DraftContext, bool), DraftError> {
        // Lock an existing assignment so a concurrent revocation cannot race a save.
        let collaborator: Option<String> = sqlx::query_scalar("SELECT access FROM proposal_collaborators WHERE organization_id=$1 AND proposal_id=$2 AND user_id=$3 FOR SHARE")
            .bind(scope.organization_id).bind(proposal).bind(actor).fetch_optional(&mut **tx).await.map_err(unavailable)?;
        let assigned = scope.owner_id == actor
            || collaborator.as_deref() == Some("EDIT")
            || scope.role == "ADMIN";
        let permitted = scope.role != "READER"
            && assigned
            && scope.head_sha.is_some()
            && scope.state != "PROVISIONING";
        Ok((
            DraftContext {
                proposal_id: proposal,
                current_head_sha: scope.head_sha.clone(),
                proposal_version: scope.version,
                proposal_state: scope.state.clone(),
                can_edit: permitted
                    && matches!(
                        scope.state.as_str(),
                        "DRAFT" | "IN_REVIEW" | "CHANGES_REQUESTED" | "APPROVED"
                    ),
                can_store_recovery: permitted && self.retention.is_some(),
                retention_days: self.retention.map(DraftRetention::days),
            },
            permitted,
        ))
    }

    fn days(&self) -> Option<i32> {
        self.retention.map(|policy| i32::from(policy.days()))
    }

    async fn buffers(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        actor: Uuid,
        context: &DraftContext,
        document: Option<&str>,
    ) -> Result<Vec<DraftBuffer>, DraftError> {
        sqlx::query_as::<_, BufferRow>("SELECT document_id,base_head_sha,version,to_char(updated_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS updated_at,to_char(least(expires_at,updated_at+make_interval(days=>$4)) AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS expires_at,CASE WHEN $3::text IS NOT NULL THEN content END AS content_utf8,coalesce(base_head_sha=$5,false) AS base_is_current FROM draft_buffers WHERE proposal_id=$1 AND user_id=$2 AND ($3::text IS NULL OR document_id=$3) AND least(expires_at,updated_at+make_interval(days=>$4))>now() ORDER BY document_id LIMIT 1001")
            .bind(context.proposal_id).bind(actor).bind(document).bind(self.days()).bind(&context.current_head_sha)
            .fetch_all(&mut **tx).await.map(|rows| rows.into_iter().map(Into::into).collect()).map_err(unavailable)
    }

    pub async fn read(
        &self,
        actor: Uuid,
        proposal: Uuid,
        document: Option<&str>,
    ) -> Result<DraftSnapshot, DraftError> {
        let mut tx = self.transaction().await?;
        let scope = Self::scope(&mut tx, actor, proposal, false).await?;
        let (context, _) = self.context(&mut tx, &scope, actor, proposal).await?;
        let buffers = self.buffers(&mut tx, actor, &context, document).await?;
        if buffers.len() > MAX_RECOVERY_DOCUMENTS as usize {
            return Err(DraftError::QuotaExceeded);
        }
        tx.commit().await.map_err(unavailable)?;
        Ok(DraftSnapshot { context, buffers })
    }

    pub async fn save(
        &self,
        actor: Uuid,
        proposal: Uuid,
        draft: &ValidatedDraft,
    ) -> Result<DraftSaved, DraftError> {
        let request = draft.request();
        let mut tx = self.transaction().await?;
        let scope = Self::scope(&mut tx, actor, proposal, true).await?;
        let (context, permitted) = self.context(&mut tx, &scope, actor, proposal).await?;
        if !permitted {
            return Err(DraftError::Forbidden);
        }
        let days = self.days().ok_or(DraftError::PolicyNotConfigured)?;
        let existing: Option<Existing> = sqlx::query_as("SELECT version,least(expires_at,updated_at+make_interval(days=>$4))>now() AS live,last_save_id,last_request_hash FROM draft_buffers WHERE proposal_id=$1 AND user_id=$2 AND document_id=$3 FOR UPDATE")
            .bind(proposal).bind(actor).bind(&request.document_id).bind(days).fetch_optional(&mut *tx).await.map_err(unavailable)?;
        let replayed = existing
            .as_ref()
            .is_some_and(|row| row.live && row.last_save_id == Some(request.save_id));
        if replayed {
            if existing
                .as_ref()
                .and_then(|row| row.last_request_hash.as_deref())
                != Some(draft.fingerprint())
            {
                return Err(DraftError::SaveIdentityMismatch);
            }
        } else {
            let current = existing
                .as_ref()
                .filter(|row| row.live)
                .map(|row| row.version);
            if current.unwrap_or(0) != request.expected_draft_version {
                return Err(DraftError::VersionConflict {
                    current_version: current,
                });
            }
            let usage: (i64, i64) = sqlx::query_as("SELECT count(*),coalesce(sum(octet_length(content)),0)::bigint FROM draft_buffers WHERE proposal_id=$1 AND user_id=$2 AND document_id<>$3 AND least(expires_at,updated_at+make_interval(days=>$4))>now()")
                .bind(proposal).bind(actor).bind(&request.document_id).bind(days).fetch_one(&mut *tx).await.map_err(unavailable)?;
            if usage.0 >= MAX_RECOVERY_DOCUMENTS
                || usage.1 + request.content_utf8.len() as i64 > MAX_RECOVERY_BYTES
            {
                return Err(DraftError::QuotaExceeded);
            }
            let version: i64 = sqlx::query_scalar("INSERT INTO draft_buffers(organization_id,proposal_id,user_id,document_id,base_head_sha,content,expires_at,last_save_id,last_request_hash) VALUES($1,$2,$3,$4,$5,$6,now()+make_interval(days=>$7),$8,$9) ON CONFLICT(proposal_id,user_id,document_id) DO UPDATE SET base_head_sha=excluded.base_head_sha,content=excluded.content,expires_at=excluded.expires_at,updated_at=now(),version=nextval('draft_buffer_version_seq'),last_save_id=excluded.last_save_id,last_request_hash=excluded.last_request_hash RETURNING version")
                .bind(scope.organization_id).bind(proposal).bind(actor).bind(&request.document_id).bind(&request.base_head_sha).bind(&request.content_utf8).bind(days).bind(request.save_id).bind(draft.fingerprint())
                .fetch_one(&mut *tx).await.map_err(unavailable)?;
            sqlx::query("INSERT INTO audit_events(id,organization_id,actor_id,action,entity_id,before_ref,after_ref,result) VALUES($1,$2,$3,'DRAFT_RECOVERY_SAVED',$4,$5,$6,'RECOVERY_ONLY')")
                .bind(Uuid::new_v4()).bind(scope.organization_id).bind(actor).bind(proposal).bind(&request.base_head_sha).bind(format!("draft:{version}"))
                .execute(&mut *tx).await.map_err(unavailable)?;
        }
        let mut buffers = self
            .buffers(&mut tx, actor, &context, Some(&request.document_id))
            .await?;
        let mut buffer = buffers.pop().ok_or(DraftError::Unavailable)?;
        buffer.content_utf8 = None;
        tx.commit().await.map_err(unavailable)?;
        Ok(DraftSaved {
            context,
            buffer,
            replayed,
        })
    }

    /// A user may discard their own recovery even after losing edit assignment.
    /// Exact versions protect buffers recreated after a lost DELETE response.
    pub async fn discard(
        &self,
        actor: Uuid,
        proposal: Uuid,
        document: &str,
        version: i64,
    ) -> Result<(), DraftError> {
        let mut tx = self.transaction().await?;
        let scope = Self::scope(&mut tx, actor, proposal, true).await?;
        let current: Option<i64> = sqlx::query_scalar("SELECT version FROM draft_buffers WHERE proposal_id=$1 AND user_id=$2 AND document_id=$3 FOR UPDATE")
            .bind(proposal).bind(actor).bind(document).fetch_optional(&mut *tx).await.map_err(unavailable)?;
        if let Some(current) = current {
            if current != version {
                return Err(DraftError::VersionConflict {
                    current_version: Some(current),
                });
            }
            sqlx::query("DELETE FROM draft_buffers WHERE proposal_id=$1 AND user_id=$2 AND document_id=$3 AND version=$4")
                .bind(proposal).bind(actor).bind(document).bind(version).execute(&mut *tx).await.map_err(unavailable)?;
            sqlx::query("INSERT INTO audit_events(id,organization_id,actor_id,action,entity_id,before_ref,result) VALUES($1,$2,$3,'DRAFT_RECOVERY_DISCARDED',$4,$5,'RECOVERY_ONLY')")
                .bind(Uuid::new_v4()).bind(scope.organization_id).bind(actor).bind(proposal).bind(format!("draft:{version}"))
                .execute(&mut *tx).await.map_err(unavailable)?;
        }
        tx.commit().await.map_err(unavailable)
    }

    /// Bounded batches; a shorter configured policy also takes effect for older
    /// buffers. Global sequence versions prevent stale tabs surviving deletion.
    pub async fn purge_expired(&self) -> Result<u64, DraftError> {
        sqlx::query("WITH expired AS (SELECT ctid FROM draft_buffers WHERE least(expires_at,updated_at+make_interval(days=>$1))<=now() ORDER BY expires_at LIMIT 500 FOR UPDATE SKIP LOCKED) DELETE FROM draft_buffers d USING expired e WHERE d.ctid=e.ctid")
            .bind(self.days()).execute(&self.pool).await.map(|result| result.rows_affected()).map_err(unavailable)
    }

    pub async fn run_cleanup(&self, mut stopped: tokio::sync::watch::Receiver<bool>) {
        let mut ticks = tokio::time::interval(std::time::Duration::from_secs(60));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            if *stopped.borrow() {
                return;
            }
            tokio::select! {
                _ = ticks.tick() => {
                    let result = tokio::time::timeout(std::time::Duration::from_secs(10), self.purge_expired()).await;
                    if !matches!(result, Ok(Ok(_))) { tracing::warn!("Draft recovery cleanup unavailable"); }
                }
                changed = stopped.changed() => { if changed.is_err() || *stopped.borrow() { return; } }
            }
        }
    }
}
fn unavailable(_: sqlx::Error) -> DraftError {
    DraftError::Unavailable
}
