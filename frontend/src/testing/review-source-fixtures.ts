import type { ReviewSourceWindow, ReviewVisitRecorded, ReviewVisitRequest } from '../app/core/api/review-api';
import type { ReviewSourceScope } from '../app/features/review/review-source-evidence';
import { product, proposal, snapshot, head, base, metadata, reviewUnits } from './review-fixtures';
export const abcHash = 'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad';
export function sourceScope(): ReviewSourceScope {
  const unit = reviewUnits().units[0]; unit.after!.range = { start_byte: 1, end_byte: 4, start_line: 1, end_line: 1, sha256: abcHash };
  return { actor: product, proposal, snapshot, fingerprint: metadata().fingerprint, head, base, unit, domain: 'PRODUCT', side: 'after' };
}
export function sourceWindow(scope = sourceScope()): ReviewSourceWindow {
  const range = scope.unit[scope.side]!.range!;
  return { actor_id: scope.actor, proposal_id: scope.proposal, snapshot_id: scope.snapshot, fingerprint: scope.fingerprint,
    delivery_id: '00000000-0000-4000-8000-000000000088', unit_id: scope.unit.id, domain: scope.domain, side: scope.side,
    head_sha: scope.head, merge_base_sha: scope.base, source: structuredClone(scope.unit[scope.side]!),
    start_byte: range.start_byte, end_byte: range.end_byte, offset: 0, next_offset: null, text: 'abc', window_sha256: abcHash,
    expires_at: new Date(Date.now() + 1800000).toISOString() };
}
export function visitReceipt(request: Readonly<ReviewVisitRequest>, scope = sourceScope(), window = sourceWindow(scope)): ReviewVisitRecorded {
  return { id: '00000000-0000-4000-8000-000000000077', request_id: request.request_id, delivery_id: request.delivery_id,
    actor_id: scope.actor, proposal_id: scope.proposal, snapshot_id: scope.snapshot, fingerprint: scope.fingerprint, domain: scope.domain,
    unit_id: scope.unit.id, side: scope.side, start_byte: window.start_byte, end_byte: window.end_byte,
    recorded_at: new Date().toISOString(), replayed: false };
}
