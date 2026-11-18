import { HttpResponse } from '@angular/common/http';
import type { components } from '../../core/api/generated/schema';
import { digest, integer, sha, uuid } from './comparison-evidence';

/** Supported current engine version for before-image validation, independent
 * of the historical after-receipt policy. Bump with the server media engine. */
export const BEFORE_MEDIA_VALIDATION_VERSION = 1;
export interface BeforeMediaDocument { file: string; path: string; blobOid: string; sha256: string; }
/** Frozen shared snapshot authority; a before-document selects before-media. */
export interface ReviewMediaContext {
  actor: string; proposal: string; snapshot: string; content: string; head: string; base: string; validationVersion: number; beforeDocument?: BeforeMediaDocument;
}
export interface ReviewMediaScope extends ReviewMediaContext { path: string; }
export type ReviewMediaEvidence = components['schemas']['ReviewMediaEvidence'];
export type ReviewBeforeMediaEvidence = components['schemas']['ReviewBeforeMediaEvidence'];
export type ReviewInlineMediaEvidence = ReviewMediaEvidence | ReviewBeforeMediaEvidence;
export const reviewMediaPath = (path: unknown): path is string => typeof path === 'string' && /^assets\/[a-f0-9]{64}\.(png|jpg|jpeg|webp)$/.test(path);
function beforeDocument(value: BeforeMediaDocument | undefined): value is BeforeMediaDocument {
  if (!value || !digest(value.file) || !sha(value.blobOid) || !digest(value.sha256) || typeof value.path !== 'string'
    || !value.path.startsWith('docs/') || !value.path.endsWith('.md') || value.path !== value.path.normalize('NFC')
    || Array.from(value.path).length > 240 || /[\u0000-\u001f\u007f-\u009f\\:]/.test(value.path)) return false;
  const parts = value.path.split('/');
  return parts.length <= 10 && parts.every(part => !!part && !['.', '..', '.git'].includes(part.toLowerCase()));
}
export function reviewMediaKey(scope: ReviewMediaScope | null): string | null {
  if (!scope || ![scope.actor, scope.proposal, scope.snapshot].every(uuid) || !digest(scope.content) || !sha(scope.head) || !sha(scope.base)
    || !integer(scope.validationVersion, 2147483647, 1) || !reviewMediaPath(scope.path)) return null;
  const before = Object.hasOwn(scope, 'beforeDocument');
  if (!before && scope.beforeDocument !== undefined) return null;
  if (before && (!beforeDocument(scope.beforeDocument) || scope.validationVersion !== BEFORE_MEDIA_VALIDATION_VERSION)) return null;
  const doc = scope.beforeDocument;
  return JSON.stringify([before ? 'before' : 'after', scope.actor, scope.proposal, scope.snapshot, scope.content, scope.head, scope.base,
    scope.validationVersion, scope.path, doc ? [doc.file, doc.path, doc.blobOid, doc.sha256] : null]);
}
export function reviewMediaEvidence(response: HttpResponse<Blob>, scope: ReviewMediaScope): ReviewInlineMediaEvidence | null {
  if (!reviewMediaKey(scope)) return null;
  const doc = scope.beforeDocument;
  const header = response.headers.get(doc ? 'X-Review-Before-Media' : 'X-Review-Media');
  if (!header || header.length > 4096 || !/^[A-Za-z0-9_-]+$/.test(header) || response.headers.get('Content-Type') !== 'image/png'
    || !(response.body instanceof Blob) || response.body.type !== 'image/png' || !integer(response.body.size, 16 * 1024 * 1024, 1)) return null;
  try {
    const value: ReviewInlineMediaEvidence = JSON.parse(atob(header.replaceAll('-', '+').replaceAll('_', '/'))), image = value?.image;
    if (!image || value.asset_path !== scope.path || value.media_validation_version !== scope.validationVersion
      || image.actor_id !== scope.actor || image.proposal_id !== scope.proposal || image.snapshot_id !== scope.snapshot
      || image.content_id !== scope.content || image.head_sha !== scope.head || image.merge_base_sha !== scope.base || image.side !== (doc ? 'before' : 'after')
      || !digest(image.file_id) || !sha(image.source_blob_oid) || image.source_sha256 !== scope.path.slice(7, 71)
      || !integer(image.source_bytes, 5 * 1024 * 1024, 1) || !digest(image.display_sha256) || image.display_bytes !== response.body.size
      || !integer(image.width, 2048, 1) || !integer(image.height, 2048, 1)) return null;
    if (doc && (!('document_path' in value) || value.document_path !== doc.path || value.document_blob_oid !== doc.blobOid
      || value.document_sha256 !== doc.sha256 || image.file_id !== doc.file)) return null;
    return value;
  } catch { return null; }
}
