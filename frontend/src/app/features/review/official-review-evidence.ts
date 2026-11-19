import type { OfficialHistory, OfficialRecorded, OfficialRequest, ReviewSnapshot } from '../../core/api/review-api';
import { uuid } from '../comparison/comparison-evidence';
import { text, utc } from './snapshot-evidence';

export const reviewAreas = ['PRODUCT', 'DEVELOPMENT', 'DESIGN'] as const;
export const reviewDecisions = ['APPROVE_ALL', 'REQUEST_CHANGES', 'COMMENT'] as const;
const order = (v: string) => { const [base, fraction = ''] = v.replace(/(?:Z|\+00:00)$/, '').split('.'); return `${base}.${fraction.padEnd(9, '0')}`; };
export function validOfficialHistory(v: OfficialHistory, actor: string, proposal: string, snapshot: ReviewSnapshot, after?: string): boolean {
  const s = snapshot.metadata.summary;
  if (!v || v.proposal_id !== proposal || v.snapshot_id !== snapshot.snapshot_id || v.fingerprint !== snapshot.fingerprint
    || v.head_sha !== s.head_sha || v.target_sha !== s.target_sha || v.policy_version !== s.policy_version || v.configuration_version !== s.configuration_version
    || !utc(v.read_at) || typeof v.current_for_metadata !== 'boolean' || typeof v.approve_all_available !== 'boolean'
    || v.unviewed_content_warning !== true || !Array.isArray(v.domains) || v.domains.length > 3 || new Set(v.domains).size !== v.domains.length
    || v.domains.some(d => !reviewAreas.includes(d) || !snapshot.metadata.assignments.some(a => a.reviewer_id === actor && a.domain === d))
    || v.domains.length > 0 && (!v.current_for_metadata || snapshot.metadata.context.author_id === actor)
    || v.approve_all_available && (!v.current_for_metadata || !v.domains.length || s.ui_impact === 'UNDECIDED'
      || s.recorded_base_sha !== s.target_sha || s.merge_base_sha !== s.target_sha)
    || !Array.isArray(v.entries) || v.entries.length > 20) return false;
  const ids = new Set<string>();
  for (const item of v.entries) {
    if (!item || !uuid(item.id) || item.id === after || ids.has(item.id) || !uuid(item.reviewer_id) || !text(item.reviewer_name, 200)
      || !reviewAreas.includes(item.domain) || !reviewDecisions.includes(item.decision) || !text(item.explanation, 10000, 1)
      || !utc(item.recorded_at) || order(item.recorded_at) > order(v.read_at) || typeof item.effective !== 'boolean'
      || typeof item.reviewer_eligible !== 'boolean' || item.decision === 'COMMENT' && item.effective
      || item.valid_for_stored_context !== (v.current_for_metadata && item.effective && item.reviewer_eligible)) return false;
    ids.add(item.id);
  }
  return v.next_after == null || v.entries.length === 20 && v.next_after === v.entries.at(-1)?.id;
}
export function validOfficialReceipt(v: OfficialRecorded, actor: string, proposal: string, snapshot: ReviewSnapshot, request: Readonly<OfficialRequest>): boolean {
  const s = snapshot.metadata.summary;
  return !!v && uuid(v.id) && v.request_id === request.request_id && v.proposal_id === proposal && v.snapshot_id === snapshot.snapshot_id
    && v.reviewer_id === actor && v.domain === request.domain && v.decision === request.decision && v.head_sha === s.head_sha
    && v.target_sha === s.target_sha && v.policy_version === s.policy_version && v.configuration_version === s.configuration_version
    && utc(v.recorded_at) && typeof v.replayed === 'boolean';
}
