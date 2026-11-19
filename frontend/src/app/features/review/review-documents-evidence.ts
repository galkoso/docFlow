import type { ReviewDocuments, ReviewSnapshot } from '../../core/api/review-api';
import { digest, integer, sha, uuid } from '../comparison/comparison-evidence';

export function validReviewDocuments(v: ReviewDocuments, actor: string, proposal: string, snapshot: ReviewSnapshot, offset: number): boolean {
  if (!v || v.actor_id !== actor || !uuid(v.actor_id) || v.proposal_id !== proposal || snapshot.proposal_id !== proposal
    || v.snapshot_id !== snapshot.snapshot_id || !uuid(v.snapshot_id) || v.fingerprint !== snapshot.fingerprint || !digest(v.fingerprint)
    || v.head_sha !== snapshot.metadata.summary.head_sha || !sha(v.head_sha) || typeof v.available !== 'boolean'
    || v.offset !== offset || !integer(v.offset, 1000) || !integer(v.total, 1000) || !Array.isArray(v.documents)
    || v.documents.length !== Math.min(20, Math.max(0, v.total - offset))
    || !v.available && (v.total !== 0 || v.documents.length !== 0 || v.next_offset != null)) return false;
  if (v.next_offset !== (offset + v.documents.length < v.total ? offset + v.documents.length : null)) return false;
  if (new Set(v.documents.map(d => d?.id)).size !== v.documents.length
    || new Set(v.documents.map(d => d?.source?.document_id)).size !== v.documents.length
    || new Set(v.documents.map(d => d?.source?.path)).size !== v.documents.length) return false;
  return v.documents.every((d, i) => {
    const s = d?.source;
    return !!d && digest(d.id) && !!s && typeof s.path === 'string' && s.path.length <= 240
      && /^docs\/[^\u0000-\u001f\\:]+\.md$/.test(s.path) && !s.path.split('/').some(p => !p || ['.', '..', '.git'].includes(p.toLowerCase()))
      && typeof s.blob_oid === 'string' && [40, 64].includes(s.blob_oid.length) && sha(s.blob_oid) && digest(s.sha256)
      && integer(s.bytes, 2 * 1024 * 1024) && s.executable === false && s.catalog_position === offset + i
      && typeof s.document_id === 'string' && /^[A-Z0-9-]{1,120}$/.test(s.document_id);
  });
}
