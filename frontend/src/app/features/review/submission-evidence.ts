import type { ReviewAccepted, ReviewDetail, ReviewJob, ReviewPublication, ReviewRequest } from '../../core/api/review-api';
import type { ReviewConfiguration } from '../../core/api/review-configuration-api';
import { digest, errorCode, integer, sha, uuid } from '../comparison/comparison-evidence';
import { validConfiguration } from './review-evidence';

export interface SubmissionContext { configuration: ReviewConfiguration; detail: ReviewDetail; body: Readonly<ReviewRequest>; }
/** This preflight only selects a saved version. The worker rechecks all source,
 * structure, media and authority before publishing; no approval is implied. */
export function submissionContext(configuration: ReviewConfiguration, detail: ReviewDetail, proposal: string): SubmissionContext | null {
  if (!validConfiguration(configuration, proposal) || !configuration.can_edit || !detail || detail.proposal_id !== proposal
    || detail.head_sha !== configuration.head_sha || detail.version !== configuration.proposal_version || detail.version >= Number.MAX_SAFE_INTEGER
    || detail.branch !== configuration.branch || detail.state !== configuration.state || detail.target_branch !== 'main'
    || detail.target_branch !== configuration.target_branch || !detail.capabilities?.can_edit || !sha(detail.head_sha)
    || !uuid(detail.author_id) || !uuid(detail.owner_id) || !uuid(detail.team_id) || !uuid(detail.repository_id)
    || typeof detail.description !== 'string' || Array.from(detail.description.trim()).length < 10
    || Array.from(detail.description).length > 10000 || detail.description.includes('\0')
    || detail.synchronization?.state !== 'in_sync' || detail.synchronization.actual_head_sha !== detail.head_sha
    || !sha(detail.synchronization.current_main_sha) || detail.synchronization.main_error_code != null) return null;
  const reviewers = configuration.reviewers;
  if (!reviewers.length || reviewers.some(item => !item.active || !item.qualified || item.reviewer_id === detail.author_id)
    || !reviewers.some(product => product.domain === 'PRODUCT' && reviewers.some(development =>
      development.domain === 'DEVELOPMENT' && development.reviewer_id !== product.reviewer_id))
    || configuration.settings.ui_impact === 'yes' && !reviewers.some(item => item.domain === 'DESIGN')) return null;
  return { configuration, detail, body: Object.freeze({ expected_head_sha: detail.head_sha,
    expected_target_sha: detail.synchronization.current_main_sha, expected_proposal_version: detail.version,
    expected_configuration_version: configuration.configuration_version, expected_policy_version: configuration.policy_version }) };
}
export function validAccepted(value: ReviewAccepted, proposal: string): boolean {
  return !!value && value.proposal_id === proposal && uuid(value.operation_id) && uuid(value.job_id);
}
export function validPublication(value: ReviewPublication | null | undefined, ticket: ReviewAccepted, request: Readonly<ReviewRequest>): boolean {
  return !!value && value.proposal_id === ticket.proposal_id && value.operation_id === ticket.operation_id && value.job_id === ticket.job_id
    && uuid(value.snapshot_id) && digest(value.fingerprint) && digest(value.evidence_digest) && typeof value.activated === 'boolean'
    && typeof value.replayed === 'boolean' && value.proposal_version === request.expected_proposal_version + Number(value.activated);
}
export function validReviewJob(value: ReviewJob, ticket: ReviewAccepted, request: Readonly<ReviewRequest>): boolean {
  return validObservedJob(value, ticket.proposal_id, ticket.job_id)
    && (value.state !== 'SUCCEEDED' || validPublication(value.review, ticket, request));
}
/** URL restoration observes an existing server-owned job without reconstructing
 * a POST body or accepting mutation authority from the URL. */
export function validObservedJob(value: ReviewJob, proposal: string, job: string): boolean {
  if (!value || value.kind !== 'SUBMIT_REVIEW' || value.job_id !== job || value.proposal_id !== proposal
    || !integer(value.attempts, 8) || value.error_code != null && !errorCode(value.error_code)
    || value.creation != null || value.save != null || value.save_failure != null || value.comparison != null) return false;
  if (value.state === 'SUCCEEDED') {
    const result = value.review;
    return value.attempts > 0 && value.phase === 'SNAPSHOT_READY' && value.error_code == null && !!result && result.proposal_id === proposal
      && result.job_id === job && uuid(result.operation_id) && uuid(result.snapshot_id) && digest(result.fingerprint) && digest(result.evidence_digest)
      && integer(result.proposal_version, Number.MAX_SAFE_INTEGER, 1) && typeof result.activated === 'boolean' && typeof result.replayed === 'boolean';
  }
  if (value.review != null || value.phase === 'SNAPSHOT_READY') return false;
  if (value.phase === 'RECONCILIATION_REQUIRED') return ['QUEUED', 'RUNNING', 'FAILED', 'CANCELLED'].includes(value.state);
  if (value.state === 'QUEUED') return ['QUEUED', 'RETRY_WAIT'].includes(value.phase);
  if (value.state === 'RUNNING') return value.attempts > 0 && value.phase === 'PREPARING';
  return ['FAILED', 'CANCELLED'].includes(value.state) && value.phase === 'FAILED';
}
