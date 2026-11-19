import { validReviewUnits } from './review-units-evidence';
import { actor, proposal, other, metadata, reviewUnits } from '../../../testing/review-fixtures';

describe('Mandatory review requirement evidence', () => {
  const valid = (value = reviewUnits(), offset = 0) => validReviewUnits(value, actor, proposal, metadata(), offset);
  it('accepts ordered bounded pages, absent section sides, whole-file ranges and legacy unavailability', () => {
    expect(valid()).toBe(true); expect(valid(reviewUnits(actor, 20, 22), 20)).toBe(true);
    expect(valid(reviewUnits(actor, 40000), 40000)).toBe(true);
    const added = reviewUnits(); added.units[0].before = null; expect(valid(added)).toBe(true);
    const deleted = reviewUnits(); deleted.units[0].after = null; expect(valid(deleted)).toBe(true);
    const whole = reviewUnits(); whole.units[0].requirement = { kind: 'file', reason: 'non_section' }; expect(valid(whole)).toBe(true);
    expect(valid({ ...reviewUnits(), available: false, total: 0, units: [], next_offset: null })).toBe(true);
  });
  it('accepts binary file requirements without inventing text ranges', () => {
    const value = reviewUnits(); value.units[0] = { id: value.units[0].id, file_id: value.units[0].file_id,
      requirement: { kind: 'file', reason: 'non_document' }, before: null,
      after: { source: { path: `assets/${'3'.repeat(64)}.png`, bytes: 100, blob_oid: 'a'.repeat(40), executable: false }, range: null } };
    expect(valid(value)).toBe(true);
  });
  it.each([{ actor_id: other }, { proposal_id: other }, { snapshot_id: other }, { fingerprint: 'a'.repeat(64) }, { head_sha: '0'.repeat(40) },
    { offset: 1 }, { total: 0 }, { total: 40001 }, { next_offset: 2 }, { available: false }])('rejects mismatched page context: %j', patch => {
    expect(valid({ ...reviewUnits(), ...patch })).toBe(false);
  });
  it.each(['duplicate', 'identity', 'path', 'range', 'sha', 'line', 'document', 'section', 'source', 'empty', 'text-range', 'reason'])
    ('rejects malformed or incomplete %s evidence', fault => {
      const value = reviewUnits(), unit = value.units[0];
      if (fault === 'duplicate') value.units[1] = unit;
      if (fault === 'identity') unit.file_id = 'bad';
      if (fault === 'path') unit.after!.source.path = 'docs/../escape.md';
      if (fault === 'range') unit.after!.range!.end_byte = 21;
      if (fault === 'sha') unit.after!.range!.sha256 = 'bad';
      if (fault === 'line') unit.after!.range!.start_line = 100;
      if (fault === 'document') unit.after!.source.document_id = 'DOC-OTHER';
      if (fault === 'section') unit.requirement = { kind: 'section', document_id: 'DOC-A', section_id: '<script>' };
      if (fault === 'source') unit.after!.source.sha256 = null;
      if (fault === 'empty') { unit.before = null; unit.after = null; }
      if (fault === 'text-range') unit.after!.range = null;
      if (fault === 'reason') unit.requirement = { kind: 'file', reason: 'unknown' as never };
      expect(valid(value)).toBe(false);
    });
});
