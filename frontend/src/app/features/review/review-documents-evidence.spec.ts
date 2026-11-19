import { validReviewDocuments } from './review-documents-evidence';
import { actor, other, proposal, metadata, reviewDocuments } from '../../../testing/review-fixtures';

describe('Sealed review document pages', () => {
  const valid = (v: ReturnType<typeof reviewDocuments>, offset = 0) => validReviewDocuments(v, actor, proposal, metadata(), offset);
  it('accepts bounded catalog pages and distinguishes legacy unavailability from an empty book', () => {
    expect(valid(reviewDocuments())).toBe(true); expect(valid(reviewDocuments(actor, 20, 21), 20)).toBe(true);
    const empty = reviewDocuments(actor, 0, 0); expect(valid(empty)).toBe(true);
    empty.available = false; expect(valid(empty)).toBe(true);
    expect(valid({ ...reviewDocuments(), available: false })).toBe(false);
  });
  it('requires actor, proposal, snapshot, fingerprint, head and requested offset to agree', () => {
    for (const patch of [{ actor_id: other }, { proposal_id: other }, { snapshot_id: other }, { fingerprint: '0'.repeat(64) },
      { head_sha: '0'.repeat(40) }, { offset: 20 }, { total: 1001 }]) expect(valid({ ...reviewDocuments(), ...patch })).toBe(false);
  });
  it('rejects repeated identities, incomplete pages and cursors that cannot make progress', () => {
    const repeated = reviewDocuments(); repeated.documents[1] = repeated.documents[0]; expect(valid(repeated)).toBe(false);
    const incomplete = reviewDocuments(actor, 0, 21); incomplete.documents.pop(); expect(valid(incomplete)).toBe(false);
    const cursor = reviewDocuments(actor, 0, 21); cursor.next_offset = 0; expect(valid(cursor)).toBe(false);
  });
  it('requires exact text-document descriptors and catalog positions', () => {
    for (const patch of [{ path: 'docs/../secret.md' }, { path: '.specspace/catalog.json' }, { executable: true },
      { sha256: null }, { blob_oid: 'a'.repeat(42) }, { document_id: '../DOC' }, { catalog_position: 1 }, { bytes: 2097153 }]) {
      const v = reviewDocuments(); Object.assign(v.documents[0].source, patch); expect(valid(v)).toBe(false);
    }
  });
});
