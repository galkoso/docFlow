import { HttpHeaders, HttpResponse } from '@angular/common/http';
import type { ReviewMediaContext, ReviewMediaEvidence, ReviewMediaScope, ReviewBeforeMediaEvidence } from './review-media-evidence';
import { rawScope } from './raw.testing';
import { imageEvidence, imageScope, displayHash } from './image.testing';

export const mediaId = '5d00496c2f055e06378f0e3a2b99460193ee36852f068995962709663615c655';
export { displayHash };
export function mediaContext(): ReviewMediaContext {
  const file = rawScope(); return { actor: file.actor, proposal: file.proposal, snapshot: file.snapshot, content: file.content,
    head: file.head, base: file.base, validationVersion: 1 };
}
export function mediaScope(): ReviewMediaScope { return { ...mediaContext(), path: imageScope().file.file.after!.path }; }
export function mediaEvidence(): ReviewMediaEvidence {
  return { asset_path: mediaScope().path, media_validation_version: 1, image: { ...imageEvidence(), file_id: mediaId } };
}
export function mediaResponse(evidence = mediaEvidence()): HttpResponse<Blob> {
  const header = btoa(JSON.stringify(evidence)).replaceAll('+', '-').replaceAll('/', '_').replaceAll('=', '');
  return new HttpResponse({ body: new Blob(['PNG'], { type: 'image/png' }), headers: new HttpHeaders({
    'Content-Type': 'image/png', 'X-Review-Media': header,
  }) });
}

export function beforeMediaContext(): ReviewMediaContext {
  const file = rawScope(), source = file.file.before!;
  return { ...mediaContext(), beforeDocument: { file: file.file.id, path: source.path, blobOid: source.blob_oid, sha256: source.sha256! } };
}
export function beforeMediaScope(): ReviewMediaScope { return { ...beforeMediaContext(), path: mediaScope().path }; }
export function beforeMediaEvidence(): ReviewBeforeMediaEvidence {
  const scope = beforeMediaScope(), doc = scope.beforeDocument!;
  return { asset_path: scope.path, media_validation_version: scope.validationVersion,
    document_path: doc.path, document_blob_oid: doc.blobOid, document_sha256: doc.sha256,
    image: { ...imageEvidence(), side: 'before', file_id: doc.file } };
}
export function beforeMediaResponse(evidence = beforeMediaEvidence()): HttpResponse<Blob> {
  const header = btoa(JSON.stringify(evidence)).replaceAll('+', '-').replaceAll('/', '_').replaceAll('=', '');
  return new HttpResponse({ body: new Blob(['PNG'], { type: 'image/png' }), headers: new HttpHeaders({
    'Content-Type': 'image/png', 'X-Review-Before-Media': header,
  }) });
}
