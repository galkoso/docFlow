import type { ReviewUnits, ReviewSnapshot } from '../../core/api/review-api';
import { digest, integer, sha, uuid, validChange } from '../comparison/comparison-evidence';

export type ReviewUnit = ReviewUnits['units'][number];
type UnitSource = NonNullable<ReviewUnit['before']>;
const reasons = ['non_document', 'non_section', 'layout', 'unstructured', 'section_limit', 'manifest_limit'];
const stableId = (v: unknown): v is string => typeof v === 'string' && /^[A-Z0-9-]{1,120}$/.test(v);
function sourceValid(value: UnitSource, file: string): boolean {
  if (!value?.source) return false;
  const s = value.source, path = s.path;
  if (typeof path !== 'string') return false;
  const kind = path === '.specspace/catalog.json' ? 'catalog' : path.startsWith('docs/') && path.endsWith('.md') ? 'document'
    : path.startsWith('assets/') ? 'asset' : 'other';
  if (!validChange({ id: file, kind, status: 'added', before: null, after: s,
    content_changed: true, mode_changed: false, catalog_position_changed: false, matched_by: null })) return false;
  const r = value.range;
  if (r == null) return !['document', 'catalog'].includes(kind);
  return digest(r.sha256) && integer(r.start_byte, s.bytes) && integer(r.end_byte, s.bytes, r.start_byte)
    && integer(r.start_line, r.start_byte + 1, 1) && integer(r.end_line, r.end_byte + 1, r.start_line);
}
export function validReviewUnits(v: ReviewUnits, actor: string, proposal: string, snapshot: ReviewSnapshot, offset: number): boolean {
  if (!v || v.actor_id !== actor || !uuid(v.actor_id) || v.proposal_id !== proposal || snapshot.proposal_id !== proposal
    || v.snapshot_id !== snapshot.snapshot_id || !uuid(v.snapshot_id) || v.fingerprint !== snapshot.fingerprint || !digest(v.fingerprint)
    || v.head_sha !== snapshot.metadata.summary.head_sha || !sha(v.head_sha) || typeof v.available !== 'boolean'
    || v.offset !== offset || !integer(v.offset, 40000) || !integer(v.total, 40000) || !Array.isArray(v.units)
    || v.units.length !== Math.min(20, Math.max(0, v.total - offset))
    || v.available && v.total < Math.max(1, snapshot.file_count)
    || !v.available && (v.total !== 0 || v.units.length !== 0 || v.next_offset != null)
    || v.next_offset !== (offset + v.units.length < v.total ? offset + v.units.length : null)
    || new Set(v.units.map(u => u?.id)).size !== v.units.length) return false;
  return v.units.every(u => {
    if (!u || !digest(u.id) || !digest(u.file_id) || !u.requirement || !u.before && !u.after) return false;
    const sources = [u.before, u.after].filter((s): s is UnitSource => s != null);
    if (!sources.every(s => sourceValid(s, u.file_id))) return false;
    const r = u.requirement;
    if (r.kind === 'file') return reasons.includes(r.reason);
    return r.kind === 'section' && stableId(r.document_id) && stableId(r.section_id)
      && sources.every(s => s.source.document_id === r.document_id && s.source.path.startsWith('docs/')
        && s.source.path.endsWith('.md') && s.range != null);
  });
}
