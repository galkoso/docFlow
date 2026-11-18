import { ChangeDetectionStrategy, Component, computed, effect, inject, input, OnDestroy, output, signal, untracked } from '@angular/core';
import { DatePipe, DOCUMENT } from '@angular/common';
import { Subscription, timeout } from 'rxjs';
import { ReviewApi, type ReviewPrStatus, type ReviewSnapshot } from '../../core/api/review-api';
import { SessionState } from '../../core/auth/session-state';
import { validPrStatus } from './pr-status-evidence';

@Component({
  selector: 'app-review-pr-status', imports: [DatePipe], changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: './review-pr-status.html', styleUrl: './review-pr-status.css',
})
export class ReviewPrStatusView implements OnDestroy {
  readonly actor = input.required<string>();
  readonly proposal = input.required<string>();
  readonly snapshot = input.required<ReviewSnapshot>();
  readonly accessDenied = output<void>();
  private readonly api = inject(ReviewApi);
  private readonly session = inject(SessionState);
  private readonly document = inject(DOCUMENT);
  private readonly data = signal<ReviewPrStatus | null>(null);
  readonly busy = signal(false);
  readonly error = signal('');
  private generation = 0;
  private owner = '';
  private epoch = -1;
  private request?: Subscription;
  private timer?: ReturnType<typeof setTimeout>;
  readonly view = computed(() => {
    const value = this.data(), snapshot = this.snapshot();
    return this.actor() === this.owner && this.session.profile()?.user_id === this.owner && this.session.logoutEpoch() === this.epoch
      && value?.proposal_id === this.proposal() && value.snapshot_id === snapshot.snapshot_id && value.fingerprint === snapshot.fingerprint ? value : null;
  });
  constructor() {
    effect(() => {
      const actor = this.actor(), proposal = this.proposal(), snapshot = this.snapshot(), live = this.session.profile()?.user_id, epoch = this.session.logoutEpoch();
      untracked(() => {
        this.clear(); this.owner = actor; this.epoch = epoch;
        if (live === actor) this.load(actor, proposal, snapshot);
      });
    });
  }
  refresh(): void {
    if (!this.busy() && this.actor() === this.owner && this.session.profile()?.user_id === this.owner && this.session.logoutEpoch() === this.epoch) {
      this.load(this.actor(), this.proposal(), this.snapshot());
    }
  }
  private load(actor: string, proposal: string, snapshot: ReviewSnapshot): void {
    clearTimeout(this.timer); this.request?.unsubscribe();
    const generation = ++this.generation, epoch = this.epoch;
    const current = () => generation === this.generation && this.session.profile()?.user_id === actor && this.session.logoutEpoch() === epoch
      && this.actor() === actor && this.proposal() === proposal && this.snapshot() === snapshot;
    this.busy.set(true); this.error.set(''); this.data.set(null);
    this.request = this.api.prStatus(actor, proposal, snapshot.snapshot_id).pipe(timeout(20000)).subscribe({
      next: value => {
        if (!current()) return;
        this.busy.set(false);
        if (!validPrStatus(value, proposal, snapshot)) { this.error.set('פרטי מצב הסנכרון אינם תואמים לגרסה. יש לבדוק שוב.'); return; }
        this.data.set(value);
        if (value.sync?.automatic || ['AWAITING', 'WAITING'].includes(value.dispatch.phase)) {
          this.timer = setTimeout(() => { if (current()) this.load(actor, proposal, snapshot); }, this.document.hidden ? 10000 : 2000);
        }
      },
      error: error => {
        if (!current()) return;
        this.busy.set(false); this.data.set(null);
        if ([401, 403, 404, 409].includes(error?.status)) { this.accessDenied.emit(); return; }
        this.error.set('לא ניתן לטעון את מצב הסנכרון כרגע. גרסת הסקירה השמורה עדיין זמינה.');
      },
    });
  }
  phase(value: ReviewPrStatus): string {
    const sync = value.sync?.phase;
    if (sync) return ({ QUEUED: 'עבודת ה־PR בתור', OBSERVING: 'עבודת ה־PR בטיפול', RETRY_WAIT: 'ממתינים לניסיון נוסף',
      RECONCILING: 'תוצאת פעולת ה־PR עדיין בבירור', OBSERVED_SYNCED: 'נשמר תיעוד התאמה לגרסה זו',
      SUPERSEDED: 'העבודה מתייחסת למצב קודם', FAILED: 'עבודת ה־PR לא הושלמה' })[sync];
    return ({ AWAITING: 'גרסת הסקירה נשמרה; ממתינים לקליטת עבודת ה־PR', WAITING: 'קליטת עבודת ה־PR ממתינה',
      ENQUEUED: 'עבודת ה־PR נקלטה', SKIPPED: 'לא נפתחה עבודה לגרסה שכבר אינה נוכחית', REJECTED: 'קליטת העבודה דורשת בדיקה',
      UNTRACKED: 'אין תיעוד קליטה לעבודה זו' })[value.dispatch.phase];
  }
  reason(value: ReviewPrStatus): string {
    const reason = value.sync?.reason ?? value.dispatch.reason;
    if (!reason) return '';
    return ({ BUSY: 'עבודה קודמת עדיין פעילה.', QUOTA: 'ממתינים למכסת עבודה פנויה.', AUTHORITY_REQUIRED: 'נדרשת בדיקת הרשאות העורך המקורי.',
      SETUP_REQUIRED: 'נדרשת בדיקת חיבור המאגר.', SOURCE_CHANGED: 'פרטי ההצעה או הענפים השתנו.', BINDING_CHANGED: 'חיבור המאגר השתנה.',
      INVALID_PUBLICATION: 'נדרשת בדיקת תיעוד גרסת הסקירה.', INVALID_PROJECTION: 'פרטי ההצעה אינם מתאימים לעדכון ה־PR.',
      PROVIDER_UNAVAILABLE: 'ספק Git אינו זמין כרגע.', PROVIDER_RATE_LIMIT: 'ממתינים למכסת ספק Git.', PROVIDER_REJECTED: 'ספק Git דחה את הבקשה.',
      CONFLICT: 'נמצאה אי־התאמה בזיהוי ה־PR.', UNCERTAIN_RESULT: 'תוצאת הפעולה טרם הוכרעה.',
      OBSERVATION_PENDING: 'ממתינים לאימות תוצאת הספק.', OTHER: 'נדרשת בדיקה נוספת של העבודה.' })[reason];
  }
  private clear(): void {
    ++this.generation; clearTimeout(this.timer); this.request?.unsubscribe(); this.data.set(null); this.busy.set(false); this.error.set('');
  }
  ngOnDestroy(): void { this.clear(); }
}
