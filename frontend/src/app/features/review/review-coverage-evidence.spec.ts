import { validReviewCoverage } from './review-coverage-evidence';
import { metadata, product, proposal, other, reviewUnits } from '../../../testing/review-fixtures';
import { reviewCoverage } from '../../../testing/review-coverage-fixtures';

describe('Server acknowledged-range coverage evidence', () => {
  const page = () => reviewUnits(product);
  const valid = (v = reviewCoverage(), p = page()) => validReviewCoverage(v, product, proposal, metadata(), p, 'PRODUCT');
  it('accepts matching pages and preserves exact historical or unavailable legacy facts', () => {
    expect(valid()).toBe(true); expect(valid({ ...reviewCoverage(), current_for_metadata: false })).toBe(true);
    const legacy = { ...page(), available: false, total: 0, next_offset: null, units: [] };
    expect(valid(reviewCoverage(legacy), legacy)).toBe(true);
    expect(valid({ ...reviewCoverage(legacy), available: true }, legacy)).toBe(false);
  });
  it('rejects known author or assignment mismatches even when the returned counts match', () => {
    const s = metadata(); s.metadata.context.author_id = product;
    expect(validReviewCoverage(reviewCoverage(), product, proposal, s, page(), 'PRODUCT')).toBe(false);
    s.metadata.context.author_id = other; s.metadata.assignments = [];
    expect(validReviewCoverage(reviewCoverage(), product, proposal, s, page(), 'PRODUCT')).toBe(false);
  });
  it.each([{ actor_id: other }, { proposal_id: other }, { snapshot_id: other }, { fingerprint: '0'.repeat(64) }, { head_sha: '0'.repeat(40) },
    { domain: 'DESIGN' }, { offset: 20 }, { next_offset: 2 }, { total_units: 3 }, { available: false }, { current_for_metadata: null },
    { viewed_units: -1 }, { unviewed_units: 1 }, { unsupported_units: 3 }])('rejects mismatched or impossible summary: %j', patch => {
    expect(valid({ ...reviewCoverage(), ...patch } as never)).toBe(false);
  });
  it('requires ordered manifest identities and exact side/whole-item states', () => {
    const value = reviewCoverage(); value.units.reverse(); expect(valid(value)).toBe(false);
    const duplicated = reviewCoverage(); duplicated.units[1] = duplicated.units[0]; expect(valid(duplicated)).toBe(false);
    for (const state of ['absent', 'unsupported', 'invented'] as const) {
      const v = reviewCoverage(); v.units[0].after = state as never; expect(valid(v)).toBe(false);
    }
    const fakeComplete = reviewCoverage(); fakeComplete.units[0].viewed = true; expect(valid(fakeComplete)).toBe(false);
    const fakeTotals = reviewCoverage(); fakeTotals.units[0].before = 'viewed'; fakeTotals.units[0].after = 'viewed'; fakeTotals.units[0].viewed = true;
    expect(valid(fakeTotals)).toBe(false); fakeTotals.viewed_units = 1; fakeTotals.unviewed_units = 1; expect(valid(fakeTotals)).toBe(true);
  });
  it('validates global counts against the displayed subset without limiting coverage to that page', () => {
    const p = reviewUnits(product, 20, 22), value = reviewCoverage(p); value.viewed_units = 15; value.unviewed_units = 7;
    expect(valid(value, p)).toBe(true); value.viewed_units = 21; value.unviewed_units = 1; expect(valid(value, p)).toBe(false);
  });
  it('keeps absent and unsupported sides distinct and never counts unsupported work as fully viewed', () => {
    const p = page(); p.units[0].before = null; const added = reviewCoverage(p); added.units[0].after = 'viewed'; added.units[0].viewed = true;
    added.viewed_units = 1; added.unviewed_units = 1; expect(valid(added, p)).toBe(true);
    p.units[1].after = { source: { path: `assets/${'3'.repeat(64)}.png`, blob_oid: 'a'.repeat(40), bytes: 10, executable: false }, range: null };
    p.units[1].before = null; p.units[1].requirement = { kind: 'file', reason: 'non_document' };
    const unsupported = reviewCoverage(p); expect(valid(unsupported, p)).toBe(true);
    unsupported.unsupported_units = 0; expect(valid(unsupported, p)).toBe(false);
  });
});
