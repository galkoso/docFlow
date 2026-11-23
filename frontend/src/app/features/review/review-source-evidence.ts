import type { ReviewSourceRequest, ReviewSourceWindow, ReviewVisitRequest, ReviewVisitRecorded } from '../../core/api/review-api';
import { digest, integer, sha, uuid } from '../comparison/comparison-evidence';
import { reviewAreas } from './official-review-evidence';
import { utc } from './snapshot-evidence';
import { validReviewUnit, type ReviewUnit } from './review-units-evidence';

export interface ReviewSourceScope {
  actor: string; proposal: string; snapshot: string; fingerprint: string; head: string; base: string;
  unit: ReviewUnit; domain: ReviewSourceRequest['domain']; side: ReviewSourceRequest['side'];
}
export type SourceDigest = (text: string) => Promise<string>;
type Source = NonNullable<ReviewUnit['before']>;
export function sourceIdentity(s: Source | null | undefined): string {
  const v = s?.source, r = s?.range;
  return JSON.stringify(v ? [[v.path, v.blob_oid, v.bytes, v.executable, v.sha256 ?? null, v.document_id ?? null, v.catalog_position ?? null],
    r ? [r.start_byte, r.end_byte, r.start_line, r.end_line, r.sha256] : null] : null);
}
export function sourceScopeKey(s: ReviewSourceScope): string | null {
  if (!s || ![s.actor, s.proposal, s.snapshot].every(uuid) || !digest(s.fingerprint) || !sha(s.head) || !sha(s.base)
    || !reviewAreas.includes(s.domain) || !['before', 'after'].includes(s.side) || !s.unit?.[s.side]?.range) return null;
  if (!validReviewUnit(s.unit)) return null;
  return JSON.stringify([s.actor, s.proposal, s.snapshot, s.fingerprint, s.head, s.base, s.unit.id, s.unit.file_id, s.unit.requirement,
    sourceIdentity(s.unit.before), sourceIdentity(s.unit.after), s.domain, s.side]);
}
function wellFormed(v: string): boolean {
  for (let i = 0; i < v.length; i++) {
    const c = v.charCodeAt(i);
    if (c >= 0xd800 && c <= 0xdbff) { const next = v.charCodeAt(++i); if (!(next >= 0xdc00 && next <= 0xdfff)) return false; }
    else if (c >= 0xdc00 && c <= 0xdfff) return false;
  }
  return true;
}
function matchingWindow(v: ReviewSourceWindow, s: ReviewSourceScope, offset: number): boolean {
  if (!sourceScopeKey(s) || !v || v.actor_id !== s.actor || v.proposal_id !== s.proposal || v.snapshot_id !== s.snapshot
    || v.fingerprint !== s.fingerprint || v.domain !== s.domain || v.unit_id !== s.unit.id || v.side !== s.side || !uuid(v.delivery_id)
    || v.head_sha !== s.head || v.merge_base_sha !== s.base || sourceIdentity(v.source) !== sourceIdentity(s.unit[s.side])
    || v.offset !== offset || !integer(offset, 2097152) || typeof v.text !== 'string' || v.text.length > 65536 || !wellFormed(v.text)
    || !digest(v.window_sha256) || !utc(v.expires_at)) return false;
  const range = s.unit[s.side]!.range!;
  if (offset > range.end_byte - range.start_byte || v.start_byte !== range.start_byte + offset
    || !integer(v.end_byte, range.end_byte, v.start_byte) || v.end_byte - v.start_byte > 65536
    || v.end_byte < Math.min(range.end_byte, v.start_byte + 65536) - 3
    || v.next_offset !== (v.end_byte < range.end_byte ? v.end_byte - range.start_byte : null)
    || v.start_byte === range.start_byte && v.end_byte === range.end_byte && v.window_sha256 !== range.sha256
    || v.next_offset != null && v.next_offset <= offset || new TextEncoder().encode(v.text).length !== v.end_byte - v.start_byte) return false;
  return true;
}
export async function validSourceWindow(v: ReviewSourceWindow, s: ReviewSourceScope, offset: number, hash: SourceDigest): Promise<boolean> {
  if (!matchingWindow(v, s, offset)) return false;
  try { return await hash(v.text) === v.window_sha256; } catch { return false; }
}
export function validVisitReceipt(v: ReviewVisitRecorded, s: ReviewSourceScope, w: ReviewSourceWindow, r: Readonly<ReviewVisitRequest>): boolean {
  return matchingWindow(w, s, w.offset) && !!v && uuid(v.id) && uuid(r.request_id) && v.request_id === r.request_id && r.delivery_id === w.delivery_id
    && v.delivery_id === w.delivery_id && v.actor_id === s.actor && v.proposal_id === s.proposal && v.snapshot_id === s.snapshot
    && r.expected_fingerprint === s.fingerprint && v.fingerprint === s.fingerprint && r.domain === s.domain && v.domain === s.domain
    && v.unit_id === s.unit.id && v.side === s.side && v.start_byte === w.start_byte && v.end_byte === w.end_byte
    && utc(v.recorded_at) && Date.parse(v.recorded_at) <= Date.parse(w.expires_at) && typeof v.replayed === 'boolean';
}
export function freezeSource<T>(value: T): T {
  if (value && typeof value === 'object' && !Object.isFrozen(value)) { for (const child of Object.values(value)) freezeSource(child); Object.freeze(value); }
  return value;
}
