import { HttpHeaders, HttpResponse } from '@angular/common/http';
import { reviewMediaEvidence, reviewMediaKey } from './review-media-evidence';
import type { ReviewMediaEvidence } from './review-media-evidence';
import { mediaScope, mediaResponse, beforeMediaScope, beforeMediaResponse } from './review-media.testing';

describe('Shared referenced-image evidence', () => {
  it('binds exact frozen context, path, source and bounded display metadata', () => {
    expect(reviewMediaEvidence(mediaResponse(), { ...mediaScope(), validationVersion: 0 })).toBeNull();
    expect(reviewMediaKey(mediaScope())).not.toBeNull(); expect(reviewMediaEvidence(mediaResponse(), mediaScope())).not.toBeNull();
    for (const path of ['https://example.com/pixel', '../assets/image.png', 'assets/bad.svg', mediaScope().path + '?ref=main']) {
      expect(reviewMediaKey({ ...mediaScope(), path })).toBeNull();
    }
  });
  it.each([
    { asset_path: 'assets/' + 'a'.repeat(64) + '.jpg' }, { media_validation_version: 2 },
    { image: { actor_id: 'other' } }, { image: { proposal_id: 'other' } }, { image: { snapshot_id: 'other' } },
    { image: { content_id: 'f'.repeat(64) } }, { image: { head_sha: 'f'.repeat(40) } }, { image: { merge_base_sha: 'f'.repeat(40) } },
    { image: { side: 'before' } }, { image: { file_id: '../other' } }, { image: { source_blob_oid: 'main' } },
    { image: { source_sha256: 'f'.repeat(64) } }, { image: { source_bytes: 0 } }, { image: { source_bytes: 5 * 1024 * 1024 + 1 } },
    { image: { display_sha256: 'bad' } }, { image: { display_bytes: 4 } }, { image: { width: 0 } }, { image: { height: 2049 } },
  ])('rejects mismatched or oversized proof before decoding: %j', patch => {
    const response = mediaResponse(), value = JSON.parse(atob(response.headers.get('X-Review-Media')!));
    const evidence = { ...value, ...patch, image: { ...value.image, ...patch.image } } as ReviewMediaEvidence;
    expect(reviewMediaEvidence(mediaResponse(evidence), mediaScope())).toBeNull();
  });
  it('rejects malformed headers, non-PNG responses and empty bodies', () => {
    const response = mediaResponse();
    for (const value of ['', '%bad', 'a'.repeat(4097), btoa('null')]) {
      expect(reviewMediaEvidence(response.clone({ headers: new HttpHeaders({ 'Content-Type': 'image/png', 'X-Review-Media': value }) }), mediaScope())).toBeNull();
    }
    expect(reviewMediaEvidence(new HttpResponse({ body: new Blob(), headers: response.headers }), mediaScope())).toBeNull();
    expect(reviewMediaEvidence(response.clone({ headers: response.headers.set('Content-Type', 'image/jpeg') }), mediaScope())).toBeNull();
  });
  it('keeps before and after proof namespaces separate even for identical asset bytes', () => {
    expect(reviewMediaEvidence(beforeMediaResponse(), beforeMediaScope())).not.toBeNull();
    expect(reviewMediaEvidence(mediaResponse(), beforeMediaScope())).toBeNull();
    expect(reviewMediaEvidence(beforeMediaResponse(), mediaScope())).toBeNull();
    expect(reviewMediaKey(beforeMediaScope())).not.toBe(reviewMediaKey(mediaScope()));
  });
  it.each([
    { document_path: 'docs/other.md' }, { document_blob_oid: 'f'.repeat(40) }, { document_sha256: 'f'.repeat(64) },
    { image: { file_id: 'f'.repeat(64) } }, { image: { side: 'after' } }, { image: { merge_base_sha: 'f'.repeat(40) } },
    { media_validation_version: 2 },
  ])('rejects mismatched before-document proof: %j', patch => {
    const response = beforeMediaResponse(), value = JSON.parse(atob(response.headers.get('X-Review-Before-Media')!));
    expect(reviewMediaEvidence(beforeMediaResponse({ ...value, ...patch, image: { ...value.image, ...patch.image } }), beforeMediaScope())).toBeNull();
  });
  it('rejects malformed before authority without silently switching to after authority', () => {
    const base = beforeMediaScope(), doc = base.beforeDocument!;
    expect(reviewMediaKey(Object.assign(Object.create({ beforeDocument: doc }), mediaScope()))).toBeNull();
    for (const patch of [{ beforeDocument: undefined }, { beforeDocument: { ...doc, file: 'bad' } },
      { beforeDocument: { ...doc, path: 'docs/../secret.md' } }, { beforeDocument: { ...doc, path: 'assets/other.md' } },
      { beforeDocument: { ...doc, blobOid: 'main' } }, { beforeDocument: { ...doc, sha256: 'bad' } }, { validationVersion: 2 }]) {
      expect(reviewMediaKey({ ...base, ...patch })).toBeNull();
    }
  });

});
