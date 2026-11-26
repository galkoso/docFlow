//! Immutable server publication intent and recovery decisions. Construction is
//! not authorization: a durable journal must fence every provider side effect.
use crate::{
    book::BookError,
    comparison_reader::RepositoryBinding,
    review_readiness::{ReadinessConclusion, ReadinessGate, ReviewReadiness},
};
use async_trait::async_trait;
use serde_json::json;
use sha2::{Digest, Sha256};
use specspace_domain::BranchName;
use specspace_git_adapter::{
    CommitSha,
    checks::{
        CHECK_NAME, CheckResult, CheckRun, CheckRunId, CheckScope, CheckState, MAX_CHECK_PAGES,
        ReviewCheckProvider,
    },
};
use std::{collections::HashSet, sync::Arc};
use uuid::Uuid;

#[derive(Debug, PartialEq, Eq)]
pub enum CheckProviderError {
    BindingChanged,
    Source(BookError),
}
#[async_trait]
pub trait CheckProviderFactory: Send + Sync {
    /// Connect to the frozen server binding, not a second mutable lookup. The
    /// caller must recheck authority/configuration after connecting.
    async fn check_provider_bound(
        &self,
        binding: &RepositoryBinding,
    ) -> Result<Arc<dyn ReviewCheckProvider>, CheckProviderError>;
}
#[derive(Clone, PartialEq, Eq)]
pub struct CheckPublicationSource {
    pub organization: Uuid,
    pub actor: Uuid,
    pub proposal: Uuid,
    pub operation: Uuid,
    pub snapshot: Uuid,
    pub repository: RepositoryBinding,
    pub app_id: u64,
    pub branch: BranchName,
    pub head: CommitSha,
    pub target: CommitSha,
    pub merge_base: CommitSha,
    pub proposal_version: i64,
    pub configuration_version: i64,
    pub policy_version: i64,
    pub fingerprint: String,
    pub evidence_digest: String,
    pub authority_digest: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckInvalidation {
    HeadChanged,
    TargetChanged,
    PolicyChanged,
    AuthorityChanged,
    SnapshotChanged,
    BlockersChanged,
    Superseded,
}
impl CheckInvalidation {
    pub fn code(self) -> &'static str {
        match self {
            Self::HeadChanged => "HEAD_CHANGED",
            Self::TargetChanged => "TARGET_CHANGED",
            Self::PolicyChanged => "POLICY_CHANGED",
            Self::AuthorityChanged => "AUTHORITY_CHANGED",
            Self::SnapshotChanged => "SNAPSHOT_CHANGED",
            Self::BlockersChanged => "BLOCKERS_CHANGED",
            Self::Superseded => "SUPERSEDED",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckPurpose {
    Evaluation {
        report_digest: String,
    },
    Invalidation {
        reason: CheckInvalidation,
        run: CheckRunId,
        check_operation: Uuid,
    },
}
#[derive(Clone)]
pub struct CheckPublicationIntent {
    source: CheckPublicationSource,
    purpose: CheckPurpose,
    result: CheckResult,
    request_hash: String,
}
#[derive(Debug, PartialEq, Eq)]
pub enum CheckPublicationError {
    InvalidSource,
    InvalidReport,
    InvalidIntent,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckPublicationPhase {
    NotStarted,
    CreateStarted,
    Created(CheckRunId),
    UpdateStarted(CheckRunId),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckPublicationDecision {
    CreatePending,
    RecordPending(CheckRunId),
    Update(CheckRunId, CheckResult),
    Published(CheckRunId),
    ObserveAgain,
    Conflict,
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn hash(value: serde_json::Value) -> String {
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&value).expect("fixed serializable metadata"))
    )
}
fn valid_source(s: &CheckPublicationSource) -> Result<(), CheckPublicationError> {
    if [
        s.organization,
        s.actor,
        s.proposal,
        s.operation,
        s.snapshot,
        s.repository.repository,
    ]
    .iter()
    .any(Uuid::is_nil)
        || s.repository.installation_id == 0
        || s.repository.installation_id > i64::MAX as u64
        || CheckScope::new(s.repository.remote_id, s.app_id, s.head.clone()).is_err()
        || [
            s.proposal_version,
            s.configuration_version,
            s.policy_version,
        ]
        .iter()
        .any(|v| *v < 1)
        || ![&s.fingerprint, &s.evidence_digest, &s.authority_digest]
            .into_iter()
            .all(|v| digest(v))
    {
        return Err(CheckPublicationError::InvalidSource);
    }
    Ok(())
}
impl CheckPublicationIntent {
    pub fn from_readiness(
        source: CheckPublicationSource,
        report: &ReviewReadiness,
    ) -> Result<Self, CheckPublicationError> {
        valid_source(&source)?;
        if report.actor_id != source.actor
            || report.proposal_id != source.proposal
            || report.snapshot_id != source.snapshot
            || report.fingerprint != source.fingerprint
            || report.head_sha != source.head.as_str()
            || report.target_sha != source.target.as_str()
            || report.policy_version != source.policy_version
            || report.configuration_version != source.configuration_version
            || !report.provider_refs_verified
            || report.evaluation.issues.len() > 20
        {
            return Err(CheckPublicationError::InvalidReport);
        }
        let mut issues = HashSet::new();
        for issue in &report.evaluation.issues {
            if !matches!(
                issue.conclusion,
                ReadinessConclusion::Failure | ReadinessConclusion::Pending
            ) || issue.code.is_empty()
                || issue.code.len() > 80
                || !issue
                    .code
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
                || issue.code == "PROVIDER_REFS_UNVERIFIED"
                || !issues.insert((format!("{:?}", issue.gate), &issue.code))
            {
                return Err(CheckPublicationError::InvalidReport);
            }
        }
        if !report.current_for_metadata
            && !report.evaluation.issues.iter().any(|i| {
                i.gate == ReadinessGate::Snapshot && i.conclusion == ReadinessConclusion::Failure
            })
        {
            return Err(CheckPublicationError::InvalidReport);
        }
        let result = if report
            .evaluation
            .issues
            .iter()
            .any(|i| i.conclusion == ReadinessConclusion::Failure)
        {
            CheckResult::Failure
        } else if report.evaluation.issues.is_empty() {
            CheckResult::Success
        } else {
            CheckResult::Pending
        };
        if !matches!(
            (&report.evaluation.conclusion, result),
            (ReadinessConclusion::Failure, CheckResult::Failure)
                | (ReadinessConclusion::Pending, CheckResult::Pending)
                | (ReadinessConclusion::Success, CheckResult::Success)
        ) {
            return Err(CheckPublicationError::InvalidReport);
        }
        let purpose = CheckPurpose::Evaluation {
            report_digest: hash(
                serde_json::to_value(report).map_err(|_| CheckPublicationError::InvalidReport)?,
            ),
        };
        Ok(Self::build(source, purpose, result))
    }
    /// Invalidation is always failure. Its journal must resolve the existing run
    /// independently; stale approvals are never needed to revoke old success.
    pub fn invalidate(
        source: CheckPublicationSource,
        reason: CheckInvalidation,
        run: CheckRunId,
        check_operation: Uuid,
    ) -> Result<Self, CheckPublicationError> {
        valid_source(&source)?;
        if check_operation.is_nil() || check_operation == source.operation {
            return Err(CheckPublicationError::InvalidIntent);
        }
        Ok(Self::build(
            source,
            CheckPurpose::Invalidation {
                reason,
                run,
                check_operation,
            },
            CheckResult::Failure,
        ))
    }
    fn build(source: CheckPublicationSource, purpose: CheckPurpose, result: CheckResult) -> Self {
        let request_hash = intent_hash(&source, &purpose, result);
        Self {
            source,
            purpose,
            result,
            request_hash,
        }
    }
    pub fn restore(
        source: CheckPublicationSource,
        purpose: CheckPurpose,
        result: CheckResult,
        request_hash: &str,
    ) -> Result<Self, CheckPublicationError> {
        valid_source(&source)?;
        match &purpose {
            CheckPurpose::Evaluation { report_digest } if digest(report_digest) => {}
            CheckPurpose::Invalidation {
                check_operation, ..
            } if result == CheckResult::Failure
                && !check_operation.is_nil()
                && *check_operation != source.operation => {}
            _ => return Err(CheckPublicationError::InvalidIntent),
        }
        let intent = Self::build(source, purpose, result);
        if !digest(request_hash) || intent.request_hash != request_hash {
            return Err(CheckPublicationError::InvalidIntent);
        }
        Ok(intent)
    }
    pub fn source(&self) -> &CheckPublicationSource {
        &self.source
    }
    pub fn purpose(&self) -> &CheckPurpose {
        &self.purpose
    }
    pub fn check_operation(&self) -> Uuid {
        match self.purpose {
            CheckPurpose::Evaluation { .. } => self.source.operation,
            CheckPurpose::Invalidation {
                check_operation, ..
            } => check_operation,
        }
    }
    pub fn result(&self) -> CheckResult {
        self.result
    }
    pub fn request_hash(&self) -> &str {
        &self.request_hash
    }
    pub fn scope(&self) -> CheckScope {
        CheckScope::new(
            self.source.repository.remote_id,
            self.source.app_id,
            self.source.head.clone(),
        )
        .expect("validated source")
    }
    /// Supply a complete, bounded discovery set. The journal separately fences
    /// active evaluations and invalidation against concurrent publication.
    pub fn decide(
        &self,
        observed: &[CheckRun],
        complete: bool,
        phase: CheckPublicationPhase,
    ) -> CheckPublicationDecision {
        use CheckPublicationDecision as D;
        use CheckPublicationPhase as P;
        if observed.len() > usize::from(MAX_CHECK_PAGES) * 100 {
            return D::Conflict;
        }
        if !complete {
            return D::ObserveAgain;
        }
        let scope = self.scope();
        let mut ids = HashSet::new();
        let mut own = None;
        for run in observed {
            if run.scope != scope || run.operation.is_nil() || !ids.insert(run.id) {
                return D::Conflict;
            }
            if run.operation == self.check_operation() && own.replace(run).is_some() {
                return D::Conflict;
            }
        }
        let Some(run) = own else {
            return if phase == P::NotStarted
                && matches!(self.purpose, CheckPurpose::Evaluation { .. })
            {
                D::CreatePending
            } else {
                D::ObserveAgain
            };
        };
        if let CheckPurpose::Invalidation { run: expected, .. } = self.purpose {
            if run.id != expected {
                return D::Conflict;
            }
            return match phase {
                P::NotStarted => D::Update(run.id, CheckResult::Failure),
                P::UpdateStarted(id) if id == run.id && run.state == CheckState::Failure => {
                    D::Published(id)
                }
                P::UpdateStarted(id) if id == run.id => D::ObserveAgain,
                _ => D::Conflict,
            };
        }
        match phase {
            P::NotStarted => D::Conflict,
            P::CreateStarted if run.state == CheckState::Pending => D::RecordPending(run.id),
            P::CreateStarted => D::Conflict,
            P::Created(id) if run.id != id => D::Conflict,
            P::Created(id) if run.state == CheckState::Pending => D::Update(id, self.result),
            P::Created(_) => D::Conflict,
            P::UpdateStarted(id) if run.id != id => D::Conflict,
            P::UpdateStarted(id) if run.state == CheckState::from(self.result) => D::Published(id),
            P::UpdateStarted(_) => D::ObserveAgain,
        }
    }
}
fn intent_hash(s: &CheckPublicationSource, purpose: &CheckPurpose, result: CheckResult) -> String {
    let purpose = match purpose {
        CheckPurpose::Evaluation { report_digest } => json!(["EVALUATION", report_digest]),
        CheckPurpose::Invalidation {
            reason,
            run,
            check_operation,
        } => json!([
            "INVALIDATION",
            reason.code(),
            run.get().to_string(),
            check_operation
        ]),
    };
    hash(json!([
        "specspace-review-check-intent-v1",
        CHECK_NAME,
        [s.organization, s.actor, s.proposal, s.operation, s.snapshot],
        [
            s.repository.repository.to_string(),
            s.repository.remote_id.to_string(),
            s.repository.installation_id.to_string(),
            s.app_id.to_string(),
            s.branch.as_str().to_owned()
        ],
        [s.head.as_str(), s.target.as_str(), s.merge_base.as_str()],
        [
            s.proposal_version.to_string(),
            s.configuration_version.to_string(),
            s.policy_version.to_string()
        ],
        [&s.fingerprint, &s.evidence_digest, &s.authority_digest],
        purpose,
        format!("{result:?}")
    ]))
}

#[cfg(test)]
mod tests;
