import { inject, Injectable, OnDestroy, signal } from '@angular/core';
import { HttpErrorResponse } from '@angular/common/http';
import { Subscription, timeout } from 'rxjs';
import { ComparisonApi, ComparisonChange, RawCursor, RawFileResponse } from '../../core/api/comparison-api';
import { changeIdentity, digest, integer, sha, uuid, validChange } from './comparison-evidence';

export interface RawScope { actor: string; proposal: string; snapshot: string; content: string; head: string; base: string; file: ComparisonChange; }
const emptyHash = 'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855';
const bytes = (value: string) => new TextEncoder().encode(value).length;
const cursorKey = (value: RawCursor) => `${value.chunk}:${value.offset}`;
const cursorValid = (v: RawCursor | null | undefined): v is RawCursor => !!v && integer(v.chunk, 1024) && integer(v.offset, 2 * 1024 * 1024);
function valid(response: RawFileResponse, scope: RawScope, cursor: RawCursor): boolean {
  if (!response || response.proposal_id !== scope.proposal || response.snapshot_id !== scope.snapshot || response.content_id !== scope.content
    || response.head_sha !== scope.head || response.merge_base_sha !== scope.base || !validChange(response.file)
    || changeIdentity(response.file) !== changeIdentity(scope.file) || !response.raw) return false;
  const raw = response.raw;
  if (raw.old_sha256 !== (scope.file.before?.sha256 ?? emptyHash) || raw.new_sha256 !== (scope.file.after?.sha256 ?? emptyHash)
    || raw.old_bytes !== (scope.file.before?.bytes ?? 0) || raw.new_bytes !== (scope.file.after?.bytes ?? 0)
    || !integer(raw.old_lines, raw.old_bytes + 1) || !integer(raw.new_lines, raw.new_bytes + 1)
    || raw.old_bytes === 0 && raw.old_lines !== 0 || raw.new_bytes === 0 && raw.new_lines !== 0
    || !integer(raw.total_chunks, 1024) || typeof raw.detailed !== 'boolean' || !cursorValid(raw.cursor)
    || cursorKey(raw.cursor) !== cursorKey(cursor) || !Array.isArray(raw.segments) || raw.segments.length > 128) return false;
  if (raw.next != null && (!cursorValid(raw.next) || raw.next.chunk >= raw.total_chunks
    || raw.next.chunk < cursor.chunk || raw.next.chunk === cursor.chunk && raw.next.offset <= cursor.offset)) return false;
  if (!raw.segments.length) return raw.next == null && raw.total_chunks === cursor.chunk && raw.returned_text_bytes === 0
    && (cursor.chunk > 0 || raw.old_bytes === 0 && raw.new_bytes === 0);
  let count = 0, oldEnd: number | undefined, newEnd: number | undefined, chunk = cursor.chunk;
  for (const segment of raw.segments) {
    if (!segment || !['equal', 'delete', 'insert'].includes(segment.kind) || segment.chunk_index !== chunk++
      || segment.chunk_index >= raw.total_chunks || typeof segment.old_text !== 'string' || typeof segment.new_text !== 'string'
      || segment.old_text.length + segment.new_text.length > 32768
      || !segment.old_text && !segment.new_text || typeof segment.continues_chunk !== 'boolean'
      || !integer(segment.old_start_column_bytes, 2 * 1024 * 1024) || !integer(segment.new_start_column_bytes, 2 * 1024 * 1024)
      || !integer(segment.old_start_byte, raw.old_bytes) || !integer(segment.old_end_byte, raw.old_bytes, segment.old_start_byte)
      || !integer(segment.new_start_byte, raw.new_bytes) || !integer(segment.new_end_byte, raw.new_bytes, segment.new_start_byte)
      || !integer(segment.old_start_line, raw.old_lines + 1, 1) || !integer(segment.new_start_line, raw.new_lines + 1, 1)
      || oldEnd != null && segment.old_start_byte !== oldEnd || newEnd != null && segment.new_start_byte !== newEnd
      || bytes(segment.old_text) !== segment.old_end_byte - segment.old_start_byte
      || bytes(segment.new_text) !== segment.new_end_byte - segment.new_start_byte) return false;
    if (segment.kind === 'equal' && segment.old_text !== segment.new_text || segment.kind === 'delete' && segment.new_text !== ''
      || segment.kind === 'insert' && segment.old_text !== '') return false;
    if (segment.continues_chunk && segment !== raw.segments.at(-1)) return false;
    count += bytes(segment.old_text) + bytes(segment.new_text);
    oldEnd = segment.old_end_byte; newEnd = segment.new_end_byte;
  }
  if (cursor.chunk === 0 && cursor.offset === 0 && (raw.segments[0].old_start_byte !== 0 || raw.segments[0].new_start_byte !== 0)) return false;
  const last = raw.segments.at(-1)!;
  const consumed = bytes(last.kind === 'insert' ? last.new_text : last.old_text);
  if (last.continues_chunk) {
    if (!raw.next || raw.next.chunk !== last.chunk_index
      || raw.next.offset !== (last.chunk_index === cursor.chunk ? cursor.offset : 0) + consumed) return false;
  } else if (raw.next ? raw.next.chunk !== last.chunk_index + 1 || raw.next.offset !== 0
    : last.chunk_index + 1 !== raw.total_chunks) return false;
  return count === raw.returned_text_bytes && count > 0 && count <= 32768
    && (raw.next != null || oldEnd === raw.old_bytes && newEnd === raw.new_bytes);
}

/** One bounded raw window in private page memory. Navigation history stores
 * coordinates only; a selection/identity change destroys source and requests. */
@Injectable()
export class RawState implements OnDestroy {
  private readonly api = inject(ComparisonApi);
  private scope: RawScope | null = null;
  private key = '';
  private generation = 0;
  private request?: Subscription;
  private cursor: RawCursor = { chunk: 0, offset: 0 };
  private history: RawCursor[] = [];
  private boundary: { old: number; next: number } | null = null;
  private metadata: string | null = null;
  readonly data = signal<RawFileResponse | null>(null);
  readonly busy = signal(false);
  readonly error = signal('');
  readonly authRequired = signal(false);
  readonly canBack = signal(false);

  open(scope: RawScope | null): void {
    if (!scope) { this.clear(); return; }
    if (!validChange(scope.file)) { this.clear(); this.error.set('מזהה הקובץ אינו תקין.'); return; }
    const key = JSON.stringify([scope.actor, scope.proposal, scope.snapshot, scope.content, scope.head, scope.base, changeIdentity(scope.file)]);
    if (key === this.key) return;
    this.clear();
    if (![scope.actor, scope.proposal, scope.snapshot].every(uuid) || !digest(scope.content) || !sha(scope.head) || !sha(scope.base)
      || !validChange(scope.file) || !['document', 'catalog'].includes(scope.file.kind)) {
      this.error.set('לא ניתן להציג את הקובץ שנבחר כהשוואת טקסט.'); return;
    }
    this.scope = structuredClone(scope); this.key = key; this.fetch();
  }
  next(): void {
    const next = this.data()?.raw.next;
    if (!next || this.busy() || this.history.length >= 512) return;
    const last = this.data()?.raw.segments.at(-1);
    this.boundary = last ? { old: last.old_end_byte, next: last.new_end_byte } : null;
    this.history.push({ ...this.cursor }); this.canBack.set(true); this.fetch(next);
  }
  back(): void {
    const previous = this.history.at(-1);
    if (!previous || this.busy()) return;
    this.history.pop(); this.boundary = null; this.canBack.set(this.history.length > 0); this.fetch(previous);
  }
  retry(): void { if (this.scope && !this.busy() && !this.authRequired()) this.fetch(this.cursor); }
  clear(): void {
    ++this.generation; this.request?.unsubscribe(); this.request = undefined; this.scope = null; this.key = '';
    this.cursor = { chunk: 0, offset: 0 }; this.history = []; this.boundary = null; this.metadata = null;
    this.data.set(null); this.busy.set(false); this.error.set(''); this.authRequired.set(false); this.canBack.set(false);
  }
  ngOnDestroy(): void { this.clear(); }
  private fetch(cursor: RawCursor = this.cursor): void {
    const scope = this.scope; if (!scope || this.busy()) return;
    const generation = ++this.generation;
    this.cursor = { ...cursor }; this.busy.set(true); this.error.set(''); this.data.set(null);
    this.request = this.api.raw(scope.actor, scope.proposal, scope.snapshot, scope.file.id, this.cursor, 32768).pipe(timeout(30_000)).subscribe({
      next: response => {
        if (generation !== this.generation || this.scope !== scope) return;
        this.busy.set(false);
        const metadata = response?.raw ? JSON.stringify([response.raw.old_lines, response.raw.new_lines, response.raw.total_chunks, response.raw.detailed]) : '';
        if (!valid(response, scope, cursor) || this.metadata != null && metadata !== this.metadata
          || this.boundary && (response.raw.segments[0]?.old_start_byte !== this.boundary.old
            || response.raw.segments[0]?.new_start_byte !== this.boundary.next)) {
          this.error.set('מקטע ההשוואה אינו תואם לגרסה שנבחרה. יש לבדוק שוב את אותו מקטע.'); return;
        }
        this.metadata = metadata; this.boundary = null; this.authRequired.set(false); this.data.set(response);
      },
      error: error => {
        if (generation !== this.generation || this.scope !== scope) return;
        this.busy.set(false); this.data.set(null);
        this.authRequired.set(error instanceof HttpErrorResponse && (error.status === 401 || error.error?.error?.code === 'SESSION_IDENTITY_CHANGED'));
        const message: unknown = error instanceof HttpErrorResponse ? error.error?.error?.message_he : null;
        this.error.set(typeof message === 'string' && message.length <= 2000 ? message : 'לא ניתן לקרוא כרגע את המקטע. אפשר לנסות שוב את אותה גרסה.');
      },
    });
  }
}
