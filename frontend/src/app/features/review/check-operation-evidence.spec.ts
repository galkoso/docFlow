import { describe, it, expect } from 'vitest';
import type { CheckOperation, CheckOperationPage, CheckRecoveryAccepted } from '../../core/api/check-operations-api';
import { validCheckOperation, validCheckPage, validCheckRecovery } from './check-operation-evidence';

const id = (n: number) => `00000000-0000-4000-8000-${n.toString(16).padStart(12, '0')}`;
const actor = id(1), organization = id(2), proposal = id(3), operation = id(4);
function value(): CheckOperation {
  return { viewer_id: actor, organization_id: organization, proposal_id: proposal, operation_id: operation, snapshot_id: id(5),
    request_hash: 'a'.repeat(64), purpose: 'EVALUATION', desired_result: 'SUCCESS', job_state: 'FAILED', operation_state: 'UNKNOWN',
    attempts: 8, max_attempts: 8, write_phase: 'CREATE_STARTED', known_run: false, published: false, error_code: 'CHECK_RESULT_UNKNOWN', provider_verified: false };
}
const valid = (v: CheckOperation) => validCheckOperation(v, actor, proposal, operation, organization);
function page(): CheckOperationPage {
  return { viewer_id: actor, organization_id: organization, proposal_id: proposal, limit: 20, items: [value()], after: null, next_cursor: null };
}
describe('check-operation journal evidence', () => {
  it.each([
    ['QUEUED', 'PENDING', 'NOT_STARTED', false, false], ['RUNNING', 'PENDING', 'NOT_STARTED', false, false],
    ['RUNNING', 'RUNNING', 'CREATE_STARTED', false, false], ['QUEUED', 'UNKNOWN', 'CREATE_STARTED', false, false],
    ['RUNNING', 'UNKNOWN', 'CREATED', true, false], ['FAILED', 'FAILED', 'NOT_STARTED', false, false],
    ['CANCELLED', 'FAILED', 'NOT_STARTED', false, false], ['FAILED', 'UNKNOWN', 'UPDATE_STARTED', true, false],
    ['SUCCEEDED', 'SUCCEEDED', 'UPDATE_STARTED', true, true],
  ] as const)('accepts the recorded %s/%s/%s state without claiming live provider verification', (job, state, phase, known, published) => {
    const v = value(); Object.assign(v, { job_state: job, operation_state: state, write_phase: phase, known_run: known, published, error_code: null });
    expect(valid(v)).toBe(true);
  });
  it.each(['viewer_id', 'organization_id', 'proposal_id', 'operation_id', 'snapshot_id', 'request_hash'] as const)('rejects substituted or malformed %s', key => {
    const v = value(); v[key] = key === 'snapshot_id' || key === 'request_hash' ? 'invalid' : id(99); expect(valid(v)).toBe(false);
  });
  it.each([
    { provider_verified: true }, { attempts: -1 }, { attempts: 9 }, { max_attempts: 0 }, { max_attempts: 2147483648 },
    { known_run: true }, { published: true }, { error_code: '<PRIVATE BODY>' }, { desired_result: 'NEUTRAL' },
    { purpose: 'UNKNOWN' }, { purpose: 'INVALIDATION', desired_result: 'SUCCESS' },
    { job_state: 'SUCCEEDED' }, { operation_state: 'SUCCEEDED' }, { write_phase: 'FINISHED' },
    { operation_state: 'PENDING' }, { job_state: 'CANCELLED' },
  ])('rejects inconsistent or unbounded journal evidence %j', patch => {
    expect(valid(Object.assign(value(), patch))).toBe(false);
  });
  it('accepts a bound failure-only invalidation before its UPDATE and rejects a missing identity', () => {
    const v = value(); Object.assign(v, { purpose: 'INVALIDATION', desired_result: 'FAILURE', job_state: 'QUEUED', operation_state: 'PENDING',
      write_phase: 'NOT_STARTED', known_run: true }); expect(valid(v)).toBe(true); v.known_run = false; expect(valid(v)).toBe(false);
  });
  it('requires complete bounded pages with unique identities and an exact cursor', () => {
    const p = page(); expect(validCheckPage(p, actor, proposal)).toBe(true);
    p.items = Array.from({ length: 20 }, (_, n) => ({ ...value(), operation_id: id(n + 20) })); p.next_cursor = id(39);
    expect(validCheckPage(p, actor, proposal)).toBe(true); p.next_cursor = id(99); expect(validCheckPage(p, actor, proposal)).toBe(false);
    p.next_cursor = null; p.items.push(value()); expect(validCheckPage(p, actor, proposal)).toBe(false);
  });
  it.each(['viewer_id', 'organization_id', 'proposal_id', 'limit', 'after', 'next_cursor', 'duplicate', 'foreign_item', 'cursor_item'] as const)
    ('rejects page substitution or incomplete evidence: %s', key => {
      const p = page();
      if (key === 'limit') p.limit = 50;
      else if (key === 'duplicate') p.items.push(value());
      else if (key === 'foreign_item') p.items[0].organization_id = id(99);
      else if (key === 'cursor_item') p.after = operation;
      else p[key] = id(99);
      expect(validCheckPage(p, actor, proposal, key === 'cursor_item' ? operation : undefined)).toBe(false);
    });
  it('correlates recovery responses to the captured mode, immutable request and accepted budget', () => {
    const original = value(), body = Object.freeze({ request_id: id(6), expected_request_hash: original.request_hash });
    const accepted: CheckRecoveryAccepted = { proposal_id: proposal, operation_id: operation, request_id: body.request_id,
      mode: 'OBSERVE_STARTED', max_attempts: 16, replayed: false };
    expect(validCheckRecovery(accepted, proposal, original, body)).toBe(true);
    for (const patch of [{ proposal_id: id(99) }, { operation_id: id(99) }, { request_id: id(99) }, { mode: 'RETRY_UNSTARTED' }, { max_attempts: 17 }]) {
      expect(validCheckRecovery({ ...accepted, ...patch }, proposal, original, body)).toBe(false);
    }
    expect(validCheckRecovery({ ...accepted, max_attempts: 24 }, proposal, original, body)).toBe(true);
    expect(validCheckRecovery(accepted, proposal, original, { ...body, expected_request_hash: 'b'.repeat(64) })).toBe(false);
    original.write_phase = 'NOT_STARTED';
    expect(validCheckRecovery({ ...accepted, max_attempts: 24 }, proposal, original, body)).toBe(true);
 expect(validCheckRecovery({ ...accepted, mode: 'RETRY_UNSTARTED', replayed: true }, proposal, original, body)).toBe(true);
  });
});
