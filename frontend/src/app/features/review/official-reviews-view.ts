import { ChangeDetectionStrategy, Component, computed, effect, inject, input, OnDestroy, output, signal, untracked } from '@angular/core';
import { DatePipe } from '@angular/common';
import { FormsModule } from '@angular/forms';
import { Subscription, timeout } from 'rxjs';
import { ReviewApi, type OfficialHistory, type OfficialRequest, type ReviewSnapshot } from '../../core/api/review-api';
import { SessionState } from '../../core/auth/session-state';
import { reviewDecisions, validOfficialHistory, validOfficialReceipt } from './official-review-evidence';

type Attempt = { body: Readonly<OfficialRequest>; actor: string; proposal: string; snapshot: ReviewSnapshot; epoch: number };
@Component({
  selector: 'app-official-reviews', imports: [DatePipe, FormsModule], changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: './official-reviews-view.html', styleUrl: './official-reviews-view.css',
})
export class OfficialReviewsView implements OnDestroy {
  readonly actor = input.required<string>();
  readonly proposal = input.required<string>();
  readonly snapshot = input.required<ReviewSnapshot>();
  readonly accessDenied = output<void>();
  readonly recorded = output<void>();
  private readonly api = inject(ReviewApi);
  private readonly session = inject(SessionState);
  private generation = 0;
  private epoch = -1;
  private owner = '';
  private reads?: Subscription;
  private writes?: Subscription;
  private retryTimer?: ReturnType<typeof setInterval>;
  private readonly now = signal(Date.now());
  private readonly retryUntil = signal(0);
  readonly retryWait = computed(() => Math.max(0, Math.ceil((this.retryUntil() - this.now()) / 1000)));
  private readonly data = signal<OfficialHistory | null>(null);
  readonly pending = signal<Attempt | null>(null);
  readonly busy = signal(false);
  readonly sending = signal(false);
  readonly error = signal('');
  readonly message = signal('');
  readonly explanation = signal('');
  readonly domain = signal<OfficialRequest['domain']>('PRODUCT');
  readonly decision = signal<OfficialRequest['decision']>('COMMENT');
  readonly hasDraft = computed(() => !!this.explanation().trim() || !!this.pending() || this.sending());
  readonly length = computed(() => Array.from(this.explanation()).length);
  readonly view = computed(() => {
    const value = this.data(); if (!value) return null;
    const snapshot = this.snapshot();
    return this.session.profile()?.user_id === this.owner && this.actor() === this.owner && this.session.logoutEpoch() === this.epoch
      && value?.proposal_id === this.proposal() && value.snapshot_id === snapshot.snapshot_id && value.fingerprint === snapshot.fingerprint ? value : null;
  });
  readonly canSend = computed(() => {
    const v = this.view();
    return !!v && !this.busy() && !this.sending() && !this.pending() && !this.retryWait() && v.domains.includes(this.domain())
      && reviewDecisions.includes(this.decision()) && !!this.explanation().trim() && this.length() <= 10000
      && (this.decision() !== 'APPROVE_ALL' || v.approve_all_available);
  });
  constructor() {
    effect(() => {
      const actor = this.actor(), proposal = this.proposal(), snapshot = this.snapshot(), live = this.session.profile()?.user_id, epoch = this.session.logoutEpoch();
      untracked(() => { this.clear(); this.owner = actor; this.epoch = epoch; if (actor === live) this.load(actor, proposal, snapshot); });
    });
  }
  private matches(actor: string, proposal: string, snapshot: ReviewSnapshot, epoch: number): boolean {
    return this.session.profile()?.user_id === actor && this.session.logoutEpoch() === epoch && this.actor() === actor
      && this.proposal() === proposal && this.snapshot() === snapshot;
  }
  refresh(after?: string): void {
    if (!this.sending() && this.matches(this.owner, this.proposal(), this.snapshot(), this.epoch)) this.load(this.owner, this.proposal(), this.snapshot(), after);
  }
  private load(actor: string, proposal: string, snapshot: ReviewSnapshot, after?: string): void {
    this.reads?.unsubscribe(); const generation = ++this.generation, epoch = this.epoch;
    this.busy.set(true); this.error.set(''); this.data.set(null);
    this.reads = this.api.decisions(actor, proposal, snapshot.snapshot_id, after).pipe(timeout(20000)).subscribe({
      next: value => {
        if (generation !== this.generation || !this.matches(actor, proposal, snapshot, epoch)) return;
        this.busy.set(false);
        if (!validOfficialHistory(value, actor, proposal, snapshot, after)) { this.error.set('פרטי החלטות הסקירה אינם תואמים לגרסה.'); return; }
        this.data.set(value);
        if (!this.pending() && !value.domains.includes(this.domain()) && value.domains.length) this.domain.set(value.domains[0]);
      },
      error: error => {
        if (generation !== this.generation || !this.matches(actor, proposal, snapshot, epoch)) return;
        this.busy.set(false);
        if (this.denied(error)) { this.clear(); this.accessDenied.emit(); return; }
        this.error.set('לא ניתן לטעון את החלטות הסקירה כרגע. גרסת התוכן השמורה עדיין זמינה.');
      },
    });
  }
  send(): void {
    if (!this.canSend()) return;
    const body = Object.freeze({ request_id: crypto.randomUUID(), expected_fingerprint: this.snapshot().fingerprint,
      domain: this.domain(), decision: this.decision(), explanation: this.explanation().trim() });
    this.pending.set({ body, actor: this.actor(), proposal: this.proposal(), snapshot: this.snapshot(), epoch: this.epoch });
    this.retry();
  }
  retry(): void {
    const attempt = this.pending();
    if (!attempt || this.busy() || this.sending() || this.retryWait() || !this.matches(attempt.actor, attempt.proposal, attempt.snapshot, attempt.epoch)) return;
    const { actor, proposal, snapshot, epoch, body } = attempt, generation = ++this.generation;
    this.reads?.unsubscribe(); this.sending.set(true); this.error.set(''); this.message.set('');
    this.writes = this.api.recordDecision(actor, proposal, snapshot.snapshot_id, body).pipe(timeout(20000)).subscribe({
      next: value => {
        if (generation !== this.generation || !this.matches(actor, proposal, snapshot, epoch)) return;
        this.sending.set(false);
        if (!validOfficialReceipt(value, actor, proposal, snapshot, body)) { this.error.set('תוצאת השליחה אינה תואמת לבקשה. יש לבדוק שוב את אותה בקשה.'); return; }
        this.pending.set(null); this.explanation.set(''); this.message.set('החלטת הסקירה נשמרה לגרסה זו. אישור סופי ומיזוג נבדקים בנפרד.');
        this.load(actor, proposal, snapshot); this.recorded.emit();
      },
      error: error => {
        if (generation !== this.generation || !this.matches(actor, proposal, snapshot, epoch)) return;
        this.sending.set(false);
        if (this.denied(error)) { this.clear(); this.accessDenied.emit(); return; }
        const wait = error?.headers?.get?.('Retry-After') ?? '';
        if (/^[1-9]\d{0,9}$/.test(wait) && Number(wait) <= 4294967295) {
          clearInterval(this.retryTimer); this.now.set(Date.now()); this.retryUntil.set(Date.now() + Number(wait) * 1000);
          this.retryTimer = setInterval(() => { this.now.set(Date.now()); if (!this.retryWait()) clearInterval(this.retryTimer); }, 1000);
        }
        const code = error?.error?.error?.code;
        if (error?.status === 409 && ['REVIEW_CONTEXT_CHANGED', 'REVIEW_BASE_OUTDATED', 'UI_IMPACT_UNDECIDED'].includes(code)
          || error?.status === 400 && code === 'INVALID_OFFICIAL_REVIEW') {
          this.pending.set(null); this.error.set('ההחלטה לא נשמרה: יש לבדוק את הגרסה, עדכון הבסיס והשפעת הממשק. הטקסט נשאר בעמוד.');
        } else this.error.set(error?.status === 429 ? 'נדרשת המתנה לפני ניסיון נוסף. הבקשה המקורית נשמרת בעמוד.'
          : 'תוצאת השליחה טרם אומתה. אפשר לטעון היסטוריה או לבדוק שוב את אותה בקשה.');
      },
    });
  }
  private denied(error: { status?: number; error?: { error?: { code?: string } } }): boolean {
    return [401, 403, 404].includes(error?.status ?? 0) || error?.error?.error?.code === 'SESSION_IDENTITY_CHANGED';
  }
  area(value: string): string { return ({ PRODUCT: 'מוצר', DEVELOPMENT: 'פיתוח', DESIGN: 'עיצוב' } as Record<string, string>)[value] ?? value; }
  label(value: string): string { return ({ APPROVE_ALL: 'אישור כל שינויי הגרסה', REQUEST_CHANGES: 'בקשת תיקונים', COMMENT: 'הערת סקירה' } as Record<string, string>)[value] ?? value; }
  private clear(): void {
    ++this.generation; this.reads?.unsubscribe(); this.writes?.unsubscribe(); this.data.set(null); this.pending.set(null);
    clearInterval(this.retryTimer); this.retryUntil.set(0);
    this.busy.set(false); this.sending.set(false); this.error.set(''); this.message.set(''); this.explanation.set('');
    this.domain.set('PRODUCT'); this.decision.set('COMMENT');
  }
  ngOnDestroy(): void { this.clear(); }
}
