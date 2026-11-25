//! Read-only provider observation. A fresh report is neither a durable required
//! check nor a merge permit. No database transaction spans provider IO.
use super::{PgReviewReadiness, ReadinessError, report::CapturedReadiness};
use specspace_application::{
    comparison_reader::{BoundProviderFactory, ComparisonReadError},
    review_readiness::{ReadinessConclusion, ReadinessGate, ReviewReadiness},
};
use specspace_git_adapter::{CommitSha, GitError, GitProvider};
use std::time::Duration;
use uuid::Uuid;

#[derive(Debug, PartialEq, Eq)]
pub enum FreshReadinessError {
    Stored(ReadinessError),
    Connection(ComparisonReadError),
    Provider(GitError),
    ContextChanged,
    HeadChanged,
    TargetChanged,
    MergeBaseChanged,
    Busy,
    TimedOut,
}
impl From<ReadinessError> for FreshReadinessError {
    fn from(value: ReadinessError) -> Self {
        Self::Stored(value)
    }
}
impl PgReviewReadiness {
    /// Server-owned inputs only. Always observes live refs; never caches a
    /// successful report. Publication and merge must recheck their own fences.
    pub async fn fresh(
        &self,
        actor: Uuid,
        proposal: Uuid,
        snapshot: Uuid,
        source: &dyn BoundProviderFactory,
    ) -> Result<ReviewReadiness, FreshReadinessError> {
        let _permit = self
            .fresh_loads
            .try_acquire()
            .map_err(|_| FreshReadinessError::Busy)?;
        tokio::time::timeout(
            Duration::from_secs(30),
            self.fresh_inner(actor, proposal, snapshot, source),
        )
        .await
        .map_err(|_| FreshReadinessError::TimedOut)?
    }
    async fn fresh_inner(
        &self,
        actor: Uuid,
        proposal: Uuid,
        snapshot: Uuid,
        source: &dyn BoundProviderFactory,
    ) -> Result<ReviewReadiness, FreshReadinessError> {
        let initial = self.capture(actor, proposal, snapshot, true).await?;
        let context = initial
            .context
            .as_ref()
            .ok_or(ReadinessError::Unavailable)?;
        let connection = source.provider_bound(&context.connection).await;
        // Recheck access and binding even after an unsuccessful connection.
        let connected = self.capture(actor, proposal, snapshot, true).await?;
        unchanged(&initial, &connected)?;
        let provider = connection.map_err(FreshReadinessError::Connection)?;
        let observed = observe(provider.as_ref(), &initial).await;
        // Latest policy, reviewer authority, effective votes and blockers are
        // assembled after IO. A lost reader cannot receive even diagnostics.
        let mut final_read = self.capture(actor, proposal, snapshot, true).await?;
        unchanged(&initial, &final_read)?;
        observed?;
        let report = &mut final_read.report;
        report.provider_refs_verified = true;
        report.evaluation.issues.retain(|i| {
            !(i.gate == ReadinessGate::Evidence && i.code == "PROVIDER_REFS_UNVERIFIED")
        });
        report.evaluation.conclusion = if report
            .evaluation
            .issues
            .iter()
            .any(|i| i.conclusion == ReadinessConclusion::Failure)
        {
            ReadinessConclusion::Failure
        } else if report.evaluation.issues.is_empty() {
            ReadinessConclusion::Success
        } else {
            ReadinessConclusion::Pending
        };
        Ok(final_read.report)
    }
}
fn unchanged(a: &CapturedReadiness, b: &CapturedReadiness) -> Result<(), FreshReadinessError> {
    let x = &a.report;
    let y = &b.report;
    if a.context != b.context
        || a.merge_base != b.merge_base
        || x.fingerprint != y.fingerprint
        || x.head_sha != y.head_sha
        || x.target_sha != y.target_sha
        || x.policy_version != y.policy_version
        || x.configuration_version != y.configuration_version
        || x.current_for_metadata != y.current_for_metadata
    {
        return Err(FreshReadinessError::ContextChanged);
    }
    Ok(())
}
async fn observe(
    provider: &dyn GitProvider,
    capture: &CapturedReadiness,
) -> Result<(), FreshReadinessError> {
    let context = capture
        .context
        .as_ref()
        .ok_or(ReadinessError::Unavailable)?;
    let head =
        CommitSha::parse(&capture.report.head_sha).map_err(|_| ReadinessError::Unavailable)?;
    let target =
        CommitSha::parse(&capture.report.target_sha).map_err(|_| ReadinessError::Unavailable)?;
    let check_refs = async || {
        let (h, t) = tokio::join!(provider.branch_head(&context.branch), provider.main_head());
        if h.map_err(FreshReadinessError::Provider)? != head {
            return Err(FreshReadinessError::HeadChanged);
        }
        if t.map_err(FreshReadinessError::Provider)? != target {
            return Err(FreshReadinessError::TargetChanged);
        }
        Ok(())
    };
    check_refs().await?;
    let base = provider
        .merge_base(&target, &head)
        .await
        .map_err(FreshReadinessError::Provider)?;
    if base.as_str() != capture.merge_base {
        return Err(FreshReadinessError::MergeBaseChanged);
    }
    // Detect ref movement while the provider calculated the immutable base.
    check_refs().await
}
