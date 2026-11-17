//! Bounded read-only orchestration. A returned candidate still requires media
//! verification and fenced durable publication; it is not an approved snapshot.
use crate::{
    comparison_reader::{
        BoundProviderFactory, ComparisonReadError, ComparisonReader, LoadedComparison, Selection,
    },
    review::{PreparedReview, ReviewError},
    review_context::{ReviewAuthorization, ReviewContextError, ReviewExpectation, ReviewScope},
};
use specspace_domain::policy::{self, Action, ResourceAccess};
use specspace_git_adapter::GitError;
use std::{sync::Arc, time::Duration};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use uuid::Uuid;
mod media;
pub use media::{MEDIA_VALIDATION_VERSION, VerifiedMedia, VerifiedReview};

#[derive(Debug)]
pub enum ReviewPreparationError {
    Context(ReviewContextError),
    Comparison(ComparisonReadError),
    Plan(ReviewError),
    Provider(GitError),
    ContextChanged,
    HeadChanged,
    TargetChanged,
    Busy,
    TimedOut,
    ProcessingUnavailable,
    InvalidMedia { path: String, code: &'static str },
}
pub struct ReviewCandidate {
    scope: ReviewScope,
    expectation: ReviewExpectation,
    comparison: Arc<LoadedComparison>,
    plan: PreparedReview,
}
impl ReviewCandidate {
    pub fn scope(&self) -> &ReviewScope {
        &self.scope
    }
    pub fn comparison(&self) -> &LoadedComparison {
        &self.comparison
    }
    pub fn plan(&self) -> &PreparedReview {
        &self.plan
    }
}
pub struct ReviewPreparer {
    authorization: Arc<dyn ReviewAuthorization>,
    reader: Arc<ComparisonReader>,
    source: Arc<dyn BoundProviderFactory>,
    loads: Arc<Semaphore>,
    timeout: Duration,
}
impl ReviewPreparer {
    /// Bind a worker's lease-specific authority while retaining process-wide
    /// IO/CPU capacity and the same frozen provider factory.
    pub fn with_authorization(&self, authorization: Arc<dyn ReviewAuthorization>) -> Self {
        Self {
            authorization,
            reader: self.reader.clone(),
            source: self.source.clone(),
            loads: self.loads.clone(),
            timeout: self.timeout,
        }
    }
    pub fn new(
        authorization: Arc<dyn ReviewAuthorization>,
        reader: Arc<ComparisonReader>,
        source: Arc<dyn BoundProviderFactory>,
        timeout: Duration,
    ) -> Result<Self, ReviewPreparationError> {
        if !(Duration::from_secs(30)..=Duration::from_secs(900)).contains(&timeout) {
            return Err(ReviewPreparationError::ProcessingUnavailable);
        }
        Ok(Self {
            authorization,
            reader,
            source,
            loads: Arc::new(Semaphore::new(2)),
            timeout,
        })
    }
    pub async fn prepare(
        &self,
        actor: Uuid,
        proposal: Uuid,
        expectation: ReviewExpectation,
    ) -> Result<ReviewCandidate, ReviewPreparationError> {
        let permit = self
            .loads
            .clone()
            .try_acquire_owned()
            .map_err(|_| ReviewPreparationError::Busy)?;
        tokio::time::timeout(
            self.timeout,
            self.prepare_inner(actor, proposal, expectation, permit),
        )
        .await
        .map_err(|_| ReviewPreparationError::TimedOut)?
    }
    async fn prepare_inner(
        &self,
        actor: Uuid,
        proposal: Uuid,
        expectation: ReviewExpectation,
        permit: OwnedSemaphorePermit,
    ) -> Result<ReviewCandidate, ReviewPreparationError> {
        let scope = self
            .authorization
            .resolve(actor, proposal, &expectation)
            .await
            .map_err(ReviewPreparationError::Context)?;
        validate_scope(actor, proposal, &expectation, &scope)?;
        let result = self
            .reader
            .load(
                actor,
                proposal,
                Selection {
                    head: Some(expectation.head.clone()),
                    target: Some(expectation.target.clone()),
                },
            )
            .await;
        // Recheck full edit/reviewer metadata even when the read-only comparison
        // returned an error. Its own authorization covers read scope only.
        self.check(&scope, &expectation).await?;
        let loaded = Arc::new(result.map_err(ReviewPreparationError::Comparison)?);
        if !loaded.matches_context(&scope.comparison) {
            return Err(ReviewPreparationError::ContextChanged);
        }
        let cpu_loaded = loaded.clone();
        let context = scope.review.clone();
        let (result, _permit) = tokio::task::spawn_blocking(move || {
            // Keep capacity through CPU work if the caller times out/cancels.
            (PreparedReview::prepare(context, &cpu_loaded), permit)
        })
        .await
        .map_err(|_| ReviewPreparationError::ProcessingUnavailable)?;
        // A validation error can include private path diagnostics. Do not return
        // it if current authority or configuration changed during processing.
        self.check(&scope, &expectation).await?;
        let plan = result.map_err(ReviewPreparationError::Plan)?;
        self.refs(&scope, &expectation).await?;
        Ok(ReviewCandidate {
            scope,
            expectation,
            comparison: loaded,
            plan,
        })
    }
    /// Recheck after later media work, before attempting a fenced DB publish.
    /// This is a fresh observation, not a lock or guarantee against future Git IO.
    pub async fn recheck(&self, candidate: &ReviewCandidate) -> Result<(), ReviewPreparationError> {
        let _permit = self
            .loads
            .clone()
            .try_acquire_owned()
            .map_err(|_| ReviewPreparationError::Busy)?;
        tokio::time::timeout(self.timeout, async {
            self.check(&candidate.scope, &candidate.expectation).await?;
            self.refs(&candidate.scope, &candidate.expectation).await
        })
        .await
        .map_err(|_| ReviewPreparationError::TimedOut)?
    }
    async fn check(
        &self,
        scope: &ReviewScope,
        expectation: &ReviewExpectation,
    ) -> Result<(), ReviewPreparationError> {
        let fresh = self
            .authorization
            .resolve(
                scope.comparison.actor,
                scope.comparison.proposal,
                expectation,
            )
            .await
            .map_err(ReviewPreparationError::Context)?;
        if fresh != *scope {
            return Err(ReviewPreparationError::ContextChanged);
        }
        Ok(())
    }
    async fn refs(
        &self,
        scope: &ReviewScope,
        expectation: &ReviewExpectation,
    ) -> Result<(), ReviewPreparationError> {
        self.checked_provider(scope, expectation).await.map(|_| ())
    }
    async fn checked_provider(
        &self,
        scope: &ReviewScope,
        expectation: &ReviewExpectation,
    ) -> Result<Arc<dyn specspace_git_adapter::GitProvider>, ReviewPreparationError> {
        let result = self
            .source
            .provider_bound(&scope.comparison.connection)
            .await;
        self.check(scope, expectation).await?;
        let provider = result.map_err(ReviewPreparationError::Comparison)?;
        let (head, target) = tokio::join!(
            provider.branch_head(&scope.comparison.branch),
            provider.main_head()
        );
        self.check(scope, expectation).await?;
        if head.map_err(ReviewPreparationError::Provider)? != expectation.head {
            return Err(ReviewPreparationError::HeadChanged);
        }
        if target.map_err(ReviewPreparationError::Provider)? != expectation.target {
            return Err(ReviewPreparationError::TargetChanged);
        }
        Ok(provider)
    }
}
fn validate_scope(
    actor: Uuid,
    proposal: Uuid,
    expectation: &ReviewExpectation,
    scope: &ReviewScope,
) -> Result<(), ReviewPreparationError> {
    let read = &scope.comparison;
    let review = &scope.review;
    if !matches!(
        read.state.as_str(),
        "DRAFT" | "IN_REVIEW" | "CHANGES_REQUESTED" | "APPROVED"
    ) || read.actor != actor
        || read.proposal != proposal
        || review.actor.user_id != actor
        || review.proposal != proposal
        || review.actor.organization_id != read.organization
        || review.repository != read.connection.repository
        || read.recorded_head != expectation.head
        || read.version != expectation.proposal_version
        || review.expected_head != expectation.head
        || review.expected_target != expectation.target
        || review.expected_version != expectation.proposal_version
        || review.configuration_version != expectation.configuration_version
        || review.policy_version != expectation.policy_version
        || !policy::authorized(
            &review.actor,
            &ResourceAccess {
                organization_id: read.organization,
                repository_access: review.repository_access,
                assigned_editor: review.assigned_editor,
                export_allowed: false,
            },
            Action::Edit,
        )
    {
        return Err(ReviewPreparationError::ContextChanged);
    }
    Ok(())
}
