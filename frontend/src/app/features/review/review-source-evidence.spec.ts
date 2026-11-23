import { validSourceWindow, validVisitReceipt, sourceScopeKey } from './review-source-evidence';
import { abcHash, sourceScope, sourceWindow, visitReceipt } from '../../../testing/review-source-fixtures';
import { other } from '../../../testing/review-fixtures';

describe('Delivered review source evidence', () => {
  const hash = async (text: string) => text === 'abc' ? abcHash : '0'.repeat(64);
  it('checks exact window metadata and digest before accepting source', async () => {
    expect(await validSourceWindow(sourceWindow(), sourceScope(), 0, hash)).toBe(true);
    expect(await validSourceWindow({ ...sourceWindow(), text: 'xyz' }, sourceScope(), 0, hash)).toBe(false);
    expect(await validSourceWindow(sourceWindow(), sourceScope(), 0, async () => { throw new Error('unavailable'); })).toBe(false);
  });
  it.each([{ actor_id: other }, { proposal_id: other }, { snapshot_id: other }, { fingerprint: 'a'.repeat(64) },
    { head_sha: '0'.repeat(40) }, { merge_base_sha: '1'.repeat(40) }, { domain: 'DEVELOPMENT' }, { side: 'before' },
    { unit_id: 'a'.repeat(64) }, { delivery_id: 'invalid' }, { offset: 1 }, { start_byte: 0 }, { end_byte: 5 },
    { next_offset: 3 }, { text: 'ab' }, { text: '\ud800bc' }, { expires_at: 'yesterday' }])('rejects unbound delivery: %j', async patch => {
    expect(await validSourceWindow({ ...sourceWindow(), ...patch } as never, sourceScope(), 0, hash)).toBe(false);
  });
  it('requires exact source descriptors and full original ranges independent of object key order', async () => {
    const w = sourceWindow(); w.source.source.path = 'docs/other.md'; expect(await validSourceWindow(w, sourceScope(), 0, hash)).toBe(false);
    const reordered = sourceWindow(); reordered.source = { range: reordered.source.range, source: { ...reordered.source.source } };
    expect(await validSourceWindow(reordered, sourceScope(), 0, hash)).toBe(true);
    reordered.source.range!.sha256 = '0'.repeat(64); expect(await validSourceWindow(reordered, sourceScope(), 0, hash)).toBe(false);
  });
  it('binds a complete window hash to its required span and accepts explicit empty spans', async () => {
    const scope = sourceScope(); scope.unit.after!.range!.sha256 = '0'.repeat(64);
    expect(await validSourceWindow(sourceWindow(scope), scope, 0, hash)).toBe(false);
    const emptyHash = 'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855';
    scope.unit.after!.range = { start_byte: 1, end_byte: 1, start_line: 1, end_line: 1, sha256: emptyHash };
    const empty = { ...sourceWindow(scope), text: '', window_sha256: emptyHash };
    expect(await validSourceWindow(empty, scope, 0, async () => emptyHash)).toBe(true);
  });
  it('requires bounded forward progress and rejects shortened nonterminal source windows', async () => {
    const scope = sourceScope(); scope.unit.after!.range!.end_byte = 100; scope.unit.after!.source.bytes = 100;
    const w = { ...sourceWindow(scope), end_byte: 4, next_offset: 3 };
    expect(await validSourceWindow(w, scope, 0, hash)).toBe(false);
    scope.unit.after!.range!.end_byte = 65541; scope.unit.after!.source.bytes = 65541;
    const window = { ...sourceWindow(scope), text: 'a'.repeat(65536), end_byte: 65537, next_offset: 65536, window_sha256: 'a'.repeat(64) };
    expect(await validSourceWindow(window, scope, 0, async () => 'a'.repeat(64))).toBe(true);
  });
  it('binds visit receipts to the exact actor, domain, window and frozen nonce', () => {
    const scope = sourceScope(), window = sourceWindow(), request = Object.freeze({ request_id: other, delivery_id: window.delivery_id,
      domain: scope.domain, expected_fingerprint: scope.fingerprint });
    const receipt = visitReceipt(request); expect(validVisitReceipt(receipt, scope, window, request)).toBe(true);
    for (const patch of [{ id: 'bad' }, { request_id: receipt.id }, { delivery_id: other }, { actor_id: other }, { proposal_id: other },
      { snapshot_id: other }, { fingerprint: '0'.repeat(64) }, { domain: 'DEVELOPMENT' }, { unit_id: '0'.repeat(64) },
      { side: 'before' }, { start_byte: 0 }, { end_byte: 5 }, { recorded_at: 'bad' }, { recorded_at: new Date(Date.now() + 3600000).toISOString() }, { replayed: null }]) {
      expect(validVisitReceipt({ ...receipt, ...patch } as never, scope, window, request)).toBe(false);
    }
    expect(validVisitReceipt(receipt, scope, { ...window, side: 'before' }, request)).toBe(false);
  });
  it('rejects non-text, invalid identities and absent-side selection before any source request', () => {
    const scope = sourceScope(); scope.unit.after!.range = null; expect(sourceScopeKey(scope)).toBeNull();
    scope.unit.after = null; expect(sourceScopeKey(scope)).toBeNull();
    const malformed = sourceScope(); malformed.unit.id = 'bad'; expect(sourceScopeKey(malformed)).toBeNull();
  });
});
