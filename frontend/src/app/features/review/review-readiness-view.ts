import { ChangeDetectionStrategy, Component, computed, effect, inject, input, output, signal, untracked, OnDestroy } from '@angular/core';
import { DatePipe } from '@angular/common';
import { Subscription, timeout } from 'rxjs';
import { ReviewApi, type StoredReviewReadiness, type ReviewSnapshot } from '../../core/api/review-api';
import { SessionState } from '../../core/auth/session-state';
import { errorCode } from '../comparison/comparison-evidence';
import { utc } from './snapshot-evidence';

const gates = [
  'EVIDENCE', 'PROPOSAL_STATE', 'SNAPSHOT', 'APPROVALS', 'REQUIRED_REVIEWER', 'BASE_UPDATE', 'CONTENT_VALIDATION',
  'MEDIA_VALIDATION', 'SECTION_COVERAGE', 'BLOCKING_THREADS', 'BLOCKING_QUESTIONS',
];
export function validStoredReadiness(v: StoredReviewReadiness, actor: string, proposal: string, snapshot: ReviewSnapshot): boolean {
  const s = snapshot.metadata.summary;
  if (!v || v.actor_id !== actor || v.proposal_id !== proposal || v.snapshot_id !== snapshot.snapshot_id || v.fingerprint !== snapshot.fingerprint
    || v.head_sha !== s.head_sha || v.target_sha !== s.target_sha || v.policy_version !== s.policy_version || v.configuration_version !== s.configuration_version
    || !utc(v.read_at) || typeof v.current_for_metadata !== 'boolean' || v.provider_refs_verified !== false || !v.evaluation
    || !Array.isArray(v.evaluation.issues) || !v.evaluation.issues.length || v.evaluation.issues.length > 20) return false;
  const issues = v.evaluation.issues;
  return issues.every(i => !!i && gates.includes(i.gate) && ['FAILURE', 'PENDING'].includes(i.conclusion) && errorCode(i.code))
    && new Set(issues.map(i => `${i.gate}/${i.code}`)).size === issues.length
    && issues.some(i => i.gate === 'EVIDENCE' && i.code === 'PROVIDER_REFS_UNVERIFIED' && i.conclusion === 'PENDING')
    && (v.current_for_metadata || issues.some(i => i.gate === 'SNAPSHOT' && i.conclusion === 'FAILURE'))
    && v.evaluation.conclusion === (issues.some(i => i.conclusion === 'FAILURE') ? 'FAILURE' : 'PENDING');
}
@Component({
  selector: 'app-review-readiness', imports: [DatePipe], changeDetection: ChangeDetectionStrategy.OnPush,
  template: `<section aria-labelledby="readiness-title"><h3 id="readiness-title">מוכנות לפי המידע השמור</h3>
    <p>הבדיקה מציגה תנאים שטרם הושלמו. היא אינה אישור סקירה, בדיקת Git עדכנית או הרשאה למיזוג.</p>
    <button type="button" (click)="refresh()" [disabled]="busy()">{{ opened() ? 'רענון בדיקת המוכנות' : 'בדיקת המוכנות השמורה' }}</button>
    @if (busy()) {<p role="status">בודקים את המידע השמור…</p>}
    @if (error()) {<p role="alert">{{ error() }}</p>}
    @if (view(); as value) {<p>{{ value.current_for_metadata ? 'תואמת לפרטי הגרסה השמורים' : 'הגרסה אינה תואמת למצב השמור כעת' }} · {{ value.read_at | date:'dd/MM/yyyy HH:mm:ss' }}</p>
      <ul>@for (issue of value.evaluation.issues; track issue.gate + '/' + issue.code) {<li><strong>{{ label(issue.gate) }}</strong>: {{ reason(issue.code) }}</li>}</ul>}
    </section>`,
  styles: `:host{display:block;min-width:0;margin-block:1rem}section{border:1px solid #c7d2df;padding:1rem}button{font:inherit;min-height:2.75rem;max-width:100%;white-space:normal;padding:.5rem .75rem}button:focus-visible{outline:3px solid #1d4ed8;outline-offset:3px}li,p{overflow-wrap:anywhere}`,
})
export class ReviewReadinessView implements OnDestroy {
  readonly actor = input.required<string>();
  readonly proposal = input.required<string>();
  readonly snapshot = input.required<ReviewSnapshot>();
  readonly accessDenied = output<void>();
  private readonly api = inject(ReviewApi);
  private readonly session = inject(SessionState);
  private read?: Subscription;
  private generation = 0;
  private readonly data = signal<StoredReviewReadiness | null>(null);
  private readonly owner = signal('');
  readonly busy = signal(false);
  readonly opened = signal(false);
  readonly error = signal('');
  readonly view = computed(() => this.live() ? this.data() : null);
  private key(): string { return JSON.stringify([this.actor(), this.proposal(), this.snapshot(), this.session.logoutEpoch()]); }
  private live(): boolean { return this.owner() === this.key() && this.actor() === this.session.profile()?.user_id; }
  constructor() {
    effect(() => { const key = this.key(), actor = this.session.profile()?.user_id;
      untracked(() => { if (key !== this.owner() || actor !== this.actor()) { this.clear(); if (actor === this.actor()) this.owner.set(key); } });
    });
  }
  refreshIfLoaded(): void { if (this.opened() && this.live()) this.refresh(); }
  refresh(): void {
    if (!this.live()) return;
    const key = this.owner(), actor = this.actor(), proposal = this.proposal(), snapshot = this.snapshot(), generation = ++this.generation;
    this.read?.unsubscribe(); this.data.set(null); this.busy.set(true); this.opened.set(true); this.error.set('');
    this.read = this.api.readiness(actor, proposal, snapshot.snapshot_id).pipe(timeout(20000)).subscribe({
      next: value => { if (generation !== this.generation || key !== this.owner() || !this.live()) return; this.busy.set(false);
        if (!validStoredReadiness(value, actor, proposal, snapshot)) { this.error.set('תוצאת בדיקת המוכנות אינה תואמת לגרסה שנבחרה.'); return; } this.data.set(value); },
      error: e => { if (generation !== this.generation || !this.live()) return; this.busy.set(false);
        if ([401, 403, 404].includes(e?.status) || e?.error?.error?.code === 'SESSION_IDENTITY_CHANGED') { this.clear(); this.accessDenied.emit(); return; }
        this.error.set('לא ניתן לבדוק מוכנות כרגע. יש לרענן לפני הסתמכות על הנתונים.'); },
    });
  }
  label(gate: string): string {
    return ({ EVIDENCE: 'אימות מקורות', PROPOSAL_STATE: 'מצב ההצעה', SNAPSHOT: 'גרסת הסקירה', APPROVALS: 'אישורי סוקרים', REQUIRED_REVIEWER: 'סוקרים נדרשים',
      BASE_UPDATE: 'עדכון בסיס', CONTENT_VALIDATION: 'בדיקת תוכן', MEDIA_VALIDATION: 'בדיקת תמונות', SECTION_COVERAGE: 'אישורי פריטים',
      BLOCKING_THREADS: 'דיונים חוסמים', BLOCKING_QUESTIONS: 'שאלות מוצר חוסמות' } as Record<string, string>)[gate] ?? gate;
  }
  reason(code: string): string {
    return ({ PROVIDER_REFS_UNVERIFIED: 'ענפי Git לא אומתו בקריאה זו. אין כאן אישור למיזוג.', POLICY_EVIDENCE_UNAVAILABLE: 'חסר מידע מאומת לתנאי זה.',
      POLICY_EVIDENCE_PENDING: 'התנאי טרם הושלם.', POLICY_GATE_FAILED: 'התנאי לא עבר את הבדיקה.', INDEPENDENT_APPROVALS_REQUIRED: 'נדרשים אישורי מוצר ופיתוח בלתי תלויים.',
      DESIGN_APPROVAL_REQUIRED: 'נדרש אישור עיצוב.', CHANGES_REQUESTED: 'יש בקשת שינויים פעילה.', REQUIRED_REVIEWER_APPROVAL_MISSING: 'חסר אישור של סוקר שנדרש.',
      APPROVAL_STALE: 'ההקשר שאושר אינו עדכני.', SNAPSHOT_NOT_CURRENT: 'גרסת הסקירה אינה הפעילה בהקשר הנוכחי.', UI_IMPACT_UNDECIDED: 'טרם הוכרעה השפעת הממשק.',
      PROPOSAL_NOT_REVIEWABLE: 'מצב ההצעה אינו מאפשר השלמת סקירה.', REVIEW_EVIDENCE_INVALID: 'יש לבדוק מחדש את ראיות הסקירה.' } as Record<string, string>)[code] ?? 'נדרש בירור נוסף לתנאי זה.';
  }
  private clear(): void { ++this.generation; this.read?.unsubscribe(); this.data.set(null); this.owner.set(''); this.busy.set(false); this.opened.set(false); this.error.set(''); }
  ngOnDestroy(): void { this.clear(); }
}
