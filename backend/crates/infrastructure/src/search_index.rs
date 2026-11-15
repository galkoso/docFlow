//! Derived snapshot publication and reads. All content reads recheck current ACL;
//! Git/provider work happens before the publication transaction begins.
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use specspace_application::search::{IndexBlock, IndexSnapshot};
use specspace_application::{book::BookError, reader::MainHistoryProof};
use specspace_document_engine::{
    find::{MatchPage, VisibleBlock},
    index::{self, IndexedBlock, IndexedText, NORMALIZER_VERSION},
};
use specspace_git_adapter::CommitSha;
use sqlx::{FromRow, PgPool, Postgres, QueryBuilder, Transaction};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
};
use tokio::sync::{Mutex, Semaphore};
use uuid::Uuid;

#[derive(Debug, PartialEq, Eq)]
pub enum IndexError {
    NotFound,
    NotReady,
    InvalidSnapshot,
    LeaseLost,
    InvalidQuery(&'static str),
    Unavailable,
}
impl From<sqlx::Error> for IndexError {
    fn from(_: sqlx::Error) -> Self {
        Self::Unavailable
    }
}

#[derive(Debug, FromRow)]
pub struct IndexStatus {
    pub job_id: Uuid,
    pub state: String,
    pub error_code: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexPayload {
    pub repository_id: Uuid,
    pub snapshot_sha: String,
    pub normalizer_version: i32,
    pub requested_by: Uuid,
}

pub struct IndexLease {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub payload: IndexPayload,
    pub attempts: i32,
    worker: Uuid,
}

struct CachedText {
    organization: Uuid,
    hash: String,
    text: Arc<SearchText>,
    bytes: usize,
}
struct SearchText {
    original: String,
    index: IndexedText,
}
#[derive(FromRow)]
struct Reference {
    book_order: i32,
    document_id: String,
    section_id: String,
    block_id: String,
    text_hash: String,
}
#[derive(FromRow)]
struct Payload {
    text_hash: String,
    original_text: String,
    normalized_text: String,
    source_map: Vec<u8>,
}

// Load only uncached payloads that match the literal normalized needle. Exact
// scoped-key lateral lookups avoid rescanning or detoasting unrelated cached
// text. References are read separately under the same repeatable-read snapshot.
const MISSING_PAYLOADS: &str = "SELECT t.text_hash,t.original_text,t.normalized_text,t.source_map
FROM unnest($3::text[]) AS wanted(hash) CROSS JOIN LATERAL (
  SELECT text_hash,original_text,normalized_text,source_map FROM search_index_texts
  WHERE organization_id=$1 AND normalizer_version=$2 AND text_hash=wanted.hash
    AND strpos(normalized_text,$4)>0 OFFSET 0
) t";

pub struct PgSearchIndex {
    pool: PgPool,
    cache: Mutex<VecDeque<CachedText>>,
    processing: Arc<Semaphore>,
}

#[async_trait]
impl MainHistoryProof for PgSearchIndex {
    async fn contains(&self, repository: Uuid, commit: &CommitSha) -> Result<bool, BookError> {
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM search_index_snapshots s JOIN jobs j ON j.id=s.job_id AND j.organization_id=s.organization_id WHERE s.repository_id=$1 AND s.snapshot_sha=$2 AND s.normalizer_version=$3 AND s.state='READY' AND j.kind='INDEX_SNAPSHOT' AND j.state='SUCCEEDED')")
            .bind(repository).bind(commit.as_str()).bind(NORMALIZER_VERSION)
            .fetch_one(&self.pool).await.map_err(|_| BookError::AuthorizationUnavailable)
    }
}

impl PgSearchIndex {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            cache: Mutex::new(VecDeque::new()),
            processing: Arc::new(Semaphore::new(2)),
        }
    }

    async fn authorize(&self, actor: Uuid, repository: Uuid) -> Result<Uuid, IndexError> {
        sqlx::query_scalar("SELECT r.organization_id FROM repositories r JOIN memberships m ON m.organization_id=r.organization_id JOIN users u ON u.id=m.user_id WHERE r.id=$1 AND m.user_id=$2 AND u.status='ACTIVE'")
            .bind(repository).bind(actor).fetch_optional(&self.pool).await?.ok_or(IndexError::NotFound)
    }

    /// Call after proving Git main-history provenance. This is derived work, not
    /// a Git mutation, and does not make a missing snapshot searchable.
    pub async fn enqueue(
        &self,
        actor: Uuid,
        repository: Uuid,
        commit: &CommitSha,
    ) -> Result<IndexStatus, IndexError> {
        let organization = self.authorize(actor, repository).await?;
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!(
                "{repository}:{}:{NORMALIZER_VERSION}",
                commit.as_str()
            ))
            .execute(&mut *tx)
            .await?;
        let existing = sqlx::query_as::<_, IndexStatus>("SELECT job_id,state,error_code FROM search_index_snapshots WHERE repository_id=$1 AND snapshot_sha=$2 AND normalizer_version=$3")
            .bind(repository).bind(commit.as_str()).bind(NORMALIZER_VERSION).fetch_optional(&mut *tx).await?;
        let status = if let Some(status) = existing {
            status
        } else {
            let job = Uuid::new_v4();
            let payload = IndexPayload {
                repository_id: repository,
                snapshot_sha: commit.as_str().into(),
                normalizer_version: NORMALIZER_VERSION,
                requested_by: actor,
            };
            sqlx::query("INSERT INTO jobs(id,organization_id,kind,payload) VALUES($1,$2,'INDEX_SNAPSHOT',$3)")
                .bind(job).bind(organization).bind(serde_json::to_value(payload).map_err(|_| IndexError::InvalidSnapshot)?).execute(&mut *tx).await?;
            sqlx::query("INSERT INTO search_index_snapshots(organization_id,repository_id,snapshot_sha,normalizer_version,job_id) VALUES($1,$2,$3,$4,$5)")
                .bind(organization).bind(repository).bind(commit.as_str()).bind(NORMALIZER_VERSION).bind(job).execute(&mut *tx).await?;
            IndexStatus {
                job_id: job,
                state: "PENDING".into(),
                error_code: None,
            }
        };
        tx.commit().await?;
        self.authorize(actor, repository).await?;
        Ok(status)
    }

    /// Expired indexing leases may be reclaimed because their only side effect is
    /// a fenced, atomic derived-index transaction. Git-operation leases are untouched.
    pub async fn claim(
        &self,
        worker: Uuid,
        seconds: i32,
    ) -> Result<Option<IndexLease>, IndexError> {
        if !(1..=3600).contains(&seconds) {
            return Err(IndexError::InvalidQuery("INVALID_LEASE"));
        }
        sqlx::query("WITH exhausted AS (UPDATE jobs SET state='FAILED',result=jsonb_build_object('error_code','INDEX_ATTEMPTS_EXHAUSTED'),lease_owner=NULL,leased_until=NULL WHERE kind='INDEX_SNAPSHOT' AND attempts>=max_attempts AND (state='QUEUED' OR (state='RUNNING' AND leased_until<=clock_timestamp())) RETURNING id) UPDATE search_index_snapshots SET state='FAILED',error_code='INDEX_ATTEMPTS_EXHAUSTED' WHERE state='PENDING' AND job_id IN(SELECT id FROM exhausted)")
            .execute(&self.pool).await?;
        let job = sqlx::query_as::<_, crate::LeasedJob>("WITH candidate AS (SELECT id FROM jobs WHERE kind='INDEX_SNAPSHOT' AND attempts<max_attempts AND ((state='QUEUED' AND available_at<=clock_timestamp()) OR (state='RUNNING' AND leased_until<=clock_timestamp())) ORDER BY created_at,id FOR UPDATE SKIP LOCKED LIMIT 1) UPDATE jobs SET state='RUNNING',lease_owner=$1,leased_until=clock_timestamp()+make_interval(secs=>$2),heartbeat_at=clock_timestamp(),attempts=attempts+1 FROM candidate WHERE jobs.id=candidate.id RETURNING jobs.id,organization_id,operation_id,kind,payload,attempts")
            .bind(worker).bind(f64::from(seconds)).fetch_optional(&self.pool).await?;
        let Some(job) = job else {
            return Ok(None);
        };
        let payload: IndexPayload = match serde_json::from_value(job.payload) {
            Ok(payload) => payload,
            Err(_) => {
                self.reject_claim(job.id, worker, "INVALID_INDEX_JOB")
                    .await?;
                return Err(IndexError::InvalidSnapshot);
            }
        };
        let supported = payload.normalizer_version == NORMALIZER_VERSION;
        let valid_sha = CommitSha::parse(&payload.snapshot_sha)
            .is_ok_and(|commit| commit.as_str() == payload.snapshot_sha);
        let linked: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM search_index_snapshots WHERE job_id=$1 AND organization_id=$2 AND repository_id=$3 AND snapshot_sha=$4 AND normalizer_version=$5 AND state='PENDING')")
            .bind(job.id).bind(job.organization_id).bind(payload.repository_id).bind(&payload.snapshot_sha).bind(payload.normalizer_version).fetch_one(&self.pool).await?;
        if !supported || !valid_sha || !linked {
            self.reject_claim(
                job.id,
                worker,
                if supported {
                    "INVALID_INDEX_JOB"
                } else {
                    "UNSUPPORTED_SCHEMA"
                },
            )
            .await?;
            return Err(IndexError::InvalidSnapshot);
        }
        Ok(Some(IndexLease {
            id: job.id,
            organization_id: job.organization_id,
            payload,
            attempts: job.attempts,
            worker,
        }))
    }

    pub async fn heartbeat(&self, lease: &IndexLease, seconds: i32) -> Result<(), IndexError> {
        if !(1..=3600).contains(&seconds) {
            return Err(IndexError::InvalidQuery("INVALID_LEASE"));
        }
        let updated = sqlx::query("UPDATE jobs SET heartbeat_at=clock_timestamp(),leased_until=clock_timestamp()+make_interval(secs=>$3) WHERE id=$1 AND lease_owner=$2 AND state='RUNNING' AND leased_until>clock_timestamp()")
            .bind(lease.id).bind(lease.worker).bind(f64::from(seconds)).execute(&self.pool).await?;
        if updated.rows_affected() == 1 {
            Ok(())
        } else {
            Err(IndexError::LeaseLost)
        }
    }

    /// Retry only derived indexing work, with a bounded delay and attempt count.
    /// Returns true when the job remains queued; false means terminal failure.
    pub async fn fail(
        &self,
        lease: &IndexLease,
        code: &'static str,
        retry_after: Option<u32>,
    ) -> Result<bool, IndexError> {
        if retry_after.is_some_and(|seconds| seconds == 0 || seconds > 86400) {
            return Err(IndexError::InvalidQuery("INVALID_RETRY_DELAY"));
        }
        let mut tx = self.pool.begin().await?;
        let state: Option<String> = sqlx::query_scalar("UPDATE jobs SET state=CASE WHEN $4::double precision IS NOT NULL AND attempts<max_attempts THEN 'QUEUED' ELSE 'FAILED' END,result=jsonb_build_object('error_code',$3::text),available_at=clock_timestamp()+make_interval(secs=>coalesce($4,0)),lease_owner=NULL,leased_until=NULL WHERE id=$1 AND lease_owner=$2 AND state='RUNNING' AND leased_until>clock_timestamp() RETURNING state")
            .bind(lease.id).bind(lease.worker).bind(code).bind(retry_after.map(f64::from)).fetch_optional(&mut *tx).await?;
        let state = state.ok_or(IndexError::LeaseLost)?;
        sqlx::query("UPDATE search_index_snapshots SET state=$3,error_code=$2 WHERE job_id=$1 AND state='PENDING'")
            .bind(lease.id).bind(code).bind(if state == "QUEUED" { "PENDING" } else { "FAILED" }).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(state == "QUEUED")
    }

    async fn reject_claim(
        &self,
        job: Uuid,
        worker: Uuid,
        code: &'static str,
    ) -> Result<(), IndexError> {
        let mut tx = self.pool.begin().await?;
        let updated = sqlx::query("UPDATE jobs SET state='FAILED',result=jsonb_build_object('error_code',$3::text),lease_owner=NULL,leased_until=NULL WHERE id=$1 AND lease_owner=$2 AND state='RUNNING' AND leased_until>clock_timestamp()")
            .bind(job).bind(worker).bind(code).execute(&mut *tx).await?;
        if updated.rows_affected() != 1 {
            return Err(IndexError::LeaseLost);
        }
        sqlx::query("UPDATE search_index_snapshots SET state='FAILED',error_code=$2 WHERE job_id=$1 AND state='PENDING'")
            .bind(job).bind(code).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn publish(
        &self,
        lease: &IndexLease,
        snapshot: &IndexSnapshot,
    ) -> Result<(), IndexError> {
        if lease.payload.normalizer_version != NORMALIZER_VERSION
            || lease.payload.snapshot_sha != snapshot.commit().as_str()
        {
            return Err(IndexError::InvalidSnapshot);
        }
        let mut tx = self.pool.begin().await?;
        let owned: Option<Uuid> = sqlx::query_scalar("SELECT id FROM jobs WHERE id=$1 AND organization_id=$2 AND kind='INDEX_SNAPSHOT' AND state='RUNNING' AND lease_owner=$3 AND leased_until>clock_timestamp() AND payload=$4 FOR UPDATE")
            .bind(lease.id).bind(lease.organization_id).bind(lease.worker)
            .bind(serde_json::to_value(&lease.payload).map_err(|_| IndexError::InvalidSnapshot)?).fetch_optional(&mut *tx).await?;
        if owned.is_none() {
            return Err(IndexError::LeaseLost);
        }
        let selected: Option<Uuid> = sqlx::query_scalar("SELECT job_id FROM search_index_snapshots WHERE organization_id=$1 AND repository_id=$2 AND snapshot_sha=$3 AND normalizer_version=$4 AND state='PENDING' FOR UPDATE")
            .bind(lease.organization_id).bind(lease.payload.repository_id).bind(snapshot.commit().as_str()).bind(NORMALIZER_VERSION).fetch_optional(&mut *tx).await?;
        if selected != Some(lease.id) {
            return Err(IndexError::LeaseLost);
        }
        let mut seen = HashSet::new();
        let mut batch = vec![];
        let mut bytes = 0usize;
        for block in snapshot.blocks() {
            if !seen.insert(&block.text_hash) {
                continue;
            }
            let size = block.original.len() + block.normalized.len() + block.source_map.len();
            if !batch.is_empty() && (bytes + size > 4 * 1024 * 1024 || batch.len() >= 32) {
                insert_texts(&mut tx, lease.organization_id, &batch).await?;
                batch.clear();
                bytes = 0;
            }
            batch.push(block);
            bytes += size;
        }
        if !batch.is_empty() {
            insert_texts(&mut tx, lease.organization_id, &batch).await?;
        }
        sqlx::query("DELETE FROM search_index_blocks WHERE repository_id=$1 AND snapshot_sha=$2 AND normalizer_version=$3")
            .bind(lease.payload.repository_id).bind(snapshot.commit().as_str()).bind(NORMALIZER_VERSION).execute(&mut *tx).await?;
        for batch in snapshot.blocks().chunks(256) {
            let mut query = QueryBuilder::<Postgres>::new(
                "INSERT INTO search_index_blocks(organization_id,repository_id,snapshot_sha,normalizer_version,book_order,document_id,section_id,block_id,text_hash) ",
            );
            query.push_values(batch, |mut row, block| {
                row.push_bind(lease.organization_id)
                    .push_bind(lease.payload.repository_id)
                    .push_bind(snapshot.commit().as_str())
                    .push_bind(NORMALIZER_VERSION)
                    .push_bind(block.book_order)
                    .push_bind(&block.document_id)
                    .push_bind(&block.section_id)
                    .push_bind(&block.block_id)
                    .push_bind(&block.text_hash);
            });
            query.build().execute(&mut *tx).await?;
        }
        sqlx::query("UPDATE search_index_snapshots SET state='READY',manifest_hash=$4,document_count=$5,block_count=$6,original_bytes=$7,normalized_bytes=$8,ready_at=clock_timestamp(),error_code=NULL WHERE repository_id=$1 AND snapshot_sha=$2 AND normalizer_version=$3")
            .bind(lease.payload.repository_id).bind(snapshot.commit().as_str()).bind(NORMALIZER_VERSION).bind(snapshot.manifest_hash())
            .bind(snapshot.document_count()).bind(i32::try_from(snapshot.blocks().len()).map_err(|_| IndexError::InvalidSnapshot)?)
            .bind(snapshot.original_bytes()).bind(snapshot.normalized_bytes()).execute(&mut *tx).await?;
        let done = sqlx::query("UPDATE jobs SET state='SUCCEEDED',result=$3,lease_owner=NULL,leased_until=NULL WHERE id=$1 AND lease_owner=$2 AND state='RUNNING' AND leased_until>clock_timestamp()")
            .bind(lease.id).bind(lease.worker).bind(serde_json::json!({"snapshot_sha":snapshot.commit().as_str(),"blocks":snapshot.blocks().len()})).execute(&mut *tx).await?;
        if done.rows_affected() != 1 {
            return Err(IndexError::LeaseLost);
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn find(
        &self,
        actor: Uuid,
        repository: Uuid,
        commit: &CommitSha,
        query: &str,
        offset: usize,
        limit: usize,
    ) -> Result<MatchPage, IndexError> {
        let started = std::time::Instant::now();
        let organization = self.authorize(actor, repository).await?;
        let needle = index::normalized_query(query).map_err(IndexError::InvalidQuery)?;
        if !(1..=500).contains(&limit) {
            return Err(IndexError::InvalidQuery("INVALID_LIMIT"));
        }
        let permit = self
            .processing
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| IndexError::Unavailable)?;
        self.authorize(actor, repository).await?;
        let mut tx = self.pool.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
            .execute(&mut *tx)
            .await?;
        let ready: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM search_index_snapshots s JOIN jobs j ON j.id=s.job_id AND j.organization_id=s.organization_id WHERE s.organization_id=$1 AND s.repository_id=$2 AND s.snapshot_sha=$3 AND s.normalizer_version=$4 AND s.state='READY' AND j.kind='INDEX_SNAPSHOT' AND j.state='SUCCEEDED')")
            .bind(organization).bind(repository).bind(commit.as_str()).bind(NORMALIZER_VERSION).fetch_one(&mut *tx).await?;
        if !ready {
            return Err(IndexError::NotReady);
        }
        tracing::debug!(
            phase = "ready",
            elapsed_ms = started.elapsed().as_millis() as u64,
            "search index read"
        );
        let references = if needle.is_empty() || needle.contains('\0') {
            vec![]
        } else {
            sqlx::query_as::<_, Reference>("SELECT book_order,document_id,section_id,block_id,text_hash FROM search_index_blocks WHERE organization_id=$1 AND repository_id=$2 AND snapshot_sha=$3 AND normalizer_version=$4 ORDER BY book_order")
                .bind(organization).bind(repository).bind(commit.as_str()).bind(NORMALIZER_VERSION)
                .fetch_all(&mut *tx).await?
        };
        let mut texts = HashMap::new();
        tracing::debug!(
            phase = "references",
            elapsed_ms = started.elapsed().as_millis() as u64,
            blocks = references.len(),
            "search index read"
        );
        let mut missing = HashSet::new();
        {
            let mut cache = self.cache.lock().await;
            for reference in &references {
                if texts.contains_key(&reference.text_hash)
                    || missing.contains(&reference.text_hash)
                {
                    continue;
                }
                if let Some(position) = cache.iter().position(|entry| {
                    entry.organization == organization && entry.hash == reference.text_hash
                }) {
                    let entry = cache.remove(position).expect("located cache entry");
                    texts.insert(entry.hash.clone(), entry.text.clone());
                    cache.push_back(entry);
                } else {
                    missing.insert(reference.text_hash.clone());
                }
            }
        }
        let hashes: Vec<_> = missing.into_iter().collect();
        let payloads = if hashes.is_empty() {
            vec![]
        } else {
            sqlx::query_as::<_, Payload>(MISSING_PAYLOADS)
                .bind(organization)
                .bind(NORMALIZER_VERSION)
                .bind(&hashes)
                .bind(&needle)
                .fetch_all(&mut *tx)
                .await?
        };
        tx.commit().await?;
        tracing::debug!(
            phase = "payloads",
            elapsed_ms = started.elapsed().as_millis() as u64,
            payloads = payloads.len(),
            "search index read"
        );
        let query = query.to_owned();
        let snapshot = commit.as_str().to_owned();
        let (page, fresh) = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let mut fresh = vec![];
            for payload in payloads {
                let index = IndexedText::restore(
                    &payload.original_text,
                    payload.normalized_text,
                    &payload.source_map,
                    &payload.text_hash,
                )
                .map_err(|_| IndexError::InvalidSnapshot)?;
                let text = Arc::new(SearchText {
                    original: payload.original_text,
                    index,
                });
                texts.insert(payload.text_hash.clone(), text.clone());
                fresh.push((payload.text_hash, text));
            }
            let mut blocks = Vec::with_capacity(references.len());
            for reference in &references {
                // A missing uncached payload was filtered by the literal SQL
                // predicate. Cached payloads are matched below in Rust; this is
                // independent of the queries that first warmed the cache.
                let Some(text) = texts.get(&reference.text_hash) else {
                    continue;
                };
                blocks.push(IndexedBlock {
                    block: VisibleBlock {
                        document_id: &reference.document_id,
                        section_id: &reference.section_id,
                        block_id: &reference.block_id,
                        text: &text.original,
                    },
                    book_order: reference.book_order as usize,
                    text: &text.index,
                });
            }
            let page = index::find(&snapshot, &blocks, &query, offset, limit)
                .map_err(|_| IndexError::InvalidSnapshot)?;
            Ok::<_, IndexError>((page, fresh))
        })
        .await
        .map_err(|_| IndexError::Unavailable)??;
        tracing::debug!(
            phase = "matched",
            elapsed_ms = started.elapsed().as_millis() as u64,
            "search index read"
        );
        {
            let mut cache = self.cache.lock().await;
            let mut used: usize = cache.iter().map(|entry| entry.bytes).sum();
            for (hash, text) in fresh {
                let bytes = text.original.capacity() + text.index.estimated_bytes() + 256;
                if bytes > 128 * 1024 * 1024
                    || cache
                        .iter()
                        .any(|entry| entry.organization == organization && entry.hash == hash)
                {
                    continue;
                }
                while cache.len() >= 4096 || used + bytes > 128 * 1024 * 1024 {
                    if let Some(evicted) = cache.pop_front() {
                        used -= evicted.bytes;
                    } else {
                        break;
                    }
                }
                cache.push_back(CachedText {
                    organization,
                    hash,
                    text,
                    bytes,
                });
                used += bytes;
            }
        }
        self.authorize(actor, repository).await?;
        tracing::debug!(
            phase = "complete",
            elapsed_ms = started.elapsed().as_millis() as u64,
            "search index read"
        );
        Ok(page)
    }
}

async fn insert_texts(
    tx: &mut Transaction<'_, Postgres>,
    organization: Uuid,
    blocks: &[&IndexBlock],
) -> Result<(), IndexError> {
    let mut query = QueryBuilder::<Postgres>::new(
        "INSERT INTO search_index_texts(organization_id,normalizer_version,text_hash,original_text,normalized_text,source_map) ",
    );
    query.push_values(blocks, |mut row, block| {
        row.push_bind(organization)
            .push_bind(NORMALIZER_VERSION)
            .push_bind(&block.text_hash)
            .push_bind(&block.original)
            .push_bind(&block.normalized)
            .push_bind(&block.source_map);
    });
    query.push(" ON CONFLICT DO NOTHING");
    query.build().execute(&mut **tx).await?;
    // A separate READ COMMITTED statement also sees a concurrent insert which
    // won the conflict. Never update immutable shared payloads just to reuse them.
    let mut verify = QueryBuilder::<Postgres>::new(
        "SELECT text_hash FROM search_index_texts WHERE organization_id=",
    );
    verify
        .push_bind(organization)
        .push(" AND normalizer_version=")
        .push_bind(NORMALIZER_VERSION)
        .push(" AND (");
    for (index, block) in blocks.iter().enumerate() {
        if index > 0 {
            verify.push(" OR ");
        }
        verify
            .push("(text_hash=")
            .push_bind(&block.text_hash)
            .push(" AND original_text=")
            .push_bind(&block.original)
            .push(" AND normalized_text=")
            .push_bind(&block.normalized)
            .push(" AND source_map=")
            .push_bind(&block.source_map)
            .push(")");
    }
    verify.push(")");
    let accepted = verify.build().fetch_all(&mut **tx).await?;
    if accepted.len() != blocks.len() {
        return Err(IndexError::InvalidSnapshot);
    }
    Ok(())
}
