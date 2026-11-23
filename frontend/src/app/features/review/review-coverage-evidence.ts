import type { ReviewCoverage, ReviewCoverageRequest, ReviewSnapshot, ReviewUnits, ReviewUnitCoverage } from '../../core/api/review-api';
import { integer } from '../comparison/comparison-evidence';
import { reviewAreas } from './official-review-evidence';
import { validReviewUnits, type ReviewUnit } from './review-units-evidence';

function validItem(v: ReviewUnitCoverage, unit: ReviewUnit): boolean {
  if (!v || v.unit_id !== unit.id || typeof v.viewed !== 'boolean') return false;
  for (const side of ['before', 'after'] as const) {
    const source = unit[side], state = v[side];
    if (!source ? state !== 'absent' : !source.range ? state !== 'unsupported' : !['unviewed', 'partial', 'viewed'].includes(state)) return false;
  }
  return v.viewed === ([v.before, v.after].every(s => s === 'absent' || s === 'viewed') && [v.before, v.after].some(s => s !== 'absent'));
}
export function validReviewCoverage(v: ReviewCoverage, actor: string, proposal: string, snapshot: ReviewSnapshot,
  page: ReviewUnits, domain: ReviewCoverageRequest['domain']): boolean {
  if (!validReviewUnits(page, actor, proposal, snapshot, page.offset) || !reviewAreas.includes(domain)
    || snapshot.metadata.context.author_id === actor || !snapshot.metadata.assignments.some(a => a.reviewer_id === actor && a.domain === domain)
    || !v || v.actor_id !== actor || v.proposal_id !== proposal || v.snapshot_id !== snapshot.snapshot_id
    || v.fingerprint !== snapshot.fingerprint || v.head_sha !== snapshot.metadata.summary.head_sha || v.domain !== domain
    || typeof v.current_for_metadata !== 'boolean' || v.available !== page.available || v.total_units !== page.total
    || v.offset !== page.offset || v.next_offset !== page.next_offset || !Array.isArray(v.units) || v.units.length !== page.units.length
    || !integer(v.viewed_units, v.total_units) || !integer(v.unviewed_units, v.total_units)
    || v.viewed_units + v.unviewed_units !== v.total_units || !integer(v.unsupported_units, v.unviewed_units)
    || v.available && v.total_units < 1) return false;
  if (!v.units.every((unit, i) => validItem(unit, page.units[i]))) return false;
  const viewed = v.units.filter(u => u.viewed).length, unread = v.units.length - viewed;
  const unsupported = v.units.filter(u => u.before === 'unsupported' || u.after === 'unsupported').length;
  const outside = page.total - page.units.length;
  return v.viewed_units >= viewed && v.viewed_units - viewed <= outside && v.unviewed_units >= unread && v.unviewed_units - unread <= outside
    && v.unsupported_units >= unsupported && v.unsupported_units - unsupported <= outside;
}
