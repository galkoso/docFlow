import type { DiscussionComments, DiscussionIntent, DiscussionRecorded, DiscussionThread, DiscussionThreads } from '../../core/api/discussion-api';
import { digest, integer, sha, uuid } from '../comparison/comparison-evidence';
import { text, utc } from './snapshot-evidence';

const count = (v: unknown, min = 0) => integer(v, Number.MAX_SAFE_INTEGER, min);
const booleans = (...values: unknown[]) => values.every(v => typeof v === 'boolean');
const actor = (v: { id: string; name: string }) => !!v && uuid(v.id) && text(v.name, 200);
const order = (v: string) => { const [base, fraction = ''] = v.replace(/(?:Z|\+00:00)$/, '').split('.'); return `${base}.${fraction.padEnd(9, '0')}`; };
const before = (a: string, b: string) => utc(a) && utc(b) && order(a) <= order(b);
const id = (v: unknown): v is string => typeof v === 'string' && /^[A-Z0-9-]{1,120}$/.test(v);

export function validDiscussionThread(v: DiscussionThread, proposal: string, readAt: string): boolean {
  if (!v || !uuid(v.id) || v.proposal_id !== proposal || !actor(v.creator) || !count(v.version, 1) || !count(v.comment_count, 1)
    || v.comment_count > v.version || !before(v.created_at, v.last_activity_at) || !before(v.last_activity_at, readAt)
    || !booleans(v.blocking, v.resolved, v.independent_requirement, v.outdated_context, v.repository_binding_changed)
    || v.repository_binding_changed && !v.outdated_context) return false;
  const p = v.permissions, o = v.origin;
  if (!p || !booleans(p.can_comment, p.can_resolve, p.can_reopen, p.can_toggle_blocking)
    || !p.can_comment && (p.can_resolve || p.can_reopen || p.can_toggle_blocking) || p.can_resolve && v.resolved || p.can_reopen && !v.resolved
    || !o || !uuid(o.repository_id) || !/^change\/[\x21-\x7e]{1,93}$/.test(o.branch) || !sha(o.head_sha) || !sha(o.target_sha)
    || !count(o.proposal_version, 1)) return false;
  if (v.resolved ? !v.resolution || !actor(v.resolution.actor) || !before(v.created_at, v.resolution.recorded_at)
    || !before(v.resolution.recorded_at, v.last_activity_at) : v.resolution != null) return false;
  if (o.snapshot_id != null ? !uuid(o.snapshot_id) || !digest(o.fingerprint) || !sha(o.merge_base_sha)
    : o.fingerprint != null || o.merge_base_sha != null) return false;
  const anchor = o.anchor;
  if (!anchor) return false;
  if (anchor.kind === 'proposal') return true;
  if (anchor.kind !== 'file' || !o.snapshot_id || !digest(anchor.file_id) || !['before', 'after'].includes(anchor.side)) return false;
  const s = anchor.source, t = anchor.text;
  if (!s || typeof s.path !== 'string' || s.path.length > 240 || !/^(?:docs\/[^\u0000-\u001f\\:]+\.md|\.specspace\/catalog\.json)$/.test(s.path)
    || s.path.split('/').some(part => !part || ['.', '..', '.git'].includes(part.toLowerCase())) || !sha(s.blob_oid) || !digest(s.sha256)
    || !integer(s.bytes, 2 * 1024 * 1024) || typeof s.executable !== 'boolean'
    || s.document_id != null && !id(s.document_id) || s.catalog_position != null && !integer(s.catalog_position, 999)
    || !t || t.source_sha256 !== s.sha256 || !digest(t.range_sha256) || !integer(t.start_byte, s.bytes)
    || !integer(t.end_byte, s.bytes, t.start_byte) || !count(t.start_line, 1) || !count(t.end_line, t.start_line)
    || !text(t.quote, 1000) || typeof t.quote_truncated !== 'boolean' || !t.selection) return false;
  const selection = t.selection;
  if (selection.kind === 'lines') return t.end_line - t.start_line < 500;
  return ['document', 'section'].includes(selection.kind) && id(selection.document_id) && selection.document_id === s.document_id
    && (selection.kind !== 'section' || id(selection.section_id));
}

export function validDiscussionThreads(v: DiscussionThreads, actorId: string, proposal: string, after?: string): boolean {
  return !!v && v.actor_id === actorId && v.proposal_id === proposal && count(v.proposal_version, 1) && utc(v.read_at)
    && (v.recorded_head_sha == null || sha(v.recorded_head_sha)) && (v.recorded_target_sha == null || sha(v.recorded_target_sha))
    && booleans(v.can_comment, v.can_open_blocking_thread) && (!v.can_open_blocking_thread || v.can_comment)
    && count(v.unresolved_blockers) && Array.isArray(v.threads) && v.threads.length <= 20
    && new Set(v.threads.map(t => t?.id)).size === v.threads.length
    && v.threads.every(t => t?.id !== after && validDiscussionThread(t, proposal, v.read_at))
    && v.unresolved_blockers >= v.threads.filter(t => t.blocking && !t.resolved).length
    && (v.next_after == null || v.threads.length === 20 && v.next_after === v.threads.at(-1)?.id);
}

export function validDiscussionComments(v: DiscussionComments, actorId: string, proposal: string, thread: string, version?: number, after?: string): boolean {
  if (!v || v.actor_id !== actorId || v.proposal_id !== proposal || !utc(v.read_at) || v.thread?.id !== thread
    || !validDiscussionThread(v.thread, proposal, v.read_at) || version != null && v.thread.version !== version
    || !Array.isArray(v.comments) || v.comments.length > 20 || v.comments.length > v.thread.comment_count
    || new Set(v.comments.map(c => c?.id)).size !== v.comments.length) return false;
  for (const c of v.comments) {
    if (!c || !uuid(c.id) || c.id === after || !actor(c.author) || !count(c.version, 1) || c.version > v.thread.version
      || !before(v.thread.created_at, c.created_at) || !before(c.created_at, v.read_at) || !booleans(c.can_edit, c.can_tombstone)
      || c.reply_to_id != null && (!uuid(c.reply_to_id) || c.reply_to_id === c.id)
      || c.edited_at != null && (!before(c.created_at, c.edited_at) || !before(c.edited_at, v.read_at))
      || c.tombstoned_at != null && (!before(c.created_at, c.tombstoned_at) || !before(c.tombstoned_at, v.read_at))) return false;
    if (c.tombstoned_at != null ? c.body != null || c.can_edit || c.can_tombstone : !text(c.body, 10000, 1)) return false;
    if ((c.can_edit || c.can_tombstone) && (c.author.id !== actorId || !v.thread.permissions.can_comment)) return false;
  }
  return v.next_after == null || v.comments.length === 20 && v.next_after === v.comments.at(-1)?.id;
}

export function validDiscussionReceipt(v: DiscussionRecorded, actorId: string, proposal: string, intent: DiscussionIntent): boolean {
  if (!v || !uuid(v.id) || v.actor_id !== actorId || v.proposal_id !== proposal || v.request_id !== intent.body.request_id
    || !uuid(v.thread_id) || !utc(v.recorded_at) || typeof v.replayed !== 'boolean' || !count(v.thread_version, 1)) return false;
  if (intent.kind === 'open') return v.thread_version === 1 && uuid(v.comment_id) && v.comment_version === 1;
  if (v.thread_id !== intent.thread || v.thread_version !== intent.body.expected_thread_version + 1) return false;
  const m = intent.body.mutation;
  if (m.kind === 'reply') return uuid(v.comment_id) && v.comment_version === 1;
  if (m.kind === 'edit' || m.kind === 'tombstone') return v.comment_id === m.comment_id && v.comment_version === m.expected_comment_version + 1;
  return v.comment_id == null && v.comment_version == null;
}
