import { computed, effect, inject, Injectable, InjectionToken, OnDestroy, signal, untracked } from '@angular/core';
import { Subscription, switchMap, timeout } from 'rxjs';
import { ReviewApi, type ReviewSourceWindow, type ReviewVisitRequest, type ReviewVisitRecorded } from '../../core/api/review-api';
import { SessionState } from '../../core/auth/session-state';
import { freezeSource, sourceScopeKey, validSourceWindow, validVisitReceipt, type ReviewSourceScope, type SourceDigest } from './review-source-evidence';

export const REVIEW_SOURCE_DIGEST = new InjectionToken<SourceDigest>('review source SHA-256', {
  providedIn: 'root', factory: () => async text => {
    const value = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(text));
    return Array.from(new Uint8Array(value), b => b.toString(16).padStart(2, '0')).join('');
  },
});
type Attempt = { scope: ReviewSourceScope; window: ReviewSourceWindow; body: Readonly<ReviewVisitRequest> };
@Injectable()
export class ReviewSourceState implements OnDestroy {
  private readonly api = inject(ReviewApi);
  private readonly session = inject(SessionState);
  private readonly hash = inject(REVIEW_SOURCE_DIGEST);
  private reads?: Subscription;
  private writes?: Subscription;
  private timer?: ReturnType<typeof setInterval>;
  private generation = 0;
  private epoch = -1;
  private live: () => boolean = () => false;
  private offsets = [0];
  private target = 0;
  readonly scope = signal<ReviewSourceScope | null>(null);
  private readonly data = signal<ReviewSourceWindow | null>(null);
  readonly pending = signal<Attempt | null>(null);
  readonly receipt = signal<ReviewVisitRecorded | null>(null);
  readonly busy = signal(false);
  readonly sending = signal(false);
  readonly error = signal('');
  readonly denied = signal(false);
  private readonly blocked = signal(false);
  private readonly serverExpired = signal(false);
  private readonly now = signal(Date.now());
  private readonly retryUntil = signal(0);
  readonly retryWait = computed(() => Math.max(0, Math.ceil((this.retryUntil() - this.now()) / 1000)));
  readonly hasPending = computed(() => !!this.pending() || this.sending());
  readonly view = computed(() => this.owns() ? this.data() : null);
  readonly expired = computed(() => this.serverExpired() || !!this.view() && Date.parse(this.view()!.expires_at) <= this.now());
  readonly canAcknowledge = computed(() => !!this.view() && !this.busy() && !this.hasPending() && !this.receipt() && !this.expired() && !this.blocked() && !this.retryWait());
  readonly canBack = computed(() => !!this.view() && this.offsets.length > 1);
  constructor() {
    effect(() => {
      const scope = this.scope(), actor = this.session.profile()?.user_id, epoch = this.session.logoutEpoch(), live = scope ? this.live() : true;
      if (scope && (!live || actor !== scope.actor || epoch !== this.epoch)) untracked(() => this.clear());
    });
  }
  open(scope: ReviewSourceScope, live: () => boolean): void {
    if (!sourceScopeKey(scope) || scope.actor !== this.session.profile()?.user_id || !live() || this.hasPending()) return;
    this.clear(); this.live = live; this.epoch = this.session.logoutEpoch(); this.scope.set(freezeSource(structuredClone(scope)));
    this.timer = setInterval(() => this.now.set(Date.now()), 1000); this.read(0);
  }
  private owns(): boolean {
    const scope = this.scope(); return !!scope && !this.denied() && this.live() && scope.actor === this.session.profile()?.user_id && this.epoch === this.session.logoutEpoch();
  }
  private read(offset: number): void {
    const scope = this.scope(); if (!scope || !this.owns() || this.hasPending() || this.retryWait()) return;
    this.reads?.unsubscribe(); const generation = ++this.generation; this.target = offset;
    this.data.set(null); this.receipt.set(null); this.error.set(''); this.busy.set(true); this.serverExpired.set(false);
    this.reads = this.api.source(scope.actor, scope.proposal, scope.snapshot, { expected_fingerprint: scope.fingerprint, unit_id: scope.unit.id,
      domain: scope.domain, side: scope.side, offset }).pipe(switchMap(async value => {
        const window = freezeSource(structuredClone(value));
        return { window, valid: this.owns() && generation === this.generation && await validSourceWindow(window, scope, offset, this.hash) };
      }), timeout(20_000)).subscribe({
        next: ({ window, valid }) => {
          if (!this.owns() || generation !== this.generation) return;
          this.busy.set(false);
          if (!valid) { this.error.set('המקור שהתקבל אינו תואם לפריט ולגרסה שנבחרו. אין אפשרות לתעד קריאה.'); return; }
          this.blocked.set(false); this.now.set(Date.now()); this.data.set(window);
        },
        error: error => {
          if (!this.owns() || generation !== this.generation) return;
          this.busy.set(false); if (this.reject(error)) return; this.wait(error);
          if (error?.status === 409) this.blocked.set(true);
          this.error.set(error?.status === 409 ? 'הקשר הסקירה השתנה. יש לבדוק את הגרסה העדכנית.' : 'לא ניתן לטעון את המקור כרגע. ניתן לנסות שוב במפורש.');
        },
      });
  }
  refresh(): void { if (!this.busy()) this.read(this.target); }
  next(): void {
    const next = this.view()?.next_offset;
    if (next == null || this.busy() || this.hasPending() || this.retryWait()) return;
    this.offsets.push(next); this.read(next);
  }
  back(): void {
    if (!this.canBack() || this.busy() || this.hasPending() || this.retryWait()) return;
    this.offsets.pop(); this.read(this.offsets.at(-1)!);
  }
  acknowledge(): void {
    const scope = this.scope(), window = this.view(); if (!scope || !window || !this.canAcknowledge()) return;
    let nonce: string;
    try { nonce = crypto.randomUUID(); } catch { this.error.set('לא ניתן להכין בקשת תיעוד בטוחה כרגע.'); return; }
    const body = Object.freeze({ request_id: nonce, delivery_id: window.delivery_id, expected_fingerprint: scope.fingerprint, domain: scope.domain });
    this.pending.set({ scope, window, body }); this.retry();
  }
  retry(): void {
    const attempt = this.pending(); if (!attempt || !this.owns() || this.sending() || this.busy() || this.retryWait()) return;
    const generation = ++this.generation, { scope, window, body } = attempt; this.sending.set(true); this.error.set('');
    this.writes = this.api.acknowledge(scope.actor, scope.proposal, scope.snapshot, body).pipe(timeout(20_000)).subscribe({
      next: value => {
        if (!this.owns() || generation !== this.generation) return;
        this.sending.set(false);
        if (!validVisitReceipt(value, scope, window, body)) { this.error.set('תוצאת התיעוד אינה תואמת לבקשה. יש לבדוק שוב את אותה בקשה.'); return; }
        this.pending.set(null); this.receipt.set(freezeSource(structuredClone(value)));
      },
      error: error => {
        if (!this.owns() || generation !== this.generation) return;
        this.sending.set(false); if (this.reject(error)) return; this.wait(error);
        if ([400, 409, 410].includes(error?.status)) {
          this.pending.set(null); if (error.status === 410) this.serverExpired.set(true); if (error.status === 409) this.blocked.set(true);
          this.error.set(error.status === 410 ? 'תוקף מסירת המקור פג. יש לטעון את המקטע מחדש לפני תיעוד קריאה.' : 'פרטי התיעוד או הקשר הסקירה השתנו. יש לבדוק את הגרסה.'); return;
        }
        this.error.set('תוצאת התיעוד טרם אומתה. ניתן לבדוק שוב את אותה בקשה; אין צורך לשלוח בקשה חדשה.');
      },
    });
  }
  private wait(error: { status?: number; headers?: { get(name: string): string | null } }): void {
    const value = error?.headers?.get('Retry-After') ?? '';
    const seconds = /^[1-9]\d{0,9}$/.test(value) && Number(value) <= 4294967295 ? Number(value) : error?.status === 429 ? 60 : 0;
    this.now.set(Date.now()); this.retryUntil.set(Date.now() + seconds * 1000);
  }
  private reject(error: { status?: number }): boolean {
    if (![401, 403, 404].includes(error?.status ?? 0)) return false;
    this.clear(); this.denied.set(true); return true;
  }
  clear(): void {
    ++this.generation; this.reads?.unsubscribe(); this.writes?.unsubscribe(); clearInterval(this.timer); this.timer = undefined;
    this.scope.set(null); this.data.set(null); this.pending.set(null); this.receipt.set(null); this.busy.set(false); this.sending.set(false);
    this.error.set(''); this.denied.set(false); this.blocked.set(false); this.serverExpired.set(false); this.retryUntil.set(0);
    this.now.set(Date.now()); this.offsets = [0]; this.target = 0; this.live = () => false;
  }
  ngOnDestroy(): void { this.clear(); }
}
