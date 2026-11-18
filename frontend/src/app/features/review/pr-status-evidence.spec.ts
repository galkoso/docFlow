import { prStatus, metadata, proposal, other } from '../../../testing/review-fixtures';
import { validPrStatus } from './pr-status-evidence';

describe('Stored PR status evidence', () => {
  it('binds one selected snapshot and preserves microsecond receipt ordering', () => {
    const value = prStatus();
    expect(validPrStatus(value, proposal, metadata())).toBe(true);
    value.read_at = '2026-11-18T00:00:00.123455+00:00';
    expect(validPrStatus(value, proposal, metadata())).toBe(false);
    value.read_at = '2026-11-18T00:00:00.123456+00:00';
    expect(validPrStatus(value, proposal, metadata())).toBe(true);
  });
  it.each(['proposal', 'snapshot', 'fingerprint', 'head', 'target', 'uncertain', 'binding', 'observation'])('rejects mixed %s evidence', fault => {
    const value = prStatus();
    if (fault === 'proposal') value.proposal_id = other;
    if (fault === 'snapshot') value.snapshot_id = other;
    if (fault === 'fingerprint') value.fingerprint = 'c'.repeat(64);
    if (fault === 'head') value.head_sha = 'c'.repeat(40);
    if (fault === 'target') value.target_sha = 'c'.repeat(40);
    if (fault === 'uncertain') value.uncertain_write = true;
    if (fault === 'binding') value.binding_current = false;
    if (fault === 'observation') value.pr!.snapshot_id = other;
    expect(validPrStatus(value, proposal, metadata())).toBe(false);
  });
  it.each(['http://github.com/a/b/pull/9', 'https://github.com.evil.test/a/b/pull/9', 'https://secret@github.com/a/b/pull/9',
    'https://github.com/a/b/pull/9?token=secret', 'https://github.com/a/b/pull/9#x', 'https://github.com/a/b/pull/10',
    ' https://github.com/a/b/pull/9', 'https://github.com/a/../b/pull/9', 'javascript:alert(1)'])('rejects unsafe or mismatched provider link %s', url => {
    const value = prStatus(); value.pr!.url = url;
    expect(validPrStatus(value, proposal, metadata())).toBe(false);
  });
  it('preserves decimal provider identity beyond JavaScript safe integers and rejects i64 overflow', () => {
    const value = prStatus(); value.pr!.number = '9007199254740993'; value.pr!.url = `https://github.com/a/b/pull/${value.pr!.number}`;
    expect(validPrStatus(value, proposal, metadata())).toBe(true);
    value.pr!.number = '9223372036854775808'; value.pr!.url = `https://github.com/a/b/pull/${value.pr!.number}`;
    expect(validPrStatus(value, proposal, metadata())).toBe(false);
  });
  it('permits an explicitly historical observation without calling the selected snapshot synchronized', () => {
    const value = prStatus(); value.sync!.phase = 'SUPERSEDED'; value.current_for_sync = false; value.pr!.snapshot_id = other;
    expect(validPrStatus(value, proposal, metadata())).toBe(true);
  });
});
