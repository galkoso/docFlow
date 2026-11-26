use super::*;
use specspace_application::{
    comparison_reader::RepositoryBinding,
    review_check_publication::{
        CheckInvalidation, CheckPublicationIntent, CheckPublicationSource, CheckPurpose,
    },
};
use specspace_domain::BranchName;
use specspace_git_adapter::checks::{CheckResult, CheckRunId};
use sqlx::FromRow;

#[derive(FromRow)]
struct Frozen {
    operation_id: Uuid,
    job_id: Uuid,
    organization_id: Uuid,
    proposal_id: Uuid,
    repository_id: Uuid,
    actor_id: Uuid,
    snapshot_id: Uuid,
    remote_id: i64,
    installation_id: i64,
    app_id: i64,
    branch: String,
    head_sha: String,
    target_sha: String,
    merge_base_sha: String,
    proposal_version: i64,
    configuration_version: i64,
    policy_version: i64,
    fingerprint: String,
    evidence_digest: String,
    authority_digest: String,
    request_hash: String,
    purpose: String,
    result: String,
    report_digest: Option<String>,
    invalidation_reason: Option<String>,
    target_run_id: Option<i64>,
    target_operation: Option<Uuid>,
}
pub(super) fn result(result: CheckResult) -> &'static str {
    match result {
        CheckResult::Pending => "PENDING",
        CheckResult::Failure => "FAILURE",
        CheckResult::Success => "SUCCESS",
    }
}
pub(super) async fn evaluation(
    conn: &mut PgConnection,
    org: Uuid,
    actor: Uuid,
    proposal: Uuid,
    snapshot: Uuid,
    operation: Uuid,
) -> Result<Option<CheckReservation>, CheckStoreError> {
    let Some(receipt) = load(conn, org, operation).await? else {
        return Ok(None);
    };
    let source = receipt.intent.source();
    if source.actor != actor
        || source.proposal != proposal
        || source.snapshot != snapshot
        || !matches!(receipt.intent.purpose(), CheckPurpose::Evaluation { .. })
    {
        return Err(CheckStoreError::IdempotencyMismatch);
    }
    Ok(Some(receipt))
}
pub(super) async fn load(
    conn: &mut PgConnection,
    org: Uuid,
    operation: Uuid,
) -> Result<Option<CheckReservation>, CheckStoreError> {
    let Some(row) = sqlx::query_as::<_, Frozen>(
        "SELECT * FROM review_check_intents WHERE organization_id=$1 AND operation_id=$2",
    )
    .bind(org)
    .bind(operation)
    .fetch_optional(conn)
    .await?
    else {
        return Ok(None);
    };
    let invalid = || CheckStoreError::InvalidRecord;
    let source = CheckPublicationSource {
        organization: row.organization_id,
        actor: row.actor_id,
        proposal: row.proposal_id,
        operation: row.operation_id,
        snapshot: row.snapshot_id,
        repository: RepositoryBinding {
            repository: row.repository_id,
            remote_id: u64::try_from(row.remote_id).map_err(|_| invalid())?,
            installation_id: u64::try_from(row.installation_id).map_err(|_| invalid())?,
        },
        app_id: u64::try_from(row.app_id).map_err(|_| invalid())?,
        branch: BranchName::parse(row.branch).map_err(|_| invalid())?,
        head: CommitSha::parse(row.head_sha).map_err(|_| invalid())?,
        target: CommitSha::parse(row.target_sha).map_err(|_| invalid())?,
        merge_base: CommitSha::parse(row.merge_base_sha).map_err(|_| invalid())?,
        proposal_version: row.proposal_version,
        configuration_version: row.configuration_version,
        policy_version: row.policy_version,
        fingerprint: row.fingerprint,
        evidence_digest: row.evidence_digest,
        authority_digest: row.authority_digest,
    };
    let result = match row.result.as_str() {
        "PENDING" => CheckResult::Pending,
        "FAILURE" => CheckResult::Failure,
        "SUCCESS" => CheckResult::Success,
        _ => return Err(invalid()),
    };
    let purpose = match row.purpose.as_str() {
        "EVALUATION"
            if row.invalidation_reason.is_none()
                && row.target_run_id.is_none()
                && row.target_operation.is_none() =>
        {
            CheckPurpose::Evaluation {
                report_digest: row.report_digest.ok_or_else(invalid)?,
            }
        }
        "INVALIDATION" if row.report_digest.is_none() => {
            let reason = match row.invalidation_reason.as_deref() {
                Some("HEAD_CHANGED") => CheckInvalidation::HeadChanged,
                Some("TARGET_CHANGED") => CheckInvalidation::TargetChanged,
                Some("POLICY_CHANGED") => CheckInvalidation::PolicyChanged,
                Some("AUTHORITY_CHANGED") => CheckInvalidation::AuthorityChanged,
                Some("SNAPSHOT_CHANGED") => CheckInvalidation::SnapshotChanged,
                Some("BLOCKERS_CHANGED") => CheckInvalidation::BlockersChanged,
                Some("SUPERSEDED") => CheckInvalidation::Superseded,
                _ => return Err(invalid()),
            };
            CheckPurpose::Invalidation {
                reason,
                run: CheckRunId::new(
                    u64::try_from(row.target_run_id.ok_or_else(invalid)?).map_err(|_| invalid())?,
                )
                .map_err(|_| invalid())?,
                check_operation: row.target_operation.ok_or_else(invalid)?,
            }
        }
        _ => return Err(invalid()),
    };
    let intent = CheckPublicationIntent::restore(source, purpose, result, &row.request_hash)
        .map_err(|_| invalid())?;
    Ok(Some(CheckReservation {
        job_id: row.job_id,
        replayed: true,
        intent,
    }))
}
