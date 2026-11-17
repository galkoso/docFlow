import { computed, inject, Injectable, signal } from '@angular/core';
import { HttpErrorResponse } from '@angular/common/http';
import { Subscription, timeout } from 'rxjs';
import { ReviewConfigurationApi, type ReviewArea, type ReviewConfiguration, type ReviewerCandidates,
  type ReviewerSelection, type ReviewConfigurationRequest, type ReviewSettings } from '../../core/api/review-configuration-api';
import { areas, canonical, trimReason, uuid, validCandidates, validConfiguration, validReceipt } from './review-evidence';

type Phase = 'idle' | 'loading' | 'ready' | 'saving' | 'uncertain' | 'error';
type Choices = { items: ReviewerCandidates['reviewers']; after?: string; next: string | null; previous: (string | undefined)[];
  loading: boolean; error: string; loaded: boolean; };
const empty = (): Choices => ({ items: [], next: null, previous: [], loading: false, error: '', loaded: false });
const initial = (): ReviewSettings => ({ ui_impact: 'undecided', ui_impact_reason: null, assignments: [] });
const errors: Record<string, string> = {
  REVIEW_CONFIGURATION_STALE: 'ההצעה או הגדרת הסקירה השתנו. יש לטעון מחדש לפני שמירה.',
  REVIEWER_NOT_ELIGIBLE: 'סוקר שנבחר אינו כשיר כעת. יש לבדוק את הרשימה ולעדכן את הבחירה.',
  UI_IMPACT_REASON_REQUIRED: 'כשאין השפעת ממשק, נדרש נימוק.',
  PROPOSAL_BUSY: 'פעולת שמירה או פרסום קודמת טרם הושלמה. יש לבדוק שוב את המצב.',
  PROPOSAL_NOT_EDITABLE: 'מצב ההצעה אינו מאפשר שינוי בהגדרת הסקירה.',
  REVIEW_CONFIGURATION_FORBIDDEN: 'אין הרשאה לערוך את הגדרת הסקירה.',
};
const denied = (error: unknown): boolean => error instanceof HttpErrorResponse
  && ([401, 403, 404].includes(error.status) || error.error?.error?.code === 'SESSION_IDENTITY_CHANGED');

@Injectable()
export class ReviewConfigurationState {
  private readonly api = inject(ReviewConfigurationApi);
  private requests = new Subscription();
  private generation = 0;
  private intent: { actor: string; proposal: string; body: ReviewConfigurationRequest } | null = null;
  readonly owner = signal<string | null>(null);
  readonly proposal = signal<string | null>(null);
  readonly data = signal<ReviewConfiguration | null>(null);
  readonly settings = signal<ReviewSettings>(initial());
  readonly names = signal<Record<string, string>>({});
  readonly phase = signal<Phase>('idle');
  readonly error = signal('');
  readonly notice = signal('');
  readonly reloadRequired = signal(false);
  readonly choices = signal<Record<ReviewArea, Choices>>({ PRODUCT: empty(), DEVELOPMENT: empty(), DESIGN: empty() });
  readonly pending = computed(() => this.phase() === 'saving' || this.phase() === 'uncertain');
  readonly dirty = computed(() => !!this.data() && JSON.stringify(canonical(this.settings())) !== JSON.stringify(canonical(this.data()!.settings)));
  readonly editable = computed(() => !!this.data()?.can_edit && !this.pending() && !this.reloadRequired() && this.phase() === 'ready');
  readonly validation = computed(() => {
    const form = this.settings();
    if (form.ui_impact === 'no' && !trimReason(form.ui_impact_reason ?? '')) return 'יש לנמק מדוע השינוי אינו משפיע על הממשק.';
    if (Array.from(form.ui_impact_reason ?? '').length > 10000 || form.ui_impact_reason?.includes('\0')) return 'הנימוק יכול לכלול עד 10,000 תווים, ללא תו אפס.';
    return form.assignments.length > 200 ? 'אפשר לבחור עד 200 הקצאות סקירה.' : '';
  });
  private cancel(): void { ++this.generation; this.requests.unsubscribe(); this.requests = new Subscription(); }
  clear(): void {
    this.cancel(); this.intent = null; this.owner.set(null); this.proposal.set(null); this.data.set(null); this.settings.set(initial());
    this.names.set({}); this.phase.set('idle'); this.error.set(''); this.notice.set(''); this.reloadRequired.set(false);
    this.choices.set({ PRODUCT: empty(), DEVELOPMENT: empty(), DESIGN: empty() });
  }
  suspend(): void {
    this.cancel();
    if (this.phase() === 'saving') { this.phase.set('uncertain'); this.error.set('תוצאת השמירה טרם אומתה. אפשר לבדוק את אותה בקשה לאחר התחברות.'); }
    else if (this.phase() === 'loading') this.phase.set('idle');
    this.choices.update(all => Object.fromEntries(areas.map(area => [area, { ...all[area], loading: false }])) as Record<ReviewArea, Choices>);
  }
  open(actor: string, proposal: string): void {
    if (this.pending()) return;
    this.clear(); this.owner.set(actor); this.proposal.set(proposal);
    if (!uuid(proposal)) { this.phase.set('error'); this.error.set('כתובת ההצעה אינה תקינה.'); return; }
    const generation = this.generation; this.phase.set('loading');
    this.requests.add(this.api.read(actor, proposal).pipe(timeout(20000)).subscribe({
      next: data => {
        if (generation !== this.generation) return;
        if (!validConfiguration(data, proposal)) { this.phase.set('error'); this.error.set('הגדרת הסקירה שהתקבלה אינה תואמת לבקשה.'); return; }
        this.install(data); this.phase.set('ready');
        if (data.can_edit) for (const area of areas) this.candidates(area);
      },
      error: () => { if (generation === this.generation) { this.phase.set('error'); this.error.set('לא ניתן לטעון את הגדרת הסקירה. יש לבדוק התחברות והרשאות ולנסות שוב.'); } },
    }));
  }
  private install(data: ReviewConfiguration): void {
    this.data.set(data); this.settings.set(canonical(data.settings));
    this.names.set(Object.fromEntries(data.reviewers.map(item => [item.reviewer_id, item.display_name])));
  }
  setImpact(value: ReviewSettings['ui_impact']): void {
    if (this.editable() && ['yes', 'no', 'undecided'].includes(value)) this.settings.update(form => ({ ...form, ui_impact: value }));
  }
  setReason(value: string): void { if (this.editable()) this.settings.update(form => ({ ...form, ui_impact_reason: value })); }
  selected(domain: ReviewArea, reviewer: string): boolean { return this.settings().assignments.some(item => item.domain === domain && item.reviewer_id === reviewer); }
  add(domain: ReviewArea, reviewer: string): void {
    const candidate = this.choices()[domain].items.find(item => item.reviewer_id === reviewer);
    if (!this.editable() || !candidate || this.selected(domain, reviewer) || this.settings().assignments.length >= 200) return;
    this.names.update(names => ({ ...names, [reviewer]: candidate.display_name }));
    this.settings.update(form => ({ ...form, assignments: [...form.assignments, { reviewer_id: reviewer, domain, required: true }] }));
  }
  remove(item: ReviewerSelection): void {
    if (this.editable()) this.settings.update(form => ({ ...form, assignments: form.assignments.filter(other => other.domain !== item.domain || other.reviewer_id !== item.reviewer_id) }));
  }
  required(item: ReviewerSelection, required: boolean): void {
    if (this.editable()) this.settings.update(form => ({ ...form, assignments: form.assignments.map(other =>
      other.domain === item.domain && other.reviewer_id === item.reviewer_id ? { ...other, required } : other) }));
  }
  ineligible(item: ReviewerSelection): boolean {
    return this.data()?.reviewers.some(status => status.reviewer_id === item.reviewer_id && status.domain === item.domain && (!status.active || !status.qualified)) ?? false;
  }
  candidates(domain: ReviewArea, direction: 'retry' | 'next' | 'previous' = 'retry'): void {
    const actor = this.owner(), proposal = this.proposal(), data = this.data(), old = this.choices()[domain];
    if (!actor || !proposal || !data?.can_edit || old.loading || this.pending()) return;
    let after = old.after, previous = [...old.previous];
    if (direction === 'next') { if (!old.next) return; previous.push(old.after); after = old.next; }
    if (direction === 'previous') { if (!previous.length) return; after = previous.pop(); }
    const generation = this.generation;
    this.choices.update(all => ({ ...all, [domain]: { ...old, loading: true, error: '' } }));
    this.requests.add(this.api.candidates(actor, proposal, domain, after).pipe(timeout(20000)).subscribe({
      next: page => {
        if (generation !== this.generation) return;
        if (!validCandidates(page, proposal, data.configuration_version, domain, after)) {
          this.choiceError(domain, 'רשימת הסוקרים אינה תואמת לגרסת ההגדרה. יש לטעון מחדש.'); this.reloadRequired.set(true); return;
        }
        this.choices.update(all => ({ ...all, [domain]: { items: page.reviewers, after, previous, next: page.next_after ?? null,
          loading: false, error: '', loaded: true } }));
      },
      error: error => {
        if (generation !== this.generation) return;
        if (denied(error)) this.accessChanged(); else this.choiceError(domain, 'רשימת הסוקרים לא נטענה. אפשר לנסות שוב.');
      },
    }));
  }
  private choiceError(domain: ReviewArea, message: string): void {
    this.choices.update(all => ({ ...all, [domain]: { ...all[domain], loading: false, error: message } }));
  }
  save(): void {
    const actor = this.owner(), proposal = this.proposal(), data = this.data();
    if (!actor || !proposal || !data?.head_sha || !this.editable() || !this.dirty() || this.validation()) return;
    // This private snapshot is never replaced while an outcome is uncertain.
    this.intent = { actor, proposal, body: { request_id: crypto.randomUUID(), expected_head_sha: data.head_sha,
      expected_proposal_version: data.proposal_version, expected_configuration_version: data.configuration_version, settings: canonical(this.settings()) } };
    this.send();
  }
  retry(): void { if (this.phase() === 'uncertain') this.send(); }
  private send(): void {
    const intent = this.intent; if (!intent || this.phase() === 'saving') return;
    this.cancel(); const generation = this.generation;
    this.choices.update(all => Object.fromEntries(areas.map(area => [area, { ...all[area], loading: false }])) as Record<ReviewArea, Choices>);
    this.phase.set('saving'); this.error.set(''); this.notice.set('');
    this.requests.add(this.api.update(intent.actor, intent.proposal, intent.body).pipe(timeout(20000)).subscribe({
      next: receipt => {
        if (generation !== this.generation) return;
        if (!validReceipt(receipt, intent.proposal, intent.body)) { this.uncertain(); return; }
        this.intent = null; this.install(receipt.current); this.phase.set('ready');
        this.notice.set(receipt.current.configuration_version > receipt.applied_configuration_version
          ? 'הבקשה נשמרה. מוצגת הגדרה חדשה יותר שנשמרה מאז.' : receipt.changed ? 'הגדרת הסקירה נשמרה.' : 'ההגדרה כבר תואמת למצב השמור.');
        this.choices.set({ PRODUCT: empty(), DEVELOPMENT: empty(), DESIGN: empty() });
        if (receipt.current.can_edit) for (const area of areas) this.candidates(area);
      },
      error: (error: unknown) => {
        if (generation !== this.generation) return;
        if (!(error instanceof HttpErrorResponse) || error.status === 0 || error.status >= 500) { this.uncertain(); return; }
        const code: unknown = error.error?.error?.code;
        this.intent = null;
        if (denied(error)) { this.accessChanged(); return; }
        this.phase.set('ready'); this.reloadRequired.set(error.status !== 422);
        this.error.set(typeof code === 'string' && errors[code] ? errors[code] : 'ההגדרה לא נשמרה. יש לבדוק את הפרטים ולטעון את המצב הנוכחי לפני ניסיון חדש.');
      },
    }));
  }
  private accessChanged(): void { this.clear(); this.phase.set('error'); this.error.set('הגישה השתנתה. יש לבדוק התחברות והרשאות ולטעון מחדש.'); }
  private uncertain(): void { this.phase.set('uncertain'); this.error.set('תוצאת השמירה טרם אומתה. יש לבדוק שוב את אותה בקשה לפני שינוי נוסף.'); }
}
