import { computed, effect, inject, Injectable, OnDestroy, signal, untracked } from '@angular/core';
import { Subscription, timeout } from 'rxjs';
import { ReviewApi, type ReviewCoverage, type ReviewSnapshot, type ReviewUnits, type UnitApprovalPage,
  type UnitApprovalRequest, type UnitApprovalRecorded } from '../../core/api/review-api';
import { SessionState } from '../../core/auth/session-state';
import { reviewAreas } from './official-review-evidence';
import { freezeSource } from './review-source-evidence';
import { validReviewUnits, type ReviewUnit } from './review-units-evidence';
import { validReviewCoverage } from './review-coverage-evidence';
import { validUnitApprovalPage, validUnitApprovalReceipt } from './unit-approval-evidence';

export type ApprovalReadScope = { actor: string; proposal: string; snapshot: ReviewSnapshot; page: ReviewUnits; domain: UnitApprovalRequest['domain'] };
type Capture = { scope: ApprovalReadScope; key: string; epoch: number; live: () => boolean };
type Attempt = { capture: Capture; unit: ReviewUnit; body: Readonly<UnitApprovalRequest>; coverage: ReviewCoverage };
function root(scope: ApprovalReadScope): string {
  const s = scope.snapshot, m = s.metadata;
  return JSON.stringify([scope.actor, scope.proposal, s.snapshot_id, s.fingerprint, m.summary.head_sha, m.summary.target_sha,
    m.summary.policy_version, m.summary.configuration_version, m.summary.repository_id, m.summary.merge_base_sha,
    m.context.author_id, m.assignments, s.file_count]);
}
@Injectable()
export class UnitApprovalState implements OnDestroy {
  private readonly api = inject(ReviewApi);
  private readonly session = inject(SessionState);
  private reads?: Subscription;
  private writes?: Subscription;
  private timer?: ReturnType<typeof setInterval>;
  private readGeneration = 0;
  private writeGeneration = 0;
  private readonly capture = signal<Capture | null>(null);
  private readonly data = signal<{ capture: Capture; value: UnitApprovalPage } | null>(null);
  private readonly attempt = signal<Attempt | null>(null);
  private readonly outcome = signal<{ capture: Capture; unit: ReviewUnit; receipt: UnitApprovalRecorded } | null>(null);
  private readonly writeCapture = signal<Capture | null>(null);
  private readonly writeFailure = signal('');
  private readonly readFailure = signal('');
  private readonly rejectedCoverage = signal<ReviewCoverage | null>(null);
  private readonly now = signal(Date.now());
  private readonly retryUntil = signal(0);
  readonly busy = signal(false);
  readonly sending = signal(false);
  readonly denied = signal(false);
  readonly hasPending = computed(() => !!this.attempt() || this.sending());
  readonly pending = computed(() => { const a = this.attempt(); return a && this.owns(a.capture) ? a : null; });
  readonly result = computed(() => { const r = this.outcome(); return r && this.owns(r.capture) ? r : null; });
  readonly view = computed(() => {
    const data = this.data(); return data && data.capture === this.capture() && this.owns(data.capture) ? data.value : null;
  });
  readonly readError = computed(() => { const c = this.capture(); return c && this.owns(c) ? this.readFailure() : ''; });
  readonly writeError = computed(() => { const c = this.writeCapture(); return c && this.owns(c) ? this.writeFailure() : ''; });
  readonly retryWait = computed(() => Math.max(0, Math.ceil((this.retryUntil() - this.now()) / 1000)));
  constructor() {
    effect(() => {
      const captures = [this.capture(), this.attempt()?.capture, this.outcome()?.capture, this.writeCapture()];
      if (captures.some(c => c && !this.owns(c))) untracked(() => this.clear());
    });
  }
  private owns(c: Capture): boolean {
    return !this.denied() && c.scope.actor === this.session.profile()?.user_id && c.epoch === this.session.logoutEpoch() && c.live();
  }
  configure(scope: ApprovalReadScope | null, live: () => boolean): void {
    if (!scope) { this.clearRead(); return; }
    const { actor, proposal, snapshot, page, domain } = scope;
    if (this.denied() || actor !== this.session.profile()?.user_id || !live() || !reviewAreas.includes(domain)
      || snapshot.metadata.context.author_id === actor || !snapshot.metadata.assignments.some(a => a.reviewer_id === actor && a.domain === domain)
      || !validReviewUnits(page, actor, proposal, snapshot, page.offset)) { this.clearRead(); return; }
    const pending = this.attempt();
    if (pending && !this.owns(pending.capture)) this.clear();
    else if (pending && (root(pending.capture.scope) !== root(scope) || pending.body.domain !== domain)) { this.clearRead(); return; }
    const epoch = this.session.logoutEpoch(), key = JSON.stringify([root(scope), domain, page, snapshot.current_metadata, epoch]);
    if (this.capture()?.key === key) return;
    const previous = this.writeCapture();
    if (!this.hasPending() && previous && (root(previous.scope) !== root(scope) || previous.scope.domain !== domain)) this.clearWrite();
    this.clearRead();
    const capture = Object.freeze({ scope: freezeSource(structuredClone(scope)), key, epoch, live });
    this.capture.set(capture); this.load(capture);
  }
  refresh(): void { const c = this.capture(); if (c && this.owns(c) && !this.busy()) this.load(c); }
  private clearRead(): void {
    ++this.readGeneration; this.reads?.unsubscribe(); this.reads = undefined;
    this.capture.set(null); this.data.set(null); this.busy.set(false); this.readFailure.set('');
  }
  private load(c: Capture): void {
    if (!this.owns(c) || this.capture() !== c) return;
    this.reads?.unsubscribe(); const generation = ++this.readGeneration, s = c.scope;
    this.data.set(null); this.readFailure.set(''); this.busy.set(true);
    this.reads = this.api.unitApprovals(s.actor, s.proposal, s.snapshot.snapshot_id, { expected_fingerprint: s.snapshot.fingerprint,
      domain: s.domain, offset: s.page.offset, limit: 20 }).pipe(timeout(20_000)).subscribe({
        next: value => {
          if (generation !== this.readGeneration || this.capture() !== c || !this.owns(c)) return;
          this.busy.set(false);
          const confirmed = this.outcome(), same = confirmed && root(confirmed.capture.scope) === root(s) && confirmed.receipt.domain === s.domain;
          if (!validUnitApprovalPage(value, s.actor, s.proposal, s.snapshot, s.page, s.domain)
            || same && (value.approved_units < 1 || value.units.some(u => u.unit_id === confirmed.unit.id && u.approval_id == null))) {
            this.readFailure.set('נתוני האישורים אינם תואמים לפריטים, לגרסה או לתחום המוצגים.'); return;
          }
          this.data.set({ capture: c, value: freezeSource(structuredClone(value)) });
        },
        error: error => {
          if (generation !== this.readGeneration || this.capture() !== c || !this.owns(c)) return;
          this.busy.set(false); if (this.reject(error)) return;
          this.readFailure.set('לא ניתן לבדוק אישורי פריטים כרגע. אפשר לנסות שוב במפורש.');
        },
      });
  }
  canApprove(id: string, coverage: ReviewCoverage | null | undefined): boolean {
    const c = this.capture(), value = this.view();
    if (!c || !value || !coverage || !this.owns(c) || this.hasPending() || this.busy() || this.retryWait()
      || coverage === this.rejectedCoverage() || !value.available || !value.current_for_metadata || !c.scope.snapshot.current_metadata) return false;
    const s = c.scope, item = value.units.find(u => u.unit_id === id);
    return !!item && item.approval_id == null && validReviewCoverage(coverage, s.actor, s.proposal, s.snapshot, s.page, s.domain)
      && coverage.current_for_metadata && !!coverage.units.find(u => u.unit_id === id)?.viewed;
  }
  approve(id: string, coverage: ReviewCoverage | null | undefined): void {
    if (!this.canApprove(id, coverage)) return;
    const capture = this.capture()!, s = capture.scope, unit = s.page.units.find(u => u.id === id)!;
    this.writeCapture.set(capture); this.writeFailure.set(''); this.outcome.set(null);
    let nonce: string;
    try { nonce = crypto.randomUUID(); } catch { this.writeFailure.set('לא ניתן להכין בקשת אישור בטוחה כרגע.'); return; }
    const body = Object.freeze({ request_id: nonce, expected_fingerprint: s.snapshot.fingerprint, domain: s.domain, unit_id: unit.id });
    this.attempt.set(Object.freeze({ capture, unit, body, coverage: coverage! })); this.retry();
  }
  retry(): void {
    const a = this.attempt(); if (!a || !this.owns(a.capture) || this.sending() || this.retryWait()) return;
    const generation = ++this.writeGeneration, s = a.capture.scope;
    this.sending.set(true); this.writeFailure.set('');
    this.writes = this.api.approveUnit(s.actor, s.proposal, s.snapshot.snapshot_id, a.body).pipe(timeout(20_000)).subscribe({
      next: value => {
        if (generation !== this.writeGeneration || !this.owns(a.capture)) return;
        this.sending.set(false);
        if (!validUnitApprovalReceipt(value, s.actor, s.proposal, s.snapshot, a.body)) {
          this.writeFailure.set('תוצאת האישור אינה תואמת לבקשה. יש לבדוק שוב את אותה בקשה.'); return;
        }
        this.attempt.set(null); this.outcome.set(Object.freeze({ capture: a.capture, unit: a.unit, receipt: freezeSource(structuredClone(value)) }));
        const current = this.capture(); if (current && this.owns(current)) this.load(current);
      },
      error: error => {
        if (generation !== this.writeGeneration || !this.owns(a.capture)) return;
        this.sending.set(false); if (this.reject(error)) return; this.wait(error);
        const code = error?.error?.error?.code;
        if ([400, 413].includes(error?.status) && code === 'INVALID_UNIT_APPROVAL'
          || error?.status === 409 && ['REVIEW_CONTEXT_CHANGED', 'REVIEW_UNIT_UNVIEWED', 'REVIEW_UNIT_EVIDENCE_UNAVAILABLE', 'UNIT_APPROVAL_CONFLICT'].includes(code)) {
          this.attempt.set(null); this.rejectedCoverage.set(a.coverage);
          this.writeFailure.set('האישור לא נשמר. יש לבדוק מחדש את הגרסה ואת מצב הקריאה לפני אישור נוסף.'); return;
        }
        this.writeFailure.set('תוצאת האישור טרם אומתה. יש לבדוק שוב את אותה בקשה, בלי ליצור אישור חדש.');
      },
    });
  }
  private wait(error: { status?: number; headers?: { get(name: string): string | null } }): void {
    clearInterval(this.timer); this.timer = undefined;
    const value = error?.headers?.get('Retry-After') ?? '';
    const seconds = /^[1-9]\d{0,9}$/.test(value) && Number(value) <= 4294967295 ? Number(value) : error?.status === 429 ? 600 : 0;
    const now = Date.now(); this.now.set(now); this.retryUntil.set(now + seconds * 1000);
    if (seconds) this.timer = setInterval(() => {
      this.now.set(Date.now()); if (!this.retryWait()) { clearInterval(this.timer); this.timer = undefined; }
    }, 1000);
  }
  private reject(error: { status?: number; error?: { error?: { code?: string } } }): boolean {
    if (![401, 403, 404].includes(error?.status ?? 0) && error?.error?.error?.code !== 'SESSION_IDENTITY_CHANGED') return false;
    this.clear(); this.denied.set(true); return true;
  }
  private clearWrite(): void {
    ++this.writeGeneration; this.writes?.unsubscribe(); this.writes = undefined; clearInterval(this.timer); this.timer = undefined;
    this.attempt.set(null); this.outcome.set(null); this.writeCapture.set(null); this.writeFailure.set(''); this.sending.set(false);
    this.rejectedCoverage.set(null); this.retryUntil.set(0); this.now.set(Date.now());
  }
  clear(): void { this.clearRead(); this.clearWrite(); this.denied.set(false); }
  ngOnDestroy(): void { this.clear(); }
}
