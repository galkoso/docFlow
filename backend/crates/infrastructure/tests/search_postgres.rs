use specspace_document_engine::index::IndexedText;
use sqlx::PgPool;
use uuid::Uuid;

const SHA: &str = "1111111111111111111111111111111111111111";
const TEXT: &str = "alpha [x]*";

struct Fixture {
    organization: Uuid,
    repository: Uuid,
    user: Uuid,
}

async fn create_fixture(pool: &PgPool) -> Fixture {
    let organization = Uuid::new_v4();
    let repository = Uuid::new_v4();
    let user = Uuid::new_v4();
    sqlx::query("INSERT INTO organizations(id,name) VALUES($1,'Synthetic index')")
        .bind(organization)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO repositories(id,organization_id,provider,remote_id,installation_id) VALUES($1,$2,'GITHUB','42','7')")
        .bind(repository).bind(organization).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO users(id,issuer,external_subject,display_name) VALUES($1,'fixture',$1::text,'Synthetic index reader')").bind(user).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO memberships(organization_id,user_id,role) VALUES($1,$2,'READER')")
        .bind(organization)
        .bind(user)
        .execute(pool)
        .await
        .unwrap();
    let fixture = Fixture {
        organization,
        repository,
        user,
    };
    pending(pool, &fixture, 1).await;
    fixture
}

async fn pending(pool: &PgPool, fixture: &Fixture, version: i32) {
    let job = Uuid::new_v4();
    sqlx::query("INSERT INTO jobs(id,organization_id,kind,payload) VALUES($1,$2,'INDEX_SNAPSHOT',$3)")
        .bind(job).bind(fixture.organization)
        .bind(serde_json::json!({"repository_id":fixture.repository,"snapshot_sha":SHA,"normalizer_version":version,"requested_by":fixture.user}))
        .execute(pool).await.unwrap();
    sqlx::query("INSERT INTO search_index_snapshots(organization_id,repository_id,snapshot_sha,normalizer_version,job_id) VALUES($1,$2,$3,$4,$5)")
        .bind(fixture.organization).bind(fixture.repository).bind(SHA).bind(version).bind(job).execute(pool).await.unwrap();
}

async fn text(pool: &PgPool, fixture: &Fixture) -> String {
    let indexed = IndexedText::build(TEXT).unwrap();
    let map = indexed.encode_map();
    let hash = IndexedText::fingerprint(TEXT, indexed.normalized(), &map);
    sqlx::query("INSERT INTO search_index_texts(organization_id,normalizer_version,text_hash,original_text,normalized_text,source_map) VALUES($1,1,$2,$3,$4,$5) ON CONFLICT DO NOTHING")
        .bind(fixture.organization).bind(&hash).bind(TEXT).bind(indexed.normalized()).bind(map).execute(pool).await.unwrap();
    hash
}

async fn block(
    pool: &PgPool,
    fixture: &Fixture,
    order: i32,
    hash: &str,
    version: i32,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO search_index_blocks(organization_id,repository_id,snapshot_sha,normalizer_version,book_order,document_id,section_id,block_id,text_hash) VALUES($1,$2,$3,$4,$5,'DOC','SEC',$6,$7)")
        .bind(fixture.organization).bind(fixture.repository).bind(SHA).bind(version).bind(order).bind(format!("block-{order}")).bind(hash)
        .execute(pool).await?;
    Ok(())
}

async fn ready(pool: &PgPool, fixture: &Fixture, count: i32) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE search_index_snapshots SET state='READY', manifest_hash=$3,document_count=1,block_count=$4,original_bytes=$5,normalized_bytes=$5,ready_at=now() WHERE repository_id=$1 AND snapshot_sha=$2 AND normalizer_version=1")
        .bind(fixture.repository).bind(SHA).bind("a".repeat(64)).bind(count).bind(i64::from(count) * TEXT.len() as i64).execute(pool).await?;
    Ok(())
}

#[sqlx::test(migrations = "../../migrations")]
async fn complete_snapshot_publication_is_atomic_immutable_and_rebuildable(pool: PgPool) {
    let fixture = create_fixture(&pool).await;
    let hash = text(&pool, &fixture).await;
    block(&pool, &fixture, 0, &hash, 1).await.unwrap();
    assert!(ready(&pool, &fixture, 2).await.is_err());
    let state: String =
        sqlx::query_scalar("SELECT state FROM search_index_snapshots WHERE repository_id=$1")
            .bind(fixture.repository)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(state, "PENDING");
    block(&pool, &fixture, 1, &hash, 1).await.unwrap();
    ready(&pool, &fixture, 2).await.unwrap();
    let shared: i64 = sqlx::query_scalar("SELECT count(*) FROM search_index_texts")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(shared, 1);
    assert!(block(&pool, &fixture, 2, &hash, 1).await.is_err());
    assert!(
        sqlx::query("DELETE FROM search_index_blocks WHERE repository_id=$1")
            .bind(fixture.repository)
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("UPDATE search_index_blocks SET section_id='OTHER' WHERE repository_id=$1")
            .bind(fixture.repository)
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("UPDATE search_index_texts SET normalized_text='changed'")
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(ready(&pool, &fixture, 0).await.is_err());
    sqlx::query("DELETE FROM search_index_snapshots WHERE repository_id=$1")
        .bind(fixture.repository)
        .execute(&pool)
        .await
        .unwrap();
    let blocks: i64 = sqlx::query_scalar("SELECT count(*) FROM search_index_blocks")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(blocks, 0);
    sqlx::query("DELETE FROM search_index_texts")
        .execute(&pool)
        .await
        .unwrap();
    pending(&pool, &fixture, 1).await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn index_rows_cannot_cross_organization_or_normalizer_version(pool: PgPool) {
    let first = create_fixture(&pool).await;
    let second = create_fixture(&pool).await;
    let hash = text(&pool, &first).await;
    assert!(block(&pool, &second, 0, &hash, 1).await.is_err());
    assert_eq!(text(&pool, &second).await, hash);
    block(&pool, &second, 0, &hash, 1).await.unwrap();
    pending(&pool, &first, 2).await;
    assert!(block(&pool, &first, 0, &hash, 2).await.is_err());
    assert!(
        sqlx::query("UPDATE search_index_snapshots SET organization_id=$2 WHERE repository_id=$1")
            .bind(first.repository)
            .bind(second.organization)
            .execute(&pool)
            .await
            .is_err()
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn ready_validation_waits_for_concurrent_block_writes_and_rejects_gaps(pool: PgPool) {
    let fixture = create_fixture(&pool).await;
    let hash = text(&pool, &fixture).await;
    block(&pool, &fixture, 1, &hash, 1).await.unwrap();
    assert!(ready(&pool, &fixture, 1).await.is_err());
    let mut writer = pool.begin().await.unwrap();
    sqlx::query("INSERT INTO search_index_blocks(organization_id,repository_id,snapshot_sha,normalizer_version,book_order,document_id,section_id,block_id,text_hash) VALUES($1,$2,$3,1,0,'DOC','SEC','block-0',$4)")
        .bind(fixture.organization).bind(fixture.repository).bind(SHA).bind(&hash).execute(&mut *writer).await.unwrap();
    let reader_pool = pool.clone();
    let mut publication = tokio::spawn(async move { ready(&reader_pool, &fixture, 1).await });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut publication)
            .await
            .is_err()
    );
    writer.commit().await.unwrap();
    assert!(publication.await.unwrap().is_err());
}

fn book(commit: &str, body: &str) -> specspace_application::book::BookSnapshot {
    let source = format!(
        "---\ndocument_id: DOC\ntitle: Synthetic\nowner_team: TEAM\nschema_version: 1\n---\n<!-- ss:section:SEC -->\n# כותרת\n\n{body}\n"
    );
    let document = specspace_document_engine::markdown::parse(&source).unwrap();
    specspace_application::book::BookSnapshot {
        commit: specspace_git_adapter::CommitSha::parse(commit).unwrap(),
        updated_at: "2026-11-14T00:00:00Z".into(),
        documents: vec![specspace_application::book::BookDocument {
            document_id: "DOC".into(),
            path: "docs/one.md".into(),
            content: Some(document),
            error_code: None,
        }],
        findings: vec![],
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn durable_store_keeps_literal_matches_pinned_and_rechecks_revoked_access(pool: PgPool) {
    use specspace_application::search::IndexSnapshot;
    use specspace_infrastructure::search_index::{IndexError, PgSearchIndex};
    let fixture = create_fixture(&pool).await;
    let store = PgSearchIndex::new(pool.clone());
    let old = book(SHA, "מְחיקה [x]*\n\nמְחיקה [x]*");
    assert!(matches!(
        store
            .find(fixture.user, fixture.repository, &old.commit, "", 0, 100)
            .await,
        Err(IndexError::NotReady)
    ));
    let lease = store.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    store
        .publish(&lease, &IndexSnapshot::build(&old).unwrap())
        .await
        .unwrap();
    for query in ["מחיקה", "[x]*", "*", "%", "", "\0"] {
        let found = store
            .find(fixture.user, fixture.repository, &old.commit, query, 0, 1)
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(found).unwrap(),
            serde_json::to_value(old.find(query, 0, 1).unwrap()).unwrap()
        );
    }
    let page = store
        .find(fixture.user, fixture.repository, &old.commit, "מחיקה", 1, 1)
        .await
        .unwrap();
    assert_eq!(page.total_matches, 2);
    let new = book(&"2".repeat(40), "צפייה [x]*");
    store
        .enqueue(fixture.user, fixture.repository, &new.commit)
        .await
        .unwrap();
    let lease = store.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    store
        .publish(&lease, &IndexSnapshot::build(&new).unwrap())
        .await
        .unwrap();
    assert_eq!(
        store
            .find(
                fixture.user,
                fixture.repository,
                &new.commit,
                "מחיקה",
                0,
                100
            )
            .await
            .unwrap()
            .total_matches,
        0
    );
    let pinned = store
        .find(fixture.user, fixture.repository, &old.commit, "מחיקה", 1, 1)
        .await
        .unwrap();
    assert_eq!(pinned.matches[0].match_id, page.matches[0].match_id);
    let shared: i64 = sqlx::query_scalar("SELECT count(*) FROM search_index_texts")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(shared, 3); // The identical heading is shared across snapshots.
    let other = create_fixture(&pool).await;
    assert!(matches!(
        store
            .find(fixture.user, other.repository, &old.commit, "מחיקה", 0, 100)
            .await,
        Err(IndexError::NotFound)
    ));
    sqlx::query("DELETE FROM memberships WHERE user_id=$1")
        .bind(fixture.user)
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        store
            .find(
                fixture.user,
                fixture.repository,
                &old.commit,
                "מחיקה",
                0,
                100
            )
            .await,
        Err(IndexError::NotFound)
    ));
}

#[sqlx::test(migrations = "../../migrations")]
async fn index_queue_is_idempotent_and_expired_owners_cannot_publish(pool: PgPool) {
    use specspace_application::search::IndexSnapshot;
    use specspace_infrastructure::search_index::{IndexError, PgSearchIndex};
    let fixture = create_fixture(&pool).await;
    sqlx::query("DELETE FROM search_index_snapshots WHERE repository_id=$1")
        .bind(fixture.repository)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM jobs WHERE organization_id=$1")
        .bind(fixture.organization)
        .execute(&pool)
        .await
        .unwrap();
    let store = PgSearchIndex::new(pool.clone());
    let book = book(SHA, "מחיקה");
    let (first, second) = tokio::join!(
        store.enqueue(fixture.user, fixture.repository, &book.commit),
        store.enqueue(fixture.user, fixture.repository, &book.commit)
    );
    assert_eq!(first.unwrap().job_id, second.unwrap().job_id);
    assert!(
        specspace_infrastructure::lease_job(&pool, Uuid::new_v4(), 60)
            .await
            .unwrap()
            .is_none()
    );
    let lease = store.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    assert!(store.claim(Uuid::new_v4(), 60).await.unwrap().is_none());
    sqlx::query("UPDATE jobs SET leased_until=clock_timestamp()-interval '1 second' WHERE id=$1")
        .bind(lease.id)
        .execute(&pool)
        .await
        .unwrap();
    let replacement = store.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    assert_eq!(
        store.heartbeat(&lease, 60).await,
        Err(IndexError::LeaseLost)
    );
    store.heartbeat(&replacement, 60).await.unwrap();
    assert_eq!(replacement.id, lease.id);
    assert_eq!(replacement.attempts, 2);
    let snapshot = IndexSnapshot::build(&book).unwrap();
    assert_eq!(
        store.publish(&lease, &snapshot).await,
        Err(IndexError::LeaseLost)
    );
    store.publish(&replacement, &snapshot).await.unwrap();
    let (state, job_state): (String,String) = sqlx::query_as("SELECT s.state,j.state FROM search_index_snapshots s JOIN jobs j ON j.id=s.job_id WHERE s.repository_id=$1")
        .bind(fixture.repository).fetch_one(&pool).await.unwrap();
    assert_eq!((state.as_str(), job_state.as_str()), ("READY", "SUCCEEDED"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn conflicting_shared_payload_rolls_back_publication_without_a_ready_marker(pool: PgPool) {
    use specspace_application::search::IndexSnapshot;
    use specspace_infrastructure::search_index::{IndexError, PgSearchIndex};
    let fixture = create_fixture(&pool).await;
    let store = PgSearchIndex::new(pool.clone());
    let source = book(SHA, "מחיקה");
    let snapshot = IndexSnapshot::build(&source).unwrap();
    let block = &snapshot.blocks()[0];
    sqlx::query("INSERT INTO search_index_texts(organization_id,normalizer_version,text_hash,original_text,normalized_text,source_map) VALUES($1,1,$2,$3,$4,$5)")
        .bind(fixture.organization).bind(&block.text_hash).bind(&block.original).bind(&block.normalized).bind(b"SSM1".as_slice()).execute(&pool).await.unwrap();
    let lease = store.claim(Uuid::new_v4(), 60).await.unwrap().unwrap();
    assert_eq!(
        store.publish(&lease, &snapshot).await,
        Err(IndexError::InvalidSnapshot)
    );
    let (state, count): (String, i64) = sqlx::query_as("SELECT state,(SELECT count(*) FROM search_index_blocks WHERE repository_id=$1) FROM search_index_snapshots WHERE repository_id=$1")
        .bind(fixture.repository).fetch_one(&pool).await.unwrap();
    assert_eq!((state.as_str(), count), ("PENDING", 0));
    sqlx::query("DELETE FROM search_index_texts WHERE organization_id=$1")
        .bind(fixture.organization)
        .execute(&pool)
        .await
        .unwrap();
    store.publish(&lease, &snapshot).await.unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn malformed_or_exhausted_index_jobs_terminate_without_reclaiming_git_side_effects(
    pool: PgPool,
) {
    use specspace_infrastructure::search_index::{IndexError, PgSearchIndex};
    let fixture = create_fixture(&pool).await;
    let store = PgSearchIndex::new(pool.clone());
    sqlx::query("UPDATE jobs SET payload='{}' WHERE organization_id=$1")
        .bind(fixture.organization)
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        store.claim(Uuid::new_v4(), 60).await,
        Err(IndexError::InvalidSnapshot)
    ));
    let (state, code): (String, Option<String>) = sqlx::query_as(
        "SELECT state,error_code FROM search_index_snapshots WHERE repository_id=$1",
    )
    .bind(fixture.repository)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (state.as_str(), code.as_deref()),
        ("FAILED", Some("INVALID_INDEX_JOB"))
    );
    let second = create_fixture(&pool).await;
    sqlx::query("UPDATE jobs SET state='RUNNING',lease_owner=$2,leased_until=clock_timestamp()-interval '1 second',attempts=max_attempts WHERE organization_id=$1")
        .bind(second.organization).bind(Uuid::new_v4()).execute(&pool).await.unwrap();
    let git_job = Uuid::new_v4();
    sqlx::query("INSERT INTO jobs(id,organization_id,kind,state,lease_owner,leased_until,attempts) VALUES($1,$2,'MERGE','RUNNING',$3,clock_timestamp()-interval '1 second',5)")
        .bind(git_job).bind(second.organization).bind(Uuid::new_v4()).execute(&pool).await.unwrap();
    assert!(store.claim(Uuid::new_v4(), 60).await.unwrap().is_none());
    let (state, code): (String, Option<String>) = sqlx::query_as(
        "SELECT state,error_code FROM search_index_snapshots WHERE repository_id=$1",
    )
    .bind(second.repository)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (state.as_str(), code.as_deref()),
        ("FAILED", Some("INDEX_ATTEMPTS_EXHAUSTED"))
    );
    let state: String = sqlx::query_scalar("SELECT state FROM jobs WHERE id=$1")
        .bind(git_job)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state, "RUNNING");
}

/// Manual store feasibility gate; excludes HTTP, Git fetches and browser rendering.
#[sqlx::test(migrations = "../../migrations")]
#[ignore = "release-mode 20MiB PostgreSQL performance measurement"]
async fn indexed_store_six_reader_20mib_benchmark(pool: PgPool) {
    use specspace_application::{
        book::{BookDocument, BookSnapshot},
        search::IndexSnapshot,
    };
    use specspace_infrastructure::search_index::PgSearchIndex;
    use std::{sync::Arc, time::Instant};
    let fixture = create_fixture(&pool).await;
    let mut actors = vec![fixture.user];
    for _ in 0..5 {
        let actor = Uuid::new_v4();
        sqlx::query("INSERT INTO users(id,issuer,external_subject,display_name) VALUES($1,'fixture',$1::text,'Synthetic performance user')").bind(actor).execute(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO memberships(organization_id,user_id,role) VALUES($1,$2,'CONTRIBUTOR')",
        )
        .bind(fixture.organization)
        .bind(actor)
        .execute(&pool)
        .await
        .unwrap();
        actors.push(actor);
    }
    let count = 1000;
    let target = 20 * 1024 * 1024;
    let phrase = "טקסט הקשר עברי רגיל לבדיקת ביצועים. ";
    let mut documents = Vec::with_capacity(count);
    for index in 0..count {
        let document_id = format!("DOC-{index:04}");
        let prefix = format!(
            "---\ndocument_id: {document_id}\ntitle: Synthetic\nowner_team: TEAM\nschema_version: 1\n---\n<!-- ss:section:SEC-{index:04} -->\n# פרק\n\n"
        );
        let suffix = format!("מחיקה מזהה{index:04}");
        let size = target / count + usize::from(index < target % count);
        let mut source = prefix.clone();
        source.push_str(&phrase.repeat((size - prefix.len() - suffix.len()) / phrase.len()));
        source.push_str(&suffix);
        source.extend(std::iter::repeat_n('x', size - source.len()));
        documents.push(BookDocument {
            document_id,
            path: format!("docs/{index:04}.md"),
            content: Some(specspace_document_engine::markdown::parse(&source).unwrap()),
            error_code: None,
        });
    }
    let book = BookSnapshot {
        commit: specspace_git_adapter::CommitSha::parse(SHA).unwrap(),
        updated_at: "2026-11-14T00:00:00Z".into(),
        documents,
        findings: vec![],
    };
    let source_bytes: usize = book
        .documents
        .iter()
        .map(|doc| doc.content.as_ref().unwrap().source.len())
        .sum();
    assert_eq!(source_bytes, target);
    let start = Instant::now();
    let snapshot = IndexSnapshot::build(&book).unwrap();
    let build = start.elapsed().as_secs_f64();
    let maps: usize = snapshot
        .blocks()
        .iter()
        .map(|block| block.source_map.len())
        .sum();
    let store = Arc::new(PgSearchIndex::new(pool));
    let lease = store.claim(Uuid::new_v4(), 300).await.unwrap().unwrap();
    let start = Instant::now();
    store.publish(&lease, &snapshot).await.unwrap();
    let publish = start.elapsed().as_secs_f64();
    use tracing::instrument::WithSubscriber;
    let subscriber = tracing::Dispatch::new(
        tracing_subscriber::fmt()
            .with_env_filter("specspace_infrastructure::search_index=debug")
            .without_time()
            .with_ansi(false)
            .finish(),
    );
    let start = Instant::now();
    assert_eq!(
        store
            .find(
                fixture.user,
                fixture.repository,
                &book.commit,
                "מחיקה",
                0,
                100
            )
            .with_subscriber(subscriber.clone())
            .await
            .unwrap()
            .total_matches,
        count
    );
    let cold = start.elapsed().as_secs_f64();
    assert!(cold < 2.0, "fresh-index cold-query regression: {cold:.3}s");
    assert_eq!(
        store
            .find(actors[1], fixture.repository, &book.commit, "מחיק", 0, 100)
            .with_subscriber(subscriber)
            .await
            .unwrap()
            .total_matches,
        count
    );
    let mut readers = tokio::task::JoinSet::new();
    for (actor, query) in
        actors
            .into_iter()
            .zip(["מחיקה", "מחיק", "מחי", "מח", "מְחיקה", "מ\u{200f}חיקה"])
    {
        let store = store.clone();
        let commit = book.commit.clone();
        let repository = fixture.repository;
        readers.spawn(async move {
            let mut durations = vec![];
            for _ in 0..20 {
                let start = Instant::now();
                let page = store
                    .find(actor, repository, &commit, query, 0, 100)
                    .await
                    .unwrap();
                assert_eq!(page.total_matches, 1000);
                assert_eq!(page.matches.len(), 100);
                durations.push(start.elapsed().as_secs_f64());
            }
            durations
        });
    }
    let mut durations = vec![];
    while let Some(reader) = readers.join_next().await {
        durations.extend(reader.unwrap());
    }
    durations.sort_by(f64::total_cmp);
    let p95 = durations[durations.len() * 95 / 100 - 1];
    println!(
        "source_bytes={source_bytes} documents={count} readers=6 samples={} map_bytes={maps} build_seconds={build:.3} publish_seconds={publish:.3} cold_store_seconds={cold:.3} warm_store_p95_seconds={p95:.3}",
        durations.len()
    );
    assert!(
        p95 < 1.0,
        "store-only feasibility target exceeded: {p95:.3}s; HTTP/browser gates remain separate"
    );
}

struct WorkerSource {
    organization: Uuid,
    repository: Uuid,
    book: std::sync::Arc<specspace_application::book::BookSnapshot>,
    mode: std::sync::atomic::AtomicU8,
    entered: tokio::sync::Notify,
}
#[async_trait::async_trait]
impl specspace_infrastructure::index_worker::IndexSource for WorkerSource {
    async fn load(
        &self,
        organization: Uuid,
        repository: Uuid,
        commit: specspace_git_adapter::CommitSha,
    ) -> Result<
        std::sync::Arc<specspace_application::book::BookSnapshot>,
        specspace_application::book::BookError,
    > {
        use specspace_application::book::BookError;
        if organization != self.organization
            || repository != self.repository
            || commit != self.book.commit
        {
            return Err(BookError::NotFound);
        }
        self.entered.notify_one();
        match self.mode.load(std::sync::atomic::Ordering::SeqCst) {
            1 => Err(BookError::Provider(
                specspace_git_adapter::GitError::RateLimited {
                    retry_after_seconds: Some(17),
                },
            )),
            2 => Err(BookError::NotFound),
            3 => std::future::pending().await,
            _ => Ok(self.book.clone()),
        }
    }
}
fn worker_source(fixture: &Fixture, mode: u8) -> std::sync::Arc<WorkerSource> {
    std::sync::Arc::new(WorkerSource {
        organization: fixture.organization,
        repository: fixture.repository,
        book: std::sync::Arc::new(book(SHA, "מְחיקה")),
        mode: std::sync::atomic::AtomicU8::new(mode),
        entered: tokio::sync::Notify::new(),
    })
}

#[sqlx::test(migrations = "../../migrations")]
async fn worker_honors_provider_delay_then_publishes_the_same_pinned_job(pool: PgPool) {
    use specspace_infrastructure::{
        index_worker::{IndexWorker, WorkResult},
        search_index::PgSearchIndex,
    };
    let fixture = create_fixture(&pool).await;
    let store = std::sync::Arc::new(PgSearchIndex::new(pool.clone()));
    let source = worker_source(&fixture, 1);
    let worker = IndexWorker::new(
        store.clone(),
        source.clone(),
        std::time::Duration::from_secs(30),
    )
    .unwrap();
    let WorkResult::Deferred(job) = worker.run_once().await.unwrap() else {
        panic!("expected deferred work");
    };
    let delayed:bool=sqlx::query_scalar("SELECT state='QUEUED' AND available_at>clock_timestamp()+interval '14 seconds' FROM jobs WHERE id=$1")
        .bind(job).fetch_one(&pool).await.unwrap();
    assert!(delayed);
    assert_eq!(worker.run_once().await.unwrap(), WorkResult::Idle);
    source.mode.store(0, std::sync::atomic::Ordering::SeqCst);
    sqlx::query("UPDATE jobs SET available_at=clock_timestamp()-interval '1 second' WHERE id=$1")
        .bind(job)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(worker.run_once().await.unwrap(), WorkResult::Completed(job));
    assert_eq!(
        store
            .find(
                fixture.user,
                fixture.repository,
                &source.book.commit,
                "מחיקה",
                0,
                100
            )
            .await
            .unwrap()
            .total_matches,
        1
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn worker_shutdown_abandons_its_lease_and_restart_recovers_without_duplicate_publication(
    pool: PgPool,
) {
    use specspace_infrastructure::{
        index_worker::{IndexWorker, WorkResult},
        search_index::PgSearchIndex,
    };
    let fixture = create_fixture(&pool).await;
    let store = std::sync::Arc::new(PgSearchIndex::new(pool.clone()));
    let source = worker_source(&fixture, 3);
    let worker = IndexWorker::new(
        store.clone(),
        source.clone(),
        std::time::Duration::from_secs(30),
    )
    .unwrap();
    let (stop, receiver) = tokio::sync::watch::channel(false);
    let running = tokio::spawn(async move { worker.run(receiver).await });
    source.entered.notified().await;
    stop.send(true).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(1), running)
        .await
        .unwrap()
        .unwrap();
    let (job, state): (Uuid, String) =
        sqlx::query_as("SELECT id,state FROM jobs WHERE organization_id=$1")
            .bind(fixture.organization)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(state, "RUNNING");
    sqlx::query("UPDATE jobs SET leased_until=clock_timestamp()-interval '1 second' WHERE id=$1")
        .bind(job)
        .execute(&pool)
        .await
        .unwrap();
    source.mode.store(0, std::sync::atomic::Ordering::SeqCst);
    let replacement = IndexWorker::new(store, source, std::time::Duration::from_secs(30)).unwrap();
    assert_eq!(
        replacement.run_once().await.unwrap(),
        WorkResult::Completed(job)
    );
    assert_eq!(replacement.run_once().await.unwrap(), WorkResult::Idle);
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM search_index_snapshots WHERE repository_id=$1 AND state='READY'",
    )
    .bind(fixture.repository)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn permanent_source_failures_become_explicit_terminal_index_errors(pool: PgPool) {
    use specspace_infrastructure::{
        index_worker::{IndexWorker, WorkResult},
        search_index::PgSearchIndex,
    };
    let fixture = create_fixture(&pool).await;
    let store = std::sync::Arc::new(PgSearchIndex::new(pool.clone()));
    let worker = IndexWorker::new(
        store,
        worker_source(&fixture, 2),
        std::time::Duration::from_secs(30),
    )
    .unwrap();
    assert!(matches!(
        worker.run_once().await.unwrap(),
        WorkResult::Failed(_)
    ));
    assert_eq!(worker.run_once().await.unwrap(), WorkResult::Idle);
    let (state, error): (String, Option<String>) = sqlx::query_as(
        "SELECT state,error_code FROM search_index_snapshots WHERE repository_id=$1",
    )
    .bind(fixture.repository)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (state.as_str(), error.as_deref()),
        ("FAILED", Some("SOURCE_NOT_AVAILABLE"))
    );
}

struct RejectIndexProvider;
#[async_trait::async_trait]
impl specspace_application::reader::BookProviderFactory for RejectIndexProvider {
    async fn provider(
        &self,
        _: Uuid,
    ) -> Result<
        std::sync::Arc<dyn specspace_git_adapter::GitProvider>,
        specspace_application::book::BookError,
    > {
        panic!("scoped cached source should not contact Git");
    }
}
#[sqlx::test(migrations = "../../migrations")]
async fn trusted_index_source_checks_organization_before_cache_and_keeps_browser_acl_current(
    pool: PgPool,
) {
    use specspace_infrastructure::index_worker::{GitIndexSource, IndexSource};
    let fixture = create_fixture(&pool).await;
    let factory = std::sync::Arc::new(RejectIndexProvider);
    let reader = std::sync::Arc::new(specspace_application::reader::BookReader::new(
        std::sync::Arc::new(
            specspace_infrastructure::authorization::PgReadAuthorization::new(pool.clone()),
        ),
        factory.clone(),
    ));
    let approved = std::sync::Arc::new(book(SHA, "מחיקה"));
    reader
        .remember_indexed_main(fixture.repository, approved.clone())
        .await;
    let source = GitIndexSource::new(pool.clone(), factory, Some(reader.clone()));
    assert!(matches!(
        source
            .load(Uuid::new_v4(), fixture.repository, approved.commit.clone())
            .await,
        Err(specspace_application::book::BookError::NotFound)
    ));
    let loaded = source
        .load(
            fixture.organization,
            fixture.repository,
            approved.commit.clone(),
        )
        .await
        .unwrap();
    assert!(std::sync::Arc::ptr_eq(&approved, &loaded));
    sqlx::query("DELETE FROM memberships WHERE user_id=$1")
        .bind(fixture.user)
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        reader
            .at(fixture.user, fixture.repository, approved.commit.clone())
            .await,
        Err(specspace_application::book::BookError::NotFound)
    ));
}
