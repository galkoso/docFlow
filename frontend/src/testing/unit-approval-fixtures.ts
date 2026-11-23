import type { UnitApprovalPage, UnitApprovalPageRequest, UnitApprovalRequest, UnitApprovalRecorded, ReviewUnits } from '../app/core/api/review-api';
import { metadata, product, proposal, snapshot, other, reviewUnits } from './review-fixtures';

export function unitApprovalPage(page: ReviewUnits = reviewUnits(product), domain: UnitApprovalPageRequest['domain'] = 'PRODUCT'): UnitApprovalPage {
  const s = metadata().metadata.summary;
  return { actor_id: page.actor_id, proposal_id: page.proposal_id, snapshot_id: page.snapshot_id, fingerprint: page.fingerprint,
    head_sha: page.head_sha, target_sha: s.target_sha, policy_version: s.policy_version, configuration_version: s.configuration_version,
    domain, current_for_metadata: true, available: page.available, total_units: page.total, approved_units: 0, unapproved_units: page.total,
    offset: page.offset, next_offset: page.next_offset, units: page.units.map(u => ({ unit_id: u.id, approval_id: null, approved_at: null })) };
}
export const unitApprovalRequest = (): UnitApprovalRequest => ({ request_id: other, unit_id: reviewUnits(product).units[0].id,
  expected_fingerprint: metadata().fingerprint, domain: 'PRODUCT' });
export function unitApprovalReceipt(request: Readonly<UnitApprovalRequest> = unitApprovalRequest()): UnitApprovalRecorded {
  const s = metadata().metadata.summary;
  return { id: '00000000-0000-4000-8000-000000000066', actor_id: product, proposal_id: proposal, snapshot_id: snapshot,
    fingerprint: request.expected_fingerprint, unit_id: request.unit_id, request_id: request.request_id, domain: request.domain,
    head_sha: s.head_sha, target_sha: s.target_sha, policy_version: s.policy_version, configuration_version: s.configuration_version,
    recorded_at: '2026-11-23T00:00:00.123456Z', replayed: false };
}
