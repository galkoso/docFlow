use specspace_document_engine::index::IndexedText;
use sqlx::PgPool;
use uuid::Uuid;

const SHA: &str = "1111111111111111111111111111111111111111";
const TEXT: &str = "alpha [x]*";

struct Fixture {
    organization: Uuid,
    repository: Uuid,
}

async fn fixture(pool: &PgPool) -> Fixture {
    let organization = Uuid::new_v4();
    let repository = Uuid::new_v4();
    sqlx::query("INSERT INTO organizations(id,name) VALUES($1,'Synthetic index')")
        .bind(organization)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO repositories(id,organization_id,provider,remote_id,installation_id) VALUES($1,$2,'GITHUB','42','7')")
        .bind(repository).bind(organization).execute(pool).await.unwrap();
    let fixture = Fixture {
        organization,
        repository,
    };
    pending(pool, &fixture, 1).await;
    fixture
}

async fn pending(pool: &PgPool, fixture: &Fixture, version: i32) {
    let job = Uuid::new_v4();
    sqlx::query("INSERT INTO jobs(id,organization_id,kind,payload) VALUES($1,$2,'INDEX_SNAPSHOT',$3)")
        .bind(job).bind(fixture.organization)
        .bind(serde_json::json!({"repository_id":fixture.repository,"snapshot_sha":SHA,"normalizer_version":version}))
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
    let fixture = fixture(&pool).await;
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
    let first = fixture(&pool).await;
    let second = fixture(&pool).await;
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
    let fixture = fixture(&pool).await;
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
