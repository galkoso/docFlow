import type { ReviewCoverage, ReviewCoverageRequest, ReviewUnits, ReviewUnitCoverage } from '../app/core/api/review-api';
import { product, reviewUnits } from './review-fixtures';
export function reviewCoverage(page: ReviewUnits = reviewUnits(product), domain: ReviewCoverageRequest['domain'] = 'PRODUCT'): ReviewCoverage {
  const status = (side: ReviewUnits['units'][number]['before']): ReviewUnitCoverage['before'] => !side ? 'absent' : !side.range ? 'unsupported' : 'unviewed';
  const units = page.units.map(u => ({ unit_id: u.id, before: status(u.before), after: status(u.after), viewed: false }));
  return { actor_id: page.actor_id, proposal_id: page.proposal_id, snapshot_id: page.snapshot_id, fingerprint: page.fingerprint, head_sha: page.head_sha,
    domain, current_for_metadata: true, available: page.available, total_units: page.total, viewed_units: 0, unviewed_units: page.total,
    unsupported_units: units.filter(u => u.before === 'unsupported' || u.after === 'unsupported').length,
    offset: page.offset, next_offset: page.next_offset, units };
}
