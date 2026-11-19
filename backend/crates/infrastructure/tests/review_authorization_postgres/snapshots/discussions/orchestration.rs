use super::*;
use specspace_application::comparison_file::{
    ComparisonFileAuthorization, FileReadContext, FileReadError,
};

struct CompletingFailure {
    pool: PgPool,
    reader: ComparisonFileReader,
    request: OpenThreadRequest,
    receipt: Mutex<Option<DiscussionRecorded>>,
}
#[async_trait]
impl ComparisonFileAuthorization for CompletingFailure {
    async fn resolve_file(
        &self,
        actor: Uuid,
        proposal: Uuid,
        _snapshot: Uuid,
        _file: &str,
    ) -> Result<FileReadContext, FileReadError> {
        // Simulate a slower duplicate read failing after an overlapping request
        // has already committed this exact intent through the real reader.
        let receipt = PgDiscussions::new(self.pool.clone())
            .open_thread(actor, proposal, self.request.clone(), &self.reader)
            .await
            .unwrap();
        *self.receipt.lock().unwrap() = Some(receipt);
        Err(FileReadError::Provider(GitError::ProviderUnavailable))
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn source_orchestration_returns_concurrent_receipt_after_a_late_duplicate_read_failure(
    pool: PgPool,
) {
    let r = review(&pool).await;
    let authority = Arc::new(CompletingFailure {
        pool: pool.clone(),
        reader: r.reader,
        request: r.request.clone(),
        receipt: Mutex::new(None),
    });
    let reader = ComparisonFileReader::new(
        authority.clone(),
        Arc::new(Factory {
            source: r.source.clone(),
            repository: r.f.repository,
        }),
        Duration::from_secs(30),
    )
    .unwrap();
    let replay = PgDiscussions::new(pool.clone())
        .open_thread(r.f.product, r.f.proposal, r.request, &reader)
        .await
        .unwrap();
    let receipt = authority.receipt.lock().unwrap().take().unwrap();
    assert_eq!(receipt.id, replay.id);
    assert!(!receipt.replayed && replay.replayed);
    let counts:(i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM discussion_threads),(SELECT count(*) FROM discussion_attempts),(SELECT count(*) FROM audit_events WHERE action='DISCUSSION_SOURCE_STOPPED')").fetch_one(&pool).await.unwrap();
    assert_eq!(counts, (1, 2, 1));
}
