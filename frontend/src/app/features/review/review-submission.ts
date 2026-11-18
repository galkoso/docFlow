import { computed, inject, Injectable, OnDestroy, signal } from '@angular/core';
import { HttpErrorResponse } from '@angular/common/http';
import { forkJoin, Subscription, timeout } from 'rxjs';
import { ReviewApi, type ReviewAccepted, type ReviewPublication, type ReviewRequest } from '../../core/api/review-api';
import { ReviewConfigurationApi } from '../../core/api/review-configuration-api';
import { errorCode, uuid } from '../comparison/comparison-evidence';
import { submissionContext, type SubmissionContext, validAccepted, validObservedJob, validReviewJob } from './submission-evidence';

type Phase = 'idle' | 'checking' | 'prepared' | 'sending' | 'observing' | 'uncertain' | 'queued' | 'preparing' | 'retry-wait'
  | 'reconciling' | 'completed' | 'failed' | 'error' | 'suspended';
interface Attempt { actor: string; proposal: string; key: string | null; body: Readonly<ReviewRequest> | null;
  ticket: ReviewAccepted | null; job: string | null; uncertain: boolean; }
const messages: Record<string, string> = {
  REVIEW_CONTEXT_CHANGED: 'ההצעה או הגדרת הסקירה השתנו. יש לבדוק את המצב השמור לפני בקשה חדשה.',
  REVIEW_HEAD_CHANGED: 'ענף ההצעה השתנה. יש לבדוק את הגרסה השמורה מחדש.',
  REVIEW_TARGET_CHANGED: 'האפיון המאושר התקדם. יש לבדוק את גרסת היעד מחדש.',
  REVIEW_NO_CHANGES: 'אין שינויים בענף לשליחה לסקירה.',
  REVIEW_VALIDATION_FAILED: 'נמצאו בעיות מבנה או קישורים. יש לבדוק ולתקן את התוכן לפני שליחה חדשה.',
  REVIEW_PROVIDER_RATE_LIMIT: 'ספק Git מגביל כרגע בקשות. מצב העבודה מציג אם נקבע ניסיון נוסף.',
  REVIEW_SETUP_REQUIRED: 'חיבור המאגר דורש הגדרה לפני הכנת גרסת סקירה.',
  REVIEW_FORBIDDEN: 'הרשאות הסקירה השתנו. יש לבדוק את הגישה מחדש.',
  PROPOSAL_BUSY: 'פעולה קודמת בהצעה עדיין ממתינה. יש לבדוק את תוצאתה לפני בקשה חדשה.',
  REVIEW_SUBMISSION_LIMIT: 'הגעת למגבלת הבקשות הממתינות או האחרונות. אפשר לבדוק עבודה קיימת או להמתין.',
};
/** Page-owned memory. An uncertain request retains its exact actor/key/body;
 * timers observe accepted jobs and never submit or replace mutation intents. */
@Injectable()
export class ReviewSubmission implements OnDestroy {
  private readonly api = inject(ReviewApi);
  private readonly configurations = inject(ReviewConfigurationApi);
  private attempt: Attempt | null = null;
  private request?: Subscription;
  private timer?: ReturnType<typeof setTimeout>;
  private generation = 0;
  private active = false;
  private checks = 0;
  readonly owner = signal<string | null>(null);
  readonly context = signal<SubmissionContext | null>(null);
  readonly phase = signal<Phase>('idle');
  readonly pending = signal(false);
  readonly busy = signal(false);
  readonly ticket = signal<ReviewAccepted | null>(null);
  readonly jobId = signal<string | null>(null);
  readonly result = signal<ReviewPublication | null>(null);
  readonly error = signal('');
  readonly code = signal('');
  readonly authRequired = signal(false);
  readonly accessDenied = signal(false);
  readonly canRetry = computed(() => this.pending() && !this.jobId() && !this.busy() && this.phase() === 'uncertain' && !this.authRequired());
  readonly canCheck = computed(() => this.pending() && !!this.jobId() && !this.busy() && !this.authRequired());
  prepare(actor: string, proposal: string): void {
    if (this.pending() || this.busy()) return;
    this.clear(); this.owner.set(actor); this.active = true;
    if (!uuid(actor) || !uuid(proposal)) { this.phase.set('error'); this.error.set('כתובת ההצעה או זהות החשבון אינן תקינות.'); return; }
    const generation = this.generation; this.busy.set(true); this.phase.set('checking');
    this.request = forkJoin({ configuration: this.configurations.read(actor, proposal), detail: this.api.detail(actor, proposal) })
      .pipe(timeout(20_000)).subscribe({
        next: ({ configuration, detail }) => {
          if (!this.active || generation !== this.generation) return;
          this.busy.set(false);
          const context = submissionContext(configuration, detail, proposal);
          if (!context) {
            this.phase.set('error'); this.error.set('הגרסה אינה מוכנה לשליחה. יש לבדוק הרשאת עריכה, גרסה שמורה וסוקרים כשירים ונפרדים, ולשמור שינויים בהגדרה.'); return;
          }
          this.context.set(context); this.phase.set('prepared');
        },
        error: error => {
          if (!this.active || generation !== this.generation) return;
          this.busy.set(false); this.phase.set('error'); this.error.set('לא ניתן לבדוק את הגרסה לשליחה. יש לבדוק התחברות, הרשאות וחיבור למאגר.');
          this.access(error);
        },
      });
  }
  start(): void {
    const context = this.context(), actor = this.owner();
    if (!this.active || !actor || !context || this.pending() || this.busy() || this.phase() !== 'prepared') return;
    const body = Object.freeze({ ...context.body });
    this.attempt = { actor, proposal: context.configuration.proposal_id, key: crypto.randomUUID(), body, ticket: null, job: null, uncertain: false };
    this.pending.set(true); this.context.set(null); this.send(this.attempt);
  }
  submitted(): Readonly<ReviewRequest> | null { return this.attempt?.body ?? null; }
  restore(actor: string, proposal: string, job: string): void {
    if (this.pending() || this.busy() || ![actor, proposal, job].every(uuid)) return;
    this.clear(); this.owner.set(actor); this.active = true; this.pending.set(true); this.jobId.set(job); this.phase.set('observing');
    this.attempt = { actor, proposal, job, key: null, body: null, ticket: null, uncertain: false }; this.observe();
  }
  retry(): void { if (this.active && this.attempt && this.canRetry()) this.send(this.attempt); }
  check(): void { if (this.active && this.canCheck()) { this.checks = 0; this.observe(); } }
  private send(attempt: Attempt): void {
    if (!attempt.body || !attempt.key || attempt.job) return;
    this.cancel(); const generation = this.generation;
    this.busy.set(true); this.phase.set('sending'); this.error.set(''); this.code.set('');
    this.request = this.api.submit(attempt.actor, attempt.proposal, attempt.key, attempt.body).pipe(timeout(20_000)).subscribe({
      next: ticket => {
        if (!this.current(attempt, generation)) return;
        this.busy.set(false);
        if (!validAccepted(ticket, attempt.proposal)) { attempt.uncertain = true; this.phase.set('uncertain'); this.error.set('אישור הבקשה לא אומת. אפשר לבדוק שוב במפורש את אותה בקשה.'); return; }
        attempt.ticket = Object.freeze({ ...ticket }); attempt.job = ticket.job_id; this.jobId.set(ticket.job_id);
        this.ticket.set(attempt.ticket); this.checks = 0; this.phase.set('observing'); this.observe();
      },
      error: error => {
        if (!this.current(attempt, generation)) return;
        this.busy.set(false);
        const status = error instanceof HttpErrorResponse ? error.status : 0;
        if (!status || status >= 500 || status === 408) attempt.uncertain = true;
        this.setError(error, 'תוצאת השליחה אינה מאומתת. יש לבדוק את אותה בקשה לפני שליחה חדשה.');
        this.phase.set(attempt.uncertain ? 'uncertain' : 'failed'); this.pending.set(attempt.uncertain);
        this.access(error);
      },
    });
  }
  private observe(): void {
    const attempt = this.attempt;
    if (!this.active || !attempt?.job || this.busy() || !this.pending()) return;
    this.cancel(); const generation = this.generation;
    this.busy.set(true); this.error.set('');
    this.request = this.api.job(attempt.actor, attempt.job).pipe(timeout(20_000)).subscribe({
      next: job => {
        if (!this.current(attempt, generation)) return;
        this.busy.set(false);
        const valid = attempt.ticket && attempt.body ? validReviewJob(job, attempt.ticket, attempt.body) : validObservedJob(job, attempt.proposal, attempt.job!);
        if (!valid) { this.phase.set('error'); this.error.set('מצב העבודה אינו תואם לבקשת השליחה. יש לבדוק שוב את העבודה הקיימת.'); return; }
        this.code.set(job.error_code ?? '');
        if (job.state === 'SUCCEEDED') { this.result.set(job.review!); this.pending.set(false); this.phase.set('completed'); return; }
        if (job.phase === 'FAILED') {
          this.pending.set(false); this.phase.set('failed');
          this.error.set(messages[job.error_code ?? ''] ?? 'הכנת הגרסה לא הושלמה. יש לבדוק את ההגדרה והתוכן לפני בקשה חדשה.'); return;
        }
        this.phase.set(job.phase === 'RECONCILIATION_REQUIRED' ? 'reconciling' : job.phase === 'RETRY_WAIT' ? 'retry-wait'
          : job.state === 'QUEUED' ? 'queued' : 'preparing');
        if (['FAILED', 'CANCELLED'].includes(job.state) || ++this.checks >= 450) {
          this.error.set('נדרשת בדיקה נוספת של העבודה הקיימת. מצב זה אינו אישור לכישלון או לשליחה חדשה.'); return;
        }
        this.timer = setTimeout(() => this.observe(), globalThis.document?.visibilityState === 'hidden' ? 10000 : 2000);
      },
      error: error => {
        if (!this.current(attempt, generation)) return;
        this.busy.set(false); this.phase.set('error'); this.setError(error, 'לא ניתן לבדוק כרגע את העבודה. אפשר לבדוק שוב בלי לשלוח בקשה חדשה.');
        this.access(error);
      },
    });
  }
  identity(actor: string | null, logout = false): void {
    if (logout || actor && this.owner() && actor !== this.owner()) { this.clear(); return; }
    if (!this.owner()) return;
    if (!actor) {
      if (!this.active) return;
      if (this.busy() && this.attempt?.body && !this.attempt.job) this.attempt.uncertain = true;
      this.cancel(); this.active = false; this.busy.set(false); this.context.set(null); this.result.set(null); this.phase.set('suspended');
      if (this.attempt?.job) this.pending.set(true);
    } else if (!this.active) {
      this.active = true; this.authRequired.set(false);
      this.phase.set(this.attempt?.job ? 'queued' : this.attempt?.uncertain ? 'uncertain' : 'idle');
      if (this.attempt?.job) this.check();
    }
  }
  private setError(error: unknown, fallback: string): void {
    const code: unknown = error instanceof HttpErrorResponse ? error.error?.error?.code : null;
    this.code.set(errorCode(code) ? code : ''); this.error.set(errorCode(code) && messages[code] ? messages[code] : fallback);
  }
  private access(error: unknown): void {
    if (!(error instanceof HttpErrorResponse)) return;
    if (error.status === 401 || error.error?.error?.code === 'SESSION_IDENTITY_CHANGED') {
      this.authRequired.set(true); this.identity(null); return;
    }
    if ([403, 404].includes(error.status)) {
      this.clear(); this.accessDenied.set(true); this.phase.set('error'); this.error.set('הגישה השתנתה. יש לבדוק התחברות והרשאות לפני פעולה נוספת.');
    }
  }
  private current(attempt: Attempt, generation: number): boolean { return this.active && this.attempt === attempt && generation === this.generation; }
  private cancel(): void { ++this.generation; clearTimeout(this.timer); this.timer = undefined; this.request?.unsubscribe(); this.request = undefined; }
  clear(): void {
    this.cancel(); this.attempt = null; this.active = false; this.checks = 0;
    this.owner.set(null); this.context.set(null); this.phase.set('idle'); this.pending.set(false); this.busy.set(false);
    this.ticket.set(null); this.jobId.set(null); this.result.set(null); this.error.set(''); this.code.set(''); this.authRequired.set(false); this.accessDenied.set(false);
  }
  ngOnDestroy(): void { this.clear(); }
}
