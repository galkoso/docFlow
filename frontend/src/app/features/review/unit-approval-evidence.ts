import type { ReviewSnapshot, ReviewUnits, UnitApprovalPage, UnitApprovalRequest, UnitApprovalRecorded } from '../../core/api/review-api';
import { digest, integer, sha, uuid } from '../comparison/comparison-evidence';
import { reviewAreas } from './official-review-evidence';
import { utc } from './snapshot-evidence';
import { validReviewUnits } from './review-units-evidence';

function eligible(actor: string, snapshot: ReviewSnapshot, domain: UnitApprovalRequest['domain']): boolean {
  return reviewAreas.includes(domain) && snapshot.metadata.context.author_id !== actor
    && snapshot.metadata.assignments.some(a => a.reviewer_id === actor && a.domain === domain);
}
function bound(v: UnitApprovalPage | UnitApprovalRecorded, actor: string, proposal: string, snapshot: ReviewSnapshot): boolean {
  const s = snapshot.metadata.summary;
  return !!v && v.actor_id === actor && uuid(actor) && v.proposal_id === proposal && uuid(proposal) && snapshot.proposal_id === proposal
    && v.snapshot_id === snapshot.snapshot_id && uuid(v.snapshot_id) && v.fingerprint === snapshot.fingerprint && digest(v.fingerprint)
    && v.head_sha === s.head_sha && sha(v.head_sha) && v.target_sha === s.target_sha && sha(v.target_sha)
    && v.policy_version === s.policy_version && integer(v.policy_version, Number.MAX_SAFE_INTEGER, 1)
    && v.configuration_version === s.configuration_version && integer(v.configuration_version, Number.MAX_SAFE_INTEGER, 1);
}
export function validUnitApprovalPage(v: UnitApprovalPage, actor: string, proposal: string, snapshot: ReviewSnapshot,
  page: ReviewUnits, domain: UnitApprovalRequest['domain']): boolean {
  if (!validReviewUnits(page, actor, proposal, snapshot, page.offset) || !eligible(actor, snapshot, domain) || !bound(v, actor, proposal, snapshot)
    || v.domain !== domain || typeof v.current_for_metadata !== 'boolean' || v.available !== page.available || v.total_units !== page.total
    || v.offset !== page.offset || v.next_offset !== page.next_offset || !Array.isArray(v.units) || v.units.length !== page.units.length
    || !integer(v.approved_units, v.total_units) || !integer(v.unapproved_units, v.total_units)
    || v.approved_units + v.unapproved_units !== v.total_units || v.available && v.total_units < 1) return false;
  const ids = new Set<string>(); let approved = 0;
  for (let i = 0; i < v.units.length; i++) {
    const row = v.units[i]; if (!row || row.unit_id !== page.units[i].id) return false;
    if (row.approval_id == null) { if (row.approved_at != null) return false; continue; }
    if ([page.units[i].before, page.units[i].after].some(side => side != null && side.range == null)) return false;
    if (!uuid(row.approval_id) || ids.has(row.approval_id) || !utc(row.approved_at)) return false;
    ids.add(row.approval_id); ++approved;
  }
  const outside = page.total - page.units.length;
  return v.approved_units >= approved && v.approved_units - approved <= outside
    && v.unapproved_units >= v.units.length - approved && v.unapproved_units - (v.units.length - approved) <= outside;
}
export function validUnitApprovalReceipt(v: UnitApprovalRecorded, actor: string, proposal: string, snapshot: ReviewSnapshot,
  request: Readonly<UnitApprovalRequest>): boolean {
  return !!request && uuid(request.request_id) && digest(request.unit_id) && request.expected_fingerprint === snapshot.fingerprint
    && eligible(actor, snapshot, request.domain) && bound(v, actor, proposal, snapshot) && uuid(v.id)
    && v.request_id === request.request_id && v.unit_id === request.unit_id && v.domain === request.domain
    && utc(v.recorded_at) && typeof v.replayed === 'boolean';
}
