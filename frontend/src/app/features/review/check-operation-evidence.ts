import {
  CHECK_PAGE_SIZE, type CheckOperation, type CheckOperationPage, type CheckRecoveryAccepted, type CheckRecoveryRequest,
} from '../../core/api/check-operations-api';
import { digest, integer, uuid } from '../comparison/comparison-evidence';

const jobs = ['QUEUED', 'RUNNING', 'SUCCEEDED', 'FAILED', 'CANCELLED'];
const states = ['PENDING', 'RUNNING', 'SUCCEEDED', 'FAILED', 'UNKNOWN'];
const phases = ['NOT_STARTED', 'CREATE_STARTED', 'CREATED', 'UPDATE_STARTED'];
export function validCheckOperation(v: CheckOperation, actor: string, proposal: string, operation: string, organization?: string): boolean {
  if (!v || v.viewer_id !== actor || v.proposal_id !== proposal || v.operation_id !== operation || !uuid(v.operation_id)
    || !uuid(v.organization_id) || organization != null && v.organization_id !== organization || !uuid(v.snapshot_id) || !digest(v.request_hash)
    || !jobs.includes(v.job_state) || !states.includes(v.operation_state) || !phases.includes(v.write_phase)
    || !['EVALUATION', 'INVALIDATION'].includes(v.purpose) || !['PENDING', 'FAILURE', 'SUCCESS'].includes(v.desired_result)
    || v.purpose === 'INVALIDATION' && v.desired_result !== 'FAILURE' || v.provider_verified !== false
    || !integer(v.max_attempts, 2147483647, 1) || !integer(v.attempts, v.max_attempts)
    || typeof v.known_run !== 'boolean' || typeof v.published !== 'boolean'
    || v.error_code != null && (typeof v.error_code !== 'string' || !/^[A-Z0-9_]{1,80}$/.test(v.error_code))) return false;
  const started = v.write_phase !== 'NOT_STARTED';
  const bound = v.purpose === 'INVALIDATION' || ['CREATED', 'UPDATE_STARTED'].includes(v.write_phase);
  if (v.known_run !== bound || v.published !== (v.job_state === 'SUCCEEDED') || v.published !== (v.operation_state === 'SUCCEEDED')) return false;
  if (v.published) return v.write_phase === 'UPDATE_STARTED' && v.known_run && v.error_code == null;
  if (v.operation_state === 'PENDING') return !started && ['QUEUED', 'RUNNING'].includes(v.job_state);
  if (v.operation_state === 'FAILED') return !started && ['FAILED', 'CANCELLED'].includes(v.job_state);
  if (v.operation_state === 'RUNNING') return started && v.job_state === 'RUNNING';
  return v.operation_state === 'UNKNOWN' && started && ['QUEUED', 'RUNNING', 'FAILED'].includes(v.job_state);
}
export function validCheckPage(v: CheckOperationPage, actor: string, proposal: string, after?: string): boolean {
  if (!v || v.viewer_id !== actor || v.proposal_id !== proposal || !uuid(v.organization_id)
    || (v.after ?? undefined) !== after || v.limit !== CHECK_PAGE_SIZE || !Array.isArray(v.items) || v.items.length > CHECK_PAGE_SIZE) return false;
  const ids = new Set<string>();
  for (const item of v.items) {
    if (!item || !validCheckOperation(item, actor, proposal, item.operation_id, v.organization_id) || ids.has(item.operation_id) || item.operation_id === after) return false;
    ids.add(item.operation_id);
  }
  return v.next_cursor == null || uuid(v.next_cursor) && v.items.length === CHECK_PAGE_SIZE
    && v.next_cursor === v.items.at(-1)?.operation_id && v.next_cursor !== after;
}
export function validCheckRecovery(v: CheckRecoveryAccepted, proposal: string, operation: CheckOperation, body: Readonly<CheckRecoveryRequest>): boolean {
  // Another operator may have added windows or advanced the immutable phase.
  // Observation can replace an unstarted retry; a started write cannot rewind.
  return !!v && uuid(body.request_id) && body.expected_request_hash === operation.request_hash
    && v.proposal_id === proposal && v.operation_id === operation.operation_id && v.request_id === body.request_id
    && typeof v.replayed === 'boolean' && integer(v.max_attempts, 2147483647, 1)
    && v.max_attempts >= operation.max_attempts + 8 && (v.max_attempts - operation.max_attempts) % 8 === 0
    && (v.mode === 'OBSERVE_STARTED' || operation.write_phase === 'NOT_STARTED' && v.mode === 'RETRY_UNSTARTED');
}
