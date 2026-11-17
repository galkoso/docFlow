import { HttpHeaders, HttpResponse } from '@angular/common/http';
import type { ImageEvidence } from '../../core/api/comparison-api';
import { rawScope } from './raw.testing';
import type { ImageScope } from './image-state';

export const displayHash = 'd'.repeat(64);
export function imageScope(): ImageScope {
  const file = rawScope();
  file.file.kind = 'asset'; file.file.status = 'added'; file.file.before = null; file.file.matched_by = null;
  file.file.after = { path: `assets/${'a'.repeat(64)}.png`, blob_oid: 'b'.repeat(40), bytes: 100, executable: false,
    sha256: null, document_id: null, catalog_position: null };
  return { file, side: 'after' };
}
export function imageEvidence(scope = imageScope()): ImageEvidence {
  const file = scope.file, source = file.file[scope.side]!;
  return { actor_id: file.actor, proposal_id: file.proposal, snapshot_id: file.snapshot, content_id: file.content, file_id: file.file.id,
    head_sha: file.head, merge_base_sha: file.base, side: scope.side, source_sha256: source.path.slice(7,71),
    source_blob_oid: source.blob_oid, source_bytes: source.bytes, display_sha256: displayHash, display_bytes: 3, width: 1, height: 1 };
}
export function proofHeader(proof = imageEvidence()): string { return btoa(JSON.stringify(proof)).replaceAll('+', '-').replaceAll('/', '_').replaceAll('=', ''); }
export function imageResponse(scope = imageScope()): HttpResponse<Blob> {
  return new HttpResponse({ body: new Blob(['PNG'], { type: 'image/png' }), headers: new HttpHeaders({
    'Content-Type': 'image/png', 'X-Comparison-Image': proofHeader(imageEvidence(scope)),
  }) });
}
