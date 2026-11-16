import { computed, inject, Injectable, OnDestroy, signal } from '@angular/core';
import { HttpErrorResponse } from '@angular/common/http';
import { Subscription, timeout } from 'rxjs';
import { EditorApi, SaveAccepted, SaveJob, SaveRequest, SaveResult } from '../../core/api/editor-api';

type Status = 'idle' | 'sending' | 'uncertain' | 'queued' | 'running' | 'reconciling' | 'failed' | 'completed' | 'suspended' | 'error';
interface Attempt { actor: string; proposal: string; key: string; body: SaveRequest; ticket: SaveAccepted | null; uncertain: boolean; }
const MAX_FILE = 2 * 1024 * 1024;
const MAX_BATCH = 10 * 1024 * 1024;
const sha = (value: unknown): value is string => typeof value === 'string' && /^(?:[a-f0-9]{2}){20,64}$/.test(value);
const digest = (value: unknown): value is string => typeof value === 'string' && /^[a-f0-9]{64}$/.test(value);
const uuid = (value: unknown): value is string => typeof value === 'string'
  && /^[a-f0-9]{8}-(?:[a-f0-9]{4}-){3}[a-f0-9]{12}$/.test(value) && value !== '00000000-0000-0000-0000-000000000000';
const documentId = (value: unknown): value is string => typeof value === 'string' && /^[A-Z0-9-]{1,120}$/.test(value);
const code = (value: unknown): value is string => typeof value === 'string' && /^[A-Z0-9_]{1,100}$/.test(value);

/** Component-owned private memory. One frozen mutation intent, observation-only
 * polling after acceptance, and no write retry on a timer or session refresh.
 * Completion remains pending until the editor applies its verified baseline. */
@Injectable()
export class GitSave implements OnDestroy {
  private readonly api = inject(EditorApi);
  private attempt: Attempt | null = null;
  private active = false;
  private generation = 0;
  private request?: Subscription;
  private timer?: ReturnType<typeof setTimeout>;
  readonly owner = signal<string | null>(null);
  readonly status = signal<Status>('idle');
  readonly busy = signal(false);
  readonly pending = signal(false);
  readonly uncertain = signal(false);
  readonly ticket = signal<SaveAccepted | null>(null);
  readonly result = signal<SaveResult | null>(null);
  readonly failure = signal<SaveJob['save_failure']>(null);
  readonly error = signal('');
  readonly errorCode = signal('');
  readonly authRequired = signal(false);
  readonly contextChanged = signal(false);
  readonly canRetry = computed(() => this.pending() && !this.ticket() && !this.busy()
    && ['uncertain', 'error'].includes(this.status()));
  readonly canCheck = computed(() => this.pending() && !!this.ticket() && !this.busy()
    && !['completed', 'suspended'].includes(this.status()));

  start(actor: string, proposal: string, input: SaveRequest): boolean {
    if (this.pending() || this.busy() || this.owner() && (this.owner() !== actor || !this.active)) return false;
    if (!actor || !proposal || !this.validInput(input)) {
      this.status.set('error'); this.error.set('פרטי השמירה חורגים מהמגבלות או אינם תקינים. הטקסט המקומי לא השתנה.'); return false;
    }
    this.clear(); this.owner.set(actor); this.active = true;
    // Copy before freezing: later typing and changes to the caller's arrays must
    // never alter the request retried under this key.
    const body = structuredClone(input);
    body.documents.forEach(document => Object.freeze(document)); Object.freeze(body.documents); Object.freeze(body);
    this.attempt = { actor, proposal, key: crypto.randomUUID(), body, ticket: null, uncertain: false };
    this.pending.set(true); this.send(this.attempt); return true;
  }
  submitted(): Readonly<SaveRequest> | null { return this.attempt?.body ?? null; }

  retry(): void {
    if (this.attempt && this.active && this.canRetry()) this.send(this.attempt);
  }
  check(): void {
    const attempt = this.attempt;
    if (!attempt?.ticket || !this.active || !this.canCheck()) return;
    this.stopTimer(); const generation = this.generation;
    this.busy.set(true); this.error.set('');
    this.request = this.api.saveJob(attempt.actor, attempt.ticket.job_id).pipe(timeout(20_000)).subscribe({
      next: job => {
        if (!this.current(attempt, generation)) return;
        this.busy.set(false);
        if (!this.validJob(job, attempt)) { this.observationError('תוצאת העבודה אינה תואמת לבקשת השמירה. נדרשת בדיקה מחדש.'); return; }
        this.errorCode.set(job.error_code ?? ''); this.failure.set(this.safeFailure(job.save_failure));
        if (job.phase === 'READY') {
          this.result.set(job.save!); this.uncertain.set(false); this.status.set('completed');
          return;
        }
        if (job.phase === 'FAILED') {
          this.pending.set(false); this.uncertain.set(false); this.status.set('failed');
          this.error.set('השמירה לא הושלמה. יש לבדוק את הממצאים והגרסה לפני יצירת בקשה חדשה.'); return;
        }
        const reconciling = job.phase === 'RECONCILIATION_REQUIRED';
        this.uncertain.set(reconciling);
        this.status.set(reconciling ? 'reconciling' : job.phase === 'QUEUED' ? 'queued' : 'running');
        // An exhausted uncertain operation keeps its reservation. Further
        // observation is explicit; it can never become a new write here.
        if (!['FAILED', 'CANCELLED'].includes(job.state)) this.timer = setTimeout(() => { this.timer = undefined; this.check(); }, 2000);
      },
      error: error => {
        if (!this.current(attempt, generation)) return;
        this.busy.set(false); this.observationError(this.message(error, 'לא ניתן לבדוק כרגע את העבודה. אין בכך הוכחה שהשמירה נכשלה.'));
        this.accessFailure(error);
      },
    });
  }

  /** Call only after pinned source at result.head_sha is checked and installed.
   * Until then a completed operation still guards navigation and another save. */
  applied(job: string): boolean {
    if (this.status() !== 'completed' || !this.result() || this.attempt?.ticket?.job_id !== job || !this.active) return false;
    this.attempt = null; this.pending.set(false); return true;
  }

  identity(actor: string | null, loggedOut = false): void {
    if (loggedOut || actor && this.owner() && actor !== this.owner()) { this.clear(); return; }
    if (!this.owner()) return;
    if (!actor) {
      if (!this.active) return;
      if (this.busy() && this.attempt && !this.attempt.ticket) this.attempt.uncertain = true;
      this.active = false; ++this.generation; this.stopTimer(); this.request?.unsubscribe(); this.busy.set(false);
      this.uncertain.set(this.attempt?.uncertain ?? this.uncertain()); this.status.set('suspended');
    } else if (!this.active) {
      this.active = true; this.authRequired.set(false);
      if (!this.pending()) { this.status.set(this.result() ? 'completed' : this.attempt ? 'failed' : 'idle'); return; }
      if (this.result()) { this.status.set('completed'); return; }
      this.status.set(this.attempt?.ticket ? 'queued' : this.attempt?.uncertain ? 'uncertain' : 'error');
      // Only safe observation resumes. A missing submission receipt requires an
      // explicit retry with the original actor/key/body.
      if (this.attempt?.ticket) this.check();
    }
  }

  private send(attempt: Attempt): void {
    this.stopTimer(); const generation = this.generation;
    this.busy.set(true); this.status.set('sending'); this.error.set(''); this.errorCode.set('');
    this.request = this.api.saveCommit(attempt.actor, attempt.proposal, attempt.key, attempt.body).pipe(timeout(20_000)).subscribe({
      next: ticket => {
        if (!this.current(attempt, generation)) return;
        this.busy.set(false);
        if (!ticket || ticket.proposal_id !== attempt.proposal || !uuid(ticket.job_id) || !uuid(ticket.operation_id)) {
          attempt.uncertain = true; this.uncertain.set(true); this.status.set('uncertain');
          this.error.set('אישור הבקשה אינו תואם להצעה. אפשר לנסות במפורש את אותה בקשה.'); return;
        }
        attempt.ticket = Object.freeze({ ...ticket }); this.ticket.set(attempt.ticket); this.status.set('queued'); this.check();
      },
      error: error => {
        if (!this.current(attempt, generation)) return;
        this.busy.set(false);
        const status = error instanceof HttpErrorResponse ? error.status : 0;
        const auth = status === 401 || this.code(error) === 'SESSION_IDENTITY_CHANGED';
        if (!status || status >= 500 || [408, 429].includes(status)) attempt.uncertain = true;
        this.uncertain.set(attempt.uncertain);
        this.errorCode.set(code(this.code(error)) ? this.code(error) as string : '');
        this.error.set(this.message(error, 'הבקשה לא אושרה. הטקסט המקומי נשמר; נדרש בירור לפני בקשה חדשה.'));
        this.status.set(attempt.uncertain ? 'uncertain' : auth ? 'error' : 'failed');
        if (!attempt.uncertain && !auth) this.pending.set(false);
        this.accessFailure(error);
      },
    });
  }
  private current(attempt: Attempt, generation: number): boolean { return this.active && this.attempt === attempt && generation === this.generation; }
  private validInput(body: SaveRequest): boolean {
    if (!body || !sha(body.expected_head_sha) || !Number.isSafeInteger(body.expected_proposal_version)
      || body.expected_proposal_version < 1 || body.expected_proposal_version >= Number.MAX_SAFE_INTEGER
      || typeof body.message !== 'string' || !body.message.trim() || body.message.length > 20000
      || Array.from(body.message).length > 10000 || body.message.includes('\0')
      || !Array.isArray(body.documents) || !body.documents.length || body.documents.length > 50) return false;
    const ids = new Set<string>(); let total = 0;
    return body.documents.every(document => {
      if (!document || !documentId(document.document_id) || ids.has(document.document_id) || !digest(document.expected_document_sha256)
        || typeof document.markdown !== 'string' || document.markdown.includes('\0')) return false;
      ids.add(document.document_id); const size = new TextEncoder().encode(document.markdown).length; total += size;
      return size <= MAX_FILE && total <= MAX_BATCH;
    });
  }
  private validJob(job: SaveJob, attempt: Attempt): boolean {
    if (!job || job.job_id !== attempt.ticket?.job_id || job.proposal_id !== attempt.proposal || job.kind !== 'SAVE_PROPOSAL'
      || !Number.isSafeInteger(job.attempts) || job.attempts < 0 || job.error_code != null && !code(job.error_code)
      || !['QUEUED', 'RUNNING', 'SUCCEEDED', 'FAILED', 'CANCELLED'].includes(job.state)) return false;
    if (job.state === 'SUCCEEDED') return job.phase === 'READY' && this.validResult(job.save, attempt);
    if (job.save != null || job.phase === 'READY') return false;
    if (job.phase === 'RECONCILIATION_REQUIRED') return true;
    if (['FAILED', 'CANCELLED'].includes(job.state)) return job.phase === 'FAILED';
    return ['QUEUED', 'PREFLIGHT', 'SAVING'].includes(job.phase);
  }
  private validResult(value: SaveResult | null | undefined, attempt: Attempt): value is SaveResult {
    if (!value || value.proposal_id !== attempt.proposal || !sha(value.head_sha) || typeof value.no_op !== 'boolean'
      || typeof value.reconciled !== 'boolean' || !Array.isArray(value.documents) || value.documents.length > attempt.body.documents.length) return false;
    if (value.no_op) return value.head_sha === attempt.body.expected_head_sha && value.proposal_version === attempt.body.expected_proposal_version
      && value.committed_at == null && !value.reconciled && value.documents.length === 0;
    if (value.head_sha === attempt.body.expected_head_sha || value.proposal_version !== attempt.body.expected_proposal_version + 1
      || typeof value.committed_at !== 'string' || value.committed_at.length > 64 || !Number.isFinite(Date.parse(value.committed_at))
      || !value.documents.length) return false;
    const requested = new Set(attempt.body.documents.map(document => document.document_id));
    const paths = new Set<string>(); const ids = new Set<string>(); let total = 0;
    return value.documents.every(document => {
      if (!document || !requested.has(document.document_id) || ids.has(document.document_id) || !digest(document.sha256)
        || typeof document.path !== 'string' || !document.path.startsWith('docs/') || !document.path.endsWith('.md')
        || paths.has(document.path.toLowerCase()) || !Number.isSafeInteger(document.bytes) || document.bytes < 0 || document.bytes > MAX_FILE
        || typeof document.normalization_changed !== 'boolean' || typeof document.normalization_only !== 'boolean') return false;
      ids.add(document.document_id); paths.add(document.path.toLowerCase()); total += document.bytes; return total <= MAX_BATCH;
    });
  }
  private safeFailure(value: SaveJob['save_failure']): SaveJob['save_failure'] {
    if (!value || typeof value !== 'object') return null;
    const validation = value.validation;
    const valid = validation && Array.isArray(validation.findings) && validation.findings.length <= 1000
      && Number.isSafeInteger(validation.findings_total) && validation.findings_total >= validation.findings.length
      && Number.isSafeInteger(validation.blockers_total) && validation.blockers_total >= 0 && validation.blockers_total <= validation.findings_total
      && typeof validation.complete === 'boolean' && typeof validation.findings_truncated === 'boolean'
      && validation.findings.every(finding => finding && code(finding.code) && typeof finding.path === 'string'
        && Array.from(finding.path).length <= 240 && ['warning', 'blocker'].includes(finding.severity)
        && typeof finding.path_truncated === 'boolean' && [finding.source_start_byte, finding.source_end_byte]
          .every(offset => offset == null || Number.isSafeInteger(offset) && offset >= 0 && offset <= MAX_FILE));
    return { document_id: documentId(value.document_id) ? value.document_id : null,
      validation_code: code(value.validation_code) ? value.validation_code : null, validation: valid ? validation : null };
  }
  private observationError(message: string): void { this.status.set('error'); this.error.set(message); }
  private accessFailure(error: unknown): void {
    const status = error instanceof HttpErrorResponse ? error.status : 0;
    if ([403, 404, 409].includes(status)) this.contextChanged.set(true);
    if (status === 401 || this.code(error) === 'SESSION_IDENTITY_CHANGED') { this.authRequired.set(true); this.identity(null); }
  }
  private code(error: unknown): unknown { return error instanceof HttpErrorResponse ? error.error?.error?.code : null; }
  private message(error: unknown, fallback: string): string {
    const value: unknown = error instanceof HttpErrorResponse ? error.error?.error?.message_he : null;
    return typeof value === 'string' ? value : fallback;
  }
  private stopTimer(): void { if (this.timer !== undefined) clearTimeout(this.timer); this.timer = undefined; }
  clear(): void {
    ++this.generation; this.stopTimer(); this.request?.unsubscribe(); this.request = undefined; this.attempt = null; this.active = false;
    this.owner.set(null); this.status.set('idle'); this.busy.set(false); this.pending.set(false); this.uncertain.set(false);
    this.ticket.set(null); this.result.set(null); this.failure.set(null); this.error.set(''); this.errorCode.set('');
    this.authRequired.set(false); this.contextChanged.set(false);
  }
  ngOnDestroy(): void { this.clear(); }
}
