//! Shared read models for stored PR synchronization evidence. These models grant
//! no mutation authority and do not assert live provider state or merge readiness.
use serde::Serialize;
use uuid::Uuid;
#[derive(Clone, Copy, Serialize, PartialEq, Eq, Debug)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum ReviewPrDispatchPhase {
    Awaiting,
    Waiting,
    Enqueued,
    Skipped,
    Rejected,
    Untracked,
}
#[derive(Clone, Copy, Serialize, PartialEq, Eq, Debug)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum ReviewPrSyncPhase {
    Queued,
    Observing,
    RetryWait,
    Reconciling,
    ObservedSynced,
    Superseded,
    Failed,
}
#[derive(Clone, Copy, Serialize, PartialEq, Eq, Debug)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum ReviewPrReason {
    Busy,
    Quota,
    AuthorityRequired,
    SetupRequired,
    SourceChanged,
    BindingChanged,
    InvalidPublication,
    InvalidProjection,
    ProviderUnavailable,
    ProviderRateLimit,
    ProviderRejected,
    Conflict,
    UncertainResult,
    ObservationPending,
    Other,
}
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReviewPrDispatchStatus {
    pub phase: ReviewPrDispatchPhase,
    pub reason: Option<ReviewPrReason>,
    pub next_attempt_at: Option<String>,
}
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReviewPrSyncStatus {
    pub phase: ReviewPrSyncPhase,
    pub reason: Option<ReviewPrReason>,
    /// Planned automatic work, not proof that a worker/provider is available.
    pub automatic: bool,
    pub next_attempt_at: Option<String>,
}
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReviewPrObservation {
    /// Decimal string avoids losing provider identity in JavaScript numbers.
    pub number: String,
    pub url: String,
    pub snapshot_id: Uuid,
    pub head_sha: String,
    pub target_sha: String,
    /// Database receipt time, not the time of a new provider read by this GET.
    pub recorded_at: String,
    pub superseded: bool,
}
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReviewPrStatus {
    pub proposal_id: Uuid,
    pub snapshot_id: Uuid,
    pub fingerprint: String,
    pub head_sha: String,
    pub target_sha: String,
    /// Current DB metadata/editable state only; provider refs are not read here.
    pub current_for_sync: bool,
    pub binding_current: bool,
    pub read_at: String,
    pub dispatch: ReviewPrDispatchStatus,
    pub sync: Option<ReviewPrSyncStatus>,
    /// Any unresolved started PR write for this proposal, including older work.
    pub uncertain_write: bool,
    /// Last stored observation for this PR; it may refer to another snapshot.
    pub pr: Option<ReviewPrObservation>,
}
