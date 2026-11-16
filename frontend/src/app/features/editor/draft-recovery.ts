import { computed, inject, Injectable, signal } from '@angular/core';
import { HttpErrorResponse } from '@angular/common/http';
import { Subscription, timeout } from 'rxjs';
import { DraftBuffer, DraftSaved, DraftSnapshot, EditorApi, SaveDraft } from '../../core/api/editor-api';

export interface RecoveryScope { actor: string; proposal: string; document: string; base: string; }
export type RecoveryStatus = 'loading' | 'idle' | 'waiting' | 'saving' | 'saved' | 'uncertain' | 'conflict' | 'disabled' | 'error' | 'suspended';
interface Attempt { scope: RecoveryScope; body: Readonly<SaveDraft>; uncertain: boolean; }
type Context = DraftSnapshot['context'];
const MAX_BYTES = 2 * 1024 * 1024;

/** Component-owned, memory-only recovery. No Git writes and no automatic retry.
 * The caller must gate private rendering by owner and call identity on changes. */
@Injectable()
export class DraftRecovery {
  private readonly api = inject(EditorApi);
  private scope: RecoveryScope | null = null;
  private original = '';
  private acknowledged: string | null = null;
  private version = 0;
  private loaded = false;
  private active = false;
  private generation = 0;
  private timer?: ReturnType<typeof setTimeout>;
  private reading?: Subscription;
  private writing?: Subscription;
  private attempt: Attempt | null = null;
  private busy = false;
  private blocked = false;
  private inputReady = true;
  readonly owner = signal<string | null>(null);
  readonly content = signal('');
  readonly status = signal<RecoveryStatus>('idle');
  readonly context = signal<Context | null>(null);
  readonly remote = signal<DraftBuffer | null>(null);
  readonly choiceRequired = signal(false);
  readonly error = signal('');
  readonly authRequired = signal(false);
  readonly savedBuffer = signal<DraftBuffer | null>(null);
  readonly dirty = signal(false);
  readonly pending = signal(false);
  readonly uncertain = signal(false);
  readonly canRetry = computed(() => this.pending() && !['saving', 'suspended', 'loading'].includes(this.status()));

  open(scope: RecoveryScope, original: string): void {
    this.clear();
    this.scope = { ...scope }; this.owner.set(scope.actor); this.original = original;
    this.content.set(original); this.active = true;
    this.inspect();
  }

  /** Null means temporarily signed out/refreshing; a different login or a
   * successful explicit logout purges all source and frozen requests. */
  identity(actor: string | null, loggedOut = false): void {
    if (!this.scope) return;
    if (loggedOut || (actor && actor !== this.scope.actor)) { this.clear(); return; }
    if (!actor) {
      if (!this.active) return;
      this.active = false; ++this.generation; this.stopTimer(); this.reading?.unsubscribe();
      if (this.busy && this.attempt) { this.attempt.uncertain = true; this.uncertain.set(true); }
      this.writing?.unsubscribe(); this.busy = false; this.status.set('suspended');
    } else if (!this.active) {
      this.active = true; this.authRequired.set(false); this.inspect();
    }
  }

  update(content: string): void {
    if (!this.scope) return;
    this.inputReady = true;
    this.content.set(content); this.dirty.set(content !== this.original);
    this.stopTimer();
    if (!this.validContent(content)) {
      this.status.set('error'); this.error.set('טיוטת ההתאוששות מוגבלת ל־2MiB ואינה יכולה לכלול תו אפס.'); return;
    }
    this.queue();
  }

  /** Preserve the last serializable Markdown but never call it a backup of a
   * newer rich document that cannot currently be represented. */
  holdInput(): void {
    this.inputReady = false; this.stopTimer();
    if (!this.busy && !this.attempt && this.active) this.inputFailure();
  }

  /** GET is safe to repeat, including while a previous write is uncertain.
   * Observing a buffer never acknowledges an unknown save or overwrites local input. */
  inspect(): void {
    const scope = this.scope;
    if (!scope || !this.active || this.busy) return;
    this.stopTimer(); this.reading?.unsubscribe();
    const generation = ++this.generation;
    this.status.set('loading'); this.error.set('');
    this.reading = this.api.recovery(scope.actor, scope.proposal, scope.document).pipe(timeout(20_000)).subscribe({
      next: snapshot => {
        if (generation !== this.generation || !this.active) return;
        if (!this.validSnapshot(snapshot, scope)) { this.failure('זהות טיוטת ההתאוששות אינה תואמת למסמך.'); return; }
        this.context.set(snapshot.context);
        const buffer = snapshot.buffers[0] ?? null;
        this.remote.set(buffer);
        if (this.attempt) {
          this.status.set(this.attempt.uncertain ? 'uncertain' : 'conflict');
          return;
        }
        const changed = !!buffer && (!this.loaded || buffer.version !== this.version
          || buffer.content_utf8 !== this.acknowledged || buffer.base_head_sha !== scope.base);
        this.loaded = true; this.blocked = false;
        this.choiceRequired.set(changed);
        if (!changed) {
          this.version = buffer?.version ?? 0; this.acknowledged = buffer?.content_utf8 ?? null;
          this.savedBuffer.set(buffer); this.queue();
        } else this.status.set('idle');
      },
      error: error => { if (generation === this.generation) this.readFailed(error); },
    });
  }

  /** Explicitly keep local text instead of the inspected recovery buffer.
   * Also permits a deliberate CAS replacement after an uncertain write, but
   * only after the UI has shown that uncertainty and the latest remote copy. */
  keepLocal(): void {
    if (!this.active || this.busy || this.status() === 'loading' || !this.loaded || !this.remote()) return;
    const buffer = this.remote()!;
    this.releaseAttempt(); this.version = buffer.version;
    this.acknowledged = buffer.base_head_sha === this.scope?.base ? buffer.content_utf8 ?? null : null;
    this.savedBuffer.set(buffer); this.choiceRequired.set(false); this.blocked = false;
    this.queue();
  }

  /** A changed base requires a separate explicit adoption decision in the UI. */
  restore(adoptDifferentBase = false): boolean {
    const buffer = this.remote(), scope = this.scope;
    if (!scope || !buffer || !this.active || this.busy || this.status() === 'loading' || buffer.content_utf8 == null
      || (buffer.base_head_sha !== scope.base && !adoptDifferentBase)) return false;
    this.releaseAttempt(); this.version = buffer.version;
    this.acknowledged = buffer.base_head_sha === scope.base ? buffer.content_utf8 : null;
    this.savedBuffer.set(buffer); this.choiceRequired.set(false); this.blocked = false;
    this.update(buffer.content_utf8);
    return true;
  }

  flush(): void { this.stopTimer(); this.save(); }

  retry(): void {
    if (!this.attempt || !this.active || this.busy || this.status() === 'loading') return;
    this.send(this.attempt);
  }

  private queue(): void {
    if (!this.active || !this.loaded || this.choiceRequired() || this.busy || this.attempt || this.blocked) return;
    if (!this.inputReady) { this.inputFailure(); return; }
    if (!this.context()?.can_store_recovery || !this.context()?.retention_days) { this.status.set('disabled'); return; }
    if (!this.needsSave()) { this.status.set(this.version ? 'saved' : 'idle'); return; }
    if (!this.validContent(this.content())) return;
    this.error.set(''); this.status.set('waiting');
    this.timer = setTimeout(() => { this.timer = undefined; this.save(); }, 2000);
  }

  private save(): void {
    const scope = this.scope;
    if (!scope || !this.active || !this.loaded || !this.inputReady || this.choiceRequired() || this.busy || this.attempt || this.blocked
      || !this.context()?.can_store_recovery || !this.context()?.retention_days || !this.needsSave() || !this.validContent(this.content())) return;
    const attempt: Attempt = { scope: { ...scope }, uncertain: false, body: Object.freeze({ save_id: crypto.randomUUID(),
      document_id: scope.document, base_head_sha: scope.base, expected_draft_version: this.version, content_utf8: this.content() }) };
    this.attempt = attempt; this.pending.set(true); this.send(attempt);
  }

  private send(attempt: Attempt): void {
    const generation = this.generation;
    this.busy = true; this.status.set('saving'); this.error.set('');
    this.writing = this.api.saveRecovery(attempt.scope.actor, attempt.scope.proposal, attempt.body).pipe(timeout(20_000)).subscribe({
      next: saved => {
        if (generation !== this.generation || this.attempt !== attempt) return;
        this.busy = false;
        if (!this.validAck(saved, attempt)) {
          attempt.uncertain = true; this.uncertain.set(true); this.status.set('uncertain');
          this.error.set('תשובת השמירה אינה תואמת לבקשה. הטקסט נשאר מקומי; נדרש בירור או ניסיון חוזר.'); return;
        }
        this.context.set(saved.context); this.savedBuffer.set(saved.buffer);
        this.remote.set({ ...saved.buffer, content_utf8: attempt.body.content_utf8 });
        this.version = saved.buffer.version; this.acknowledged = attempt.body.content_utf8;
        this.releaseAttempt(); this.blocked = false; this.queue();
      },
      error: error => {
        if (generation !== this.generation || this.attempt !== attempt) return;
        this.busy = false;
        const status = error instanceof HttpErrorResponse ? error.status : 0;
        if (!status || status >= 500 || [408, 429].includes(status)) attempt.uncertain = true;
        this.uncertain.set(attempt.uncertain);
        this.status.set(attempt.uncertain ? 'uncertain' : status === 409 ? 'conflict' : 'error');
        this.error.set(this.message(error, 'טיוטת ההתאוששות לא אושרה. הטקסט נשאר בדפדפן; אפשר לנסות את אותה בקשה שוב.'));
        if (status === 401 || this.code(error) === 'SESSION_IDENTITY_CHANGED') {
          this.authRequired.set(true); this.identity(null);
        }
      },
    });
  }

  private validSnapshot(value: DraftSnapshot, scope: RecoveryScope): boolean {
    return this.validContext(value?.context, scope) && Array.isArray(value?.buffers) && value.buffers.length <= 1
      && value.buffers.every(buffer => this.validBuffer(buffer, scope.document) && typeof buffer.content_utf8 === 'string'
        && this.validContent(buffer.content_utf8));
  }
  private validContext(context: Context, scope: RecoveryScope): boolean {
    return !!context && context.proposal_id === scope.proposal && Number.isSafeInteger(context.proposal_version)
      && context.proposal_version > 0 && typeof context.can_store_recovery === 'boolean' && typeof context.can_edit === 'boolean'
      && (context.retention_days == null || (Number.isInteger(context.retention_days) && context.retention_days >= 1 && context.retention_days <= 365))
      && (context.current_head_sha == null || /^[a-f0-9]{40,128}$/.test(context.current_head_sha));
  }
  private validBuffer(buffer: DraftBuffer, document: string): boolean {
    return !!buffer && buffer.document_id === document && Number.isSafeInteger(buffer.version) && buffer.version > 0
      && /^[a-f0-9]{40,128}$/.test(buffer.base_head_sha) && Number.isFinite(Date.parse(buffer.updated_at))
      && Number.isFinite(Date.parse(buffer.expires_at)) && Date.parse(buffer.expires_at) > Date.parse(buffer.updated_at);
  }
  private validAck(value: DraftSaved, attempt: Attempt): boolean {
    return this.validContext(value?.context, attempt.scope) && this.validBuffer(value?.buffer, attempt.scope.document)
      && value.buffer.base_head_sha === attempt.body.base_head_sha && value.buffer.version > attempt.body.expected_draft_version
      && (value.buffer.content_utf8 == null || value.buffer.content_utf8 === attempt.body.content_utf8);
  }
  private validContent(value: string): boolean { return !value.includes('\0') && new TextEncoder().encode(value).length <= MAX_BYTES; }
  private needsSave(): boolean { return this.version ? this.content() !== this.acknowledged : this.content() !== this.original; }
  private code(error: unknown): unknown { return error instanceof HttpErrorResponse ? error.error?.error?.code : null; }
  private message(error: unknown, fallback: string): string {
    const message: unknown = error instanceof HttpErrorResponse ? error.error?.error?.message_he : null;
    return typeof message === 'string' ? message : fallback;
  }
  private readFailed(error: unknown): void {
    this.failure(this.message(error, 'לא ניתן לבדוק כרגע טיוטת התאוששות. העריכה המקומית לא הוחלפה.'));
    if (error instanceof HttpErrorResponse && (error.status === 401 || this.code(error) === 'SESSION_IDENTITY_CHANGED')) {
      this.authRequired.set(true); this.identity(null);
    }
  }
  private failure(message: string): void { this.blocked = true; this.error.set(message); this.status.set('error'); }
  private inputFailure(): void {
    this.status.set('error'); this.error.set('יש בעורך שינוי שאינו מיוצג ב־Markdown ולכן עדיין אינו כלול בגיבוי. יש לתקן, לבטל או להוריד עותק של מצב העורך.');
  }
  private stopTimer(): void { if (this.timer !== undefined) clearTimeout(this.timer); this.timer = undefined; }
  private releaseAttempt(): void { this.attempt = null; this.pending.set(false); this.uncertain.set(false); }
  clear(): void {
    ++this.generation; this.stopTimer(); this.reading?.unsubscribe(); this.writing?.unsubscribe();
    this.scope = null; this.original = ''; this.acknowledged = null; this.version = 0; this.loaded = false;
    this.active = false; this.busy = false; this.blocked = false; this.inputReady = true; this.releaseAttempt();
    this.owner.set(null); this.content.set(''); this.status.set('idle'); this.context.set(null); this.remote.set(null);
    this.choiceRequired.set(false); this.error.set(''); this.authRequired.set(false); this.savedBuffer.set(null); this.dirty.set(false);
  }
}
