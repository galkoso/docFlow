import { HttpResponse } from '@angular/common/http';
import type { components } from '../../core/api/generated/schema';
import { digest, integer, sha, uuid } from './comparison-evidence';

/** Frozen after-image authority from the validated shared snapshot header. */
export interface ReviewMediaContext {
  actor: string; proposal: string; snapshot: string; content: string; head: string; base: string; validationVersion: number;
}
export interface ReviewMediaScope extends ReviewMediaContext { path: string; }
export type ReviewMediaEvidence = components['schemas']['ReviewMediaEvidence'];
export const reviewMediaPath = (path: unknown): path is string => typeof path === 'string' && /^assets\/[a-f0-9]{64}\.(png|jpg|jpeg|webp)$/.test(path);
export function reviewMediaKey(scope: ReviewMediaScope | null): string | null {
  return scope && [scope.actor, scope.proposal, scope.snapshot].every(uuid) && digest(scope.content) && sha(scope.head) && sha(scope.base)
    && integer(scope.validationVersion, 2147483647, 1) && reviewMediaPath(scope.path)
    ? JSON.stringify([scope.actor, scope.proposal, scope.snapshot, scope.content, scope.head, scope.base, scope.validationVersion, scope.path]) : null;
}
export function reviewMediaEvidence(response: HttpResponse<Blob>, scope: ReviewMediaScope): ReviewMediaEvidence | null {
  if (!reviewMediaKey(scope)) return null;
  const header = response.headers.get('X-Review-Media');
  if (!header || header.length > 4096 || !/^[A-Za-z0-9_-]+$/.test(header) || response.headers.get('Content-Type') !== 'image/png'
    || !(response.body instanceof Blob) || response.body.type !== 'image/png' || !integer(response.body.size, 16 * 1024 * 1024, 1)) return null;
  try {
    const value: ReviewMediaEvidence = JSON.parse(atob(header.replaceAll('-', '+').replaceAll('_', '/'))), image = value?.image;
    if (!image || value.asset_path !== scope.path || value.media_validation_version !== scope.validationVersion
      || image.actor_id !== scope.actor || image.proposal_id !== scope.proposal || image.snapshot_id !== scope.snapshot
      || image.content_id !== scope.content || image.head_sha !== scope.head || image.merge_base_sha !== scope.base || image.side !== 'after'
      || !digest(image.file_id) || !sha(image.source_blob_oid) || image.source_sha256 !== scope.path.slice(7, 71)
      || !integer(image.source_bytes, 5 * 1024 * 1024, 1) || !digest(image.display_sha256) || image.display_bytes !== response.body.size
      || !integer(image.width, 2048, 1) || !integer(image.height, 2048, 1)) return null;
    return value;
  } catch { return null; }
}
