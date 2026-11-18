import { HttpHeaders, HttpResponse } from '@angular/common/http';
import type { ReviewMediaContext, ReviewMediaEvidence, ReviewMediaScope } from './review-media-evidence';
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
