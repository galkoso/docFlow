import { ChangeDetectionStrategy, Component, computed, effect, HostListener, inject, OnDestroy, signal, untracked } from '@angular/core';
import { DatePipe, DOCUMENT } from '@angular/common';
import { ActivatedRoute, RouterLink, type ParamMap } from '@angular/router';
import { toSignal } from '@angular/core/rxjs-interop';
import { forkJoin, of, Subscription, timeout } from 'rxjs';
import { CheckOperationsApi, type CheckOperation, type CheckOperationPage, type CheckRecoveryRequest, } from '../../core/api/check-operations-api';
import { SessionState } from '../../core/auth/session-state';
import { uuid } from '../comparison/comparison-evidence';
import { validCheckOperation, validCheckPage, validCheckRecovery } from './check-operation-evidence';

type Location = { after?: string; operation?: string };
type Attempt = {
  actor: string; proposal: string; organization: string; epoch: number;
  source: Readonly<CheckOperation>; body: Readonly<CheckRecoveryRequest>;
};
@Component({
  selector: 'app-check-operations', imports: [RouterLink, DatePipe], changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: './check-operations-page.html', styleUrl: './check-operations-page.css',
})
export class CheckOperationsPage implements OnDestroy {
  readonly session = inject(SessionState);
  private readonly api = inject(CheckOperationsApi);
  private readonly document = inject(DOCUMENT);
  private readonly route = inject(ActivatedRoute);
  private readonly params = toSignal(this.route.paramMap, { initialValue: this.route.snapshot.paramMap });
  private readonly query = toSignal(this.route.queryParamMap, { initialValue: this.route.snapshot.queryParamMap });
  readonly proposal = computed(() => this.params().get('id'));
  readonly location = computed(() => this.parseLocation(this.query()));
  readonly busy = signal(false);
  readonly recovering = signal(false);
  readonly error = signal('');
  readonly message = signal('');
  readonly denied = signal(false);
  readonly retryAt = signal(0);
  private readonly organization = signal<string | null>(null);
  private readonly data = signal<CheckOperationPage | null>(null);
  private readonly selected = signal<CheckOperation | null>(null);
  private readonly pending = signal<Attempt | null>(null);
  readonly page = computed(() => { const v = this.data(); return v && this.current() ? v : null; });
  readonly view = computed(() => { const v = this.selected(); return v && this.current() ? v : null; });
  readonly recoveryPending = computed(() => { const v = this.pending(); return !!v && this.current(); });
  private context = '';
  private owner = '';
  private epoch = -1;
  private generation = 0;
  private readGeneration = 0;
  private request?: Subscription;
  private recoveryRequest?: Subscription;
  private backoff?: ReturnType<typeof setTimeout>;
  private readonly releaseLogout = this.session.guardLogout(() => this.canLeave());
  constructor() {
    effect(() => {
      const profile = this.session.profile(), proposal = this.proposal(), location = this.location(), epoch = this.session.logoutEpoch();
      untracked(() => {
        if (!profile || !uuid(profile.user_id) || !uuid(proposal) || !location) {
          this.clear(); this.context = '';
          if (profile && (!uuid(profile.user_id) || !uuid(proposal) || !location)) this.error.set('כתובת בדיקות הסקירה או פרטי ההתחברות אינם תקינים.');
          return;
        }
        const key = this.key(profile.user_id, proposal, location, epoch);
        if (!this.admin() || key === this.context && this.organization() && !this.admin(this.organization()!)) {
          this.block(); return;
        }
        if (key !== this.context) {
          this.clear(); this.owner = profile.user_id; this.epoch = epoch; this.context = key; this.load();
        }
      });
    });
  }
  private parseLocation(query: ParamMap): Location | null {
    if (query.keys.some(key => !['after', 'operation'].includes(key) || query.getAll(key).length !== 1)) return null;
    const after = query.get('after'), operation = query.get('operation');
    return after != null && !uuid(after) || operation != null && !uuid(operation) ? null : { after: after ?? undefined, operation: operation ?? undefined };
  }
  private key(actor: string, proposal: string, location: Location, epoch: number): string {
    return JSON.stringify([actor, proposal, epoch, location.after ?? null, location.operation ?? null]);
  }
  private admin(org?: string): boolean {
    const memberships = this.session.profile()?.memberships;
    return Array.isArray(memberships) && memberships.some(m => !!m && m.role === 'ADMIN' && (org == null || m.organization_id === org));
  }
  private current(): boolean {
    const proposal = this.proposal(), location = this.location(), org = this.organization();
    return !this.denied() && this.session.profile()?.user_id === this.owner && this.session.logoutEpoch() === this.epoch
      && !!proposal && !!location && this.context === this.key(this.owner, proposal, location, this.epoch)
      && this.admin(org ?? undefined);
  }
  reload(): void {
    if (this.recovering() || this.busy() || !this.session.profile() || !uuid(this.proposal()) || !this.location()) return;
    if (!this.admin()) { this.block(); return; }
    if (this.denied()) { this.clear(); this.context = ''; this.owner = this.session.profile()!.user_id; this.epoch = this.session.logoutEpoch();
      this.context = this.key(this.owner, this.proposal()!, this.location()!, this.epoch); }
    if (this.current()) this.load();
  }
  private load(): void {
    if (!this.current()) return;
    this.request?.unsubscribe(); const token = ++this.readGeneration, generation = this.generation;
    const actor = this.owner, proposal = this.proposal()!, location = this.location()!;
    const current = () => token === this.readGeneration && generation === this.generation && this.current();
    this.busy.set(true); this.error.set(''); this.data.set(null); this.selected.set(null);
    this.request = forkJoin({ page: this.api.page(actor, proposal, location.after),
      operation: location.operation ? this.api.read(actor, proposal, location.operation) : of(null),
    }).pipe(timeout(20000)).subscribe({
      next: ({ page, operation }) => {
        if (!current()) return;
        this.busy.set(false);
        if (!validCheckPage(page, actor, proposal, location.after) || !this.admin(page.organization_id)
          || this.organization() != null && page.organization_id !== this.organization()
          || operation != null && !validCheckOperation(operation, actor, proposal, location.operation!, page.organization_id)) {
          this.error.set('לא ניתן לאמת את פרטי הבדיקות עבור ההצעה והחשבון הנוכחיים.'); return;
        }
        this.organization.set(page.organization_id); this.data.set(page); this.selected.set(operation);
      },
      error: error => {
        if (!current()) return; this.busy.set(false);
        if (this.accessFailure(error)) { this.block(); return; }
        this.error.set('לא ניתן לטעון את מצב הבדיקות כרגע. אפשר לנסות לעדכן את המצב.');
      },
    });
  }
  recover(): void {
    if (!this.current() || this.busy() || this.recovering() || Date.now() < this.retryAt()) return;
    let attempt = this.pending();
    if (!attempt) {
      const source = this.view();
      if (!source || source.job_state !== 'FAILED' || source.published || source.max_attempts > 2147483639) return;
      attempt = { actor: this.owner, proposal: this.proposal()!, organization: source.organization_id, epoch: this.epoch,
        source: Object.freeze({ ...source }), body: Object.freeze({ request_id: crypto.randomUUID(), expected_request_hash: source.request_hash }) };
    }
    if (attempt.actor !== this.owner || attempt.proposal !== this.proposal() || attempt.epoch !== this.epoch || !this.admin(attempt.organization)) return;
    this.pending.set(attempt); this.recovering.set(true); this.error.set(''); this.message.set('');
    this.request?.unsubscribe(); ++this.readGeneration; this.data.set(null); this.selected.set(null);
    const generation = this.generation, captured = attempt;
    const current = () => generation === this.generation && this.current();
    this.recoveryRequest = this.api.recover(attempt.actor, attempt.proposal, attempt.source.operation_id, attempt.body).pipe(timeout(20000)).subscribe({
      next: value => {
        if (!current()) return; this.recovering.set(false);
        if (!validCheckRecovery(value, captured.proposal, captured.source, captured.body)) {
          this.error.set('לא ניתן לאמת את קבלת הבקשה. אפשר לבדוק שוב את אותה בקשה.'); return;
        }
        this.pending.set(null); this.message.set('בקשת ההתאוששות נקלטה. תוצאת העבודה נבדקת בנפרד.'); this.load();
      },
      error: error => {
        if (!current()) return; this.recovering.set(false);
        if (this.accessFailure(error)) { this.block(); return; }
        if (error?.status === 429) {
          const header = error.headers?.get('Retry-After'); const seconds = typeof header === 'string' && /^[1-9]\d{0,9}$/.test(header) ? Number(header) : 600;
          this.wait(Date.now() + Math.min(seconds, 4294967295) * 1000);
          this.error.set('יש להמתין לפני ניסיון נוסף. הבקשה המקורית נשמרת בעמוד זה.');
        } else this.error.set('תוצאת הבקשה טרם אומתה. אפשר לעדכן מצב או לבדוק שוב את אותה בקשה.');
      },
    });
  }
  private wait(until: number): void {
    clearTimeout(this.backoff); this.retryAt.set(until);
    this.backoff = setTimeout(() => {
      if (!this.current()) return;
      if (Date.now() >= until) this.retryAt.set(0); else this.wait(until);
    }, Math.min(Math.max(0, until - Date.now()), 2147483647));
  }
  private accessFailure(error: { status?: number; error?: { error?: { code?: string } } }): boolean {
    return [401, 403, 404].includes(error?.status ?? 0) || error?.status === 409 && error?.error?.error?.code === 'SESSION_IDENTITY_CHANGED';
  }
  private block(): void { this.clear(); this.denied.set(true); this.error.set('אין גישה למצב הבדיקות. יש לבדוק את ההתחברות וההרשאות לפני טעינה מחדש.'); }
  canLeave(): boolean {
    if (!this.current()) return true;
    return !this.recoveryPending() && !this.recovering() || this.document.defaultView?.confirm('תוצאת בקשת ההתאוששות טרם אומתה. לעזוב ולבדוק את מצב העבודה מאוחר יותר?') === true;
  }
  @HostListener('window:beforeunload', ['$event']) beforeUnload(event: BeforeUnloadEvent): void {
    if (this.current() && (this.recoveryPending() || this.recovering())) { event.preventDefault(); event.returnValue = ''; }
  }
  label(value: CheckOperation): string {
    if (value.published) return 'נרשמה תוצאת שליחה';
    if (value.operation_state === 'UNKNOWN') return value.job_state === 'FAILED' ? 'בדיקת התוצאה נעצרה' : 'תוצאת השליחה בבירור';
    return ({ QUEUED: 'ממתין בתור', RUNNING: 'בטיפול', FAILED: 'העבודה נעצרה', CANCELLED: 'העבודה בוטלה', SUCCEEDED: 'הושלמה' })[value.job_state] ?? 'מצב לא ידוע';
  }
  private clear(): void {
    ++this.generation; ++this.readGeneration; this.request?.unsubscribe(); this.recoveryRequest?.unsubscribe(); clearTimeout(this.backoff);
    this.data.set(null); this.selected.set(null); this.pending.set(null); this.organization.set(null); this.busy.set(false); this.recovering.set(false);
    this.error.set(''); this.message.set(''); this.denied.set(false); this.retryAt.set(0); this.owner = ''; this.epoch = -1; this.context = '';
  }
  ngOnDestroy(): void { this.clear(); this.releaseLogout(); }
}
