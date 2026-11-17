import { computed, inject, Injectable, OnDestroy, signal } from '@angular/core';
import { HttpErrorResponse } from '@angular/common/http';
import { Subscription, timeout } from 'rxjs';
import { ComparisonApi, ComparisonChange, ComparisonRequest, DiffResponse } from '../../core/api/comparison-api';
import { changeIdentity, digest, errorCode, integer, PAGE_SIZE, sha, summaryIdentity, uuid, validDiff } from './comparison-evidence';

type Status = 'idle' | 'loading' | 'queued' | 'publishing' | 'retry_wait' | 'ready' | 'failed' | 'error' | 'suspended' | 'waiting';
interface Intent {
  actor: string; proposal: string; request: Readonly<ComparisonRequest> | null;
  snapshot: string | null; job: string | null; summary: string | null; terminal: string | null; offset: number; file: string | null;
}
export interface ComparisonSelection { head?: string; target?: string; request_id?: string; }
/** Page-owned private state. New preparation is explicit, accepted work is only
 * observed, and no URL/session event can silently change an existing intent. */
@Injectable()
export class ComparisonState implements OnDestroy {
  private readonly api = inject(ComparisonApi);
  private intent: Intent | null = null;
  private readonly active = signal(false);
  private generation = 0;
  private polls = 0;
  private request?: Subscription;
  private timer?: ReturnType<typeof setTimeout>;
  readonly owner = signal<string | null>(null);
  readonly status = signal<Status>('idle');
  readonly busy = signal(false);
  readonly data = signal<DiffResponse | null>(null);
  readonly selected = signal<ComparisonChange | null>(null);
  readonly snapshot = signal<string | null>(null);
  readonly requestId = signal<string | null>(null);
  readonly error = signal('');
  readonly errorCode = signal('');
  readonly authRequired = signal(false);
  readonly identityChanged = signal(false);
  readonly uncertain = signal(false);
  readonly canRetry = computed(() => ['error', 'waiting'].includes(this.status()) && this.active() && !this.busy() && !!this.intent);
  readonly canCheck = computed(() => !!this.snapshot() && this.active() && !this.busy());
  readonly noChanges = computed(() => this.data()?.state === 'READY' && this.data()?.summary?.changes_total === 0);

  start(actor: string, proposal: string, selection: ComparisonSelection = {}): boolean {
    if (!this.canOwn(actor)) return false;
    if (!uuid(actor) || !uuid(proposal) || selection.head != null && !sha(selection.head)
      || selection.target != null && !sha(selection.target) || selection.request_id != null && !uuid(selection.request_id)) return this.invalidInput();
    const request = Object.freeze({ request_id: selection.request_id ?? crypto.randomUUID(),
      ...(selection.head ? { head: selection.head } : {}), ...(selection.target ? { target: selection.target } : {}) });
    if (this.intent?.proposal === proposal && this.intent.request?.request_id === request.request_id) {
      if (JSON.stringify(this.intent.request) !== JSON.stringify(request)) { this.cancel(); this.mismatch(); }
      return false;
    }
    if (this.busy() && this.intent?.proposal === proposal && !selection.request_id) return false;
    this.clear(); this.owner.set(actor); this.active.set(true); this.requestId.set(request.request_id);
    this.intent = { actor, proposal, request, snapshot: null, job: null, summary: null, terminal: null, offset: 0, file: null };
    this.fetch(); return true;
  }
  openSnapshot(actor: string, proposal: string, snapshot: string, offset = 0): boolean {
    if (!this.canOwn(actor)) return false;
    if (!uuid(actor) || !uuid(proposal) || !uuid(snapshot) || !integer(offset, 40000)) return this.invalidInput();
    if (this.intent?.actor === actor && this.intent.proposal === proposal && this.intent.snapshot === snapshot) {
      const located = this.intent.file != null; this.intent.file = null;
      if (this.intent.offset !== offset || located) {
        this.cancel(); this.intent.offset = offset; this.polls = 0; this.fetch();
      }
      return false; // Includes promotion of our receipt into the route URL.
    }
    this.clear(); this.owner.set(actor); this.active.set(true); this.snapshot.set(snapshot);
    this.intent = { actor, proposal, request: null, snapshot, job: null, summary: null, terminal: null, offset, file: null };
    this.fetch(); return true;
  }
  openFile(actor: string, proposal: string, snapshot: string, file: string): boolean {
    if (!this.canOwn(actor)) return false;
    if (!uuid(actor) || !uuid(proposal) || !uuid(snapshot) || !digest(file)) return this.invalidInput();
    if (this.intent?.actor === actor && this.intent.proposal === proposal && this.intent.snapshot === snapshot) {
      if (this.intent.file !== file && this.busy()) this.cancel();
      if (this.selected()?.id === file) { this.intent.file = file; return false; }
      if (this.data()?.page?.files.some(change => change.id === file)) {
        this.intent.file = file; this.select(file); return false;
      }
      if (this.intent.file === file) return false;
      this.cancel(); this.intent.file = file; this.selected.set(null); this.data.set(null); this.polls = 0; this.fetch(); return true;
    }
    this.clear(); this.owner.set(actor); this.active.set(true); this.snapshot.set(snapshot);
    this.intent = { actor, proposal, request: null, snapshot, job: null, summary: null, terminal: null, offset: 0, file };
    this.fetch(); return true;
  }
  files(offset: number): void {
    if (!this.active() || !this.intent?.snapshot || this.data()?.state !== 'READY' || !integer(offset, 40000)) return;
    this.cancel(); this.intent.offset = offset; this.intent.file = null; this.polls = 0; this.fetch();
  }
  select(id: string): boolean {
    if (!this.active() || this.data()?.state !== 'READY') return false;
    const file = this.data()?.page?.files.find(file => file.id === id);
    if (!file) return false;
    const captured = structuredClone(file);
    if (captured.before) Object.freeze(captured.before);
    if (captured.after) Object.freeze(captured.after);
    this.selected.set(Object.freeze(captured)); return true;
  }
  retry(): void { if (this.canRetry()) { this.polls = 0; this.fetch(); } }
  check(): void { if (this.canCheck()) { this.polls = 0; this.fetch(); } }
  identity(actor: string | null, loggedOut = false): void {
    if (loggedOut || actor && this.owner() && actor !== this.owner()) {
      const changed = !loggedOut && !!this.owner();
      this.clear(); this.identityChanged.set(changed);
      if (changed) this.error.set('ההתחברות השתנתה. יש לבחור את ההשוואה מחדש.');
      return;
    }
    if (!this.intent) return;
    if (!actor) {
      if (!this.active()) return;
      if (this.busy() && !this.intent.snapshot) this.uncertain.set(true);
      this.cancel(); this.active.set(false); this.data.set(null); this.selected.set(null); this.status.set('suspended');
    } else if (!this.active()) {
      this.active.set(true); this.authRequired.set(false);
      if (this.intent.snapshot) this.fetch(); // Observation of the accepted snapshot only.
      else { this.status.set('error'); this.error.set('אפשר לנסות במפורש את בקשת ההכנה השמורה.'); }
    }
  }
  clear(): void {
    this.cancel(); this.intent = null; this.active.set(false); this.polls = 0;
    this.owner.set(null); this.snapshot.set(null); this.requestId.set(null); this.data.set(null); this.selected.set(null);
    this.status.set('idle'); this.error.set(''); this.errorCode.set(''); this.authRequired.set(false);
    this.identityChanged.set(false); this.uncertain.set(false);
  }
  ngOnDestroy(): void { this.clear(); }
  private canOwn(actor: string): boolean {
    if (this.owner() && this.owner() !== actor) { this.identity(actor); return false; }
    return !this.owner() || this.active();
  }
  private invalidInput(): false {
    this.clear(); this.status.set('error'); this.errorCode.set('INVALID_COMPARISON_INPUT');
    this.error.set('פרטי ההשוואה אינם תקינים. יש לבחור הצעה וגרסאות תקינות.'); return false;
  }
  private cancel(): void {
    ++this.generation; this.stopTimer(); this.request?.unsubscribe(); this.request = undefined; this.busy.set(false);
  }
  private stopTimer(): void { if (this.timer !== undefined) clearTimeout(this.timer); this.timer = undefined; }
  private fetch(): void {
    const intent = this.intent;
    if (!intent || !this.active() || this.busy()) return;
    this.stopTimer(); const generation = this.generation;
    const expected = { proposal: intent.proposal, snapshot: intent.snapshot, job: intent.job, summary: intent.summary,
      request: intent.request, offset: intent.file ? null : intent.offset };
    this.busy.set(true); this.error.set(''); this.errorCode.set('');
    if (!this.data()) this.status.set('loading');
    const request = intent.snapshot ? intent.file ? this.api.filePage(intent.actor, intent.proposal, intent.snapshot, intent.file)
      : this.api.page(intent.actor, intent.proposal, intent.snapshot, intent.offset, PAGE_SIZE) : this.api.prepare(intent.actor, intent.proposal, intent.request!);
    this.request = request.pipe(timeout(20_000)).subscribe({
      next: response => {
        if (!this.current(intent, generation)) return;
        this.busy.set(false);
        if (!validDiff(response, expected) || intent.summary != null && response.state !== 'READY'
          || response.state === 'READY' && intent.file != null && !response.page?.files.some(change => change.id === intent.file)) { this.mismatch(); return; }
        const terminal = response.state === 'PENDING' ? null : JSON.stringify([response.state, response.job_state,
          response.phase, response.attempts, response.completed_at, response.error_code ?? null]);
        const known = new Map((this.data()?.page?.files ?? []).map(file => [file.id, changeIdentity(file)]));
        const selected = this.selected();
        if (selected) known.set(selected.id, changeIdentity(selected));
        if (intent.terminal != null && terminal !== intent.terminal || response.page?.files.some(file =>
          known.has(file.id) && known.get(file.id) !== changeIdentity(file))) { this.mismatch(); return; }
        if (terminal) intent.terminal = terminal;
        intent.snapshot = response.snapshot_id; intent.job = response.job_id;
        if (response.summary) intent.summary = summaryIdentity(response.summary);
        this.snapshot.set(response.snapshot_id); this.uncertain.set(false); this.data.set(structuredClone(response));
        if (response.page) intent.offset = response.page.offset;
        if (intent.file && response.state === 'READY') this.select(intent.file);
        this.status.set(response.phase.toLowerCase() as Status);
        if (response.state === 'FAILED') {
          this.errorCode.set(response.error_code ?? ''); this.error.set('הכנת ההשוואה לא הושלמה. יש לבדוק את הסיבה לפני הכנת השוואה חדשה.'); return;
        }
        if (response.state === 'READY') return;
        if (++this.polls >= 450) {
          this.status.set('waiting'); this.error.set('הכנת ההשוואה נמשכת. אפשר לבדוק שוב את אותה השוואה.'); return;
        }
        this.timer = setTimeout(() => { this.timer = undefined; this.fetch(); }, 2000);
      },
      error: error => {
        if (!this.current(intent, generation)) return;
        this.busy.set(false); this.stopTimer(); this.status.set('error');
        const status = error instanceof HttpErrorResponse ? error.status : 0;
        const code: unknown = error instanceof HttpErrorResponse ? error.error?.error?.code : null;
        const message: unknown = error instanceof HttpErrorResponse ? error.error?.error?.message_he : null;
        this.errorCode.set(errorCode(code) ? code : '');
        this.error.set(typeof message === 'string' && message.length <= 2000 ? message : 'לא ניתן לבדוק כרגע את ההשוואה. אפשר לנסות שוב את אותה בקשה.');
        if (!intent.snapshot && (!status || status >= 500 || status === 408)) this.uncertain.set(true);
        if ([401, 403, 404].includes(status) || code === 'SESSION_IDENTITY_CHANGED' || code === 'COMPARISON_CONTEXT_CHANGED') {
          this.data.set(null); this.selected.set(null);
        }
        if (status === 401 || code === 'SESSION_IDENTITY_CHANGED') {
          this.authRequired.set(true); this.active.set(false); this.status.set('suspended'); ++this.generation;
        }
      },
    });
  }
  private current(intent: Intent, generation: number): boolean { return this.active() && this.intent === intent && this.generation === generation; }
  private mismatch(): void {
    this.stopTimer(); this.data.set(null); this.selected.set(null); this.status.set('error');
    this.errorCode.set('COMPARISON_RESPONSE_MISMATCH');
    this.error.set('נתוני ההשוואה אינם תואמים לבקשה השמורה. יש לבדוק שוב את אותה השוואה.');
  }
}
