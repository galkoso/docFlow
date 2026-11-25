import { ChangeDetectionStrategy, Component, computed, effect, inject, input, OnDestroy, output, signal, untracked } from '@angular/core';
import { DatePipe } from '@angular/common';
import { FormsModule } from '@angular/forms';
import { Subscription, timeout } from 'rxjs';
import { ProductQuestionApi, type ProductQuestions, type ProductQuestionHistory, type ProductQuestionChange } from '../../core/api/product-question-api';
import { SessionState } from '../../core/auth/session-state';
import { uuid } from '../comparison/comparison-evidence';
import { freezeSource } from './review-source-evidence';
import {
  questionText, validProductQuestions, validProductQuestionHistory, validProductQuestionReceipt, type Question, type QuestionIntent,
} from './product-question-evidence';

export interface QuestionScope { actor: string; proposal: string; }
type Capture = QuestionScope & { epoch: number };
type Draft = { kind: 'create'; version: number } | { kind: ProductQuestionChange['action']['kind']; version: number; question: Question; blocking: boolean };
type Attempt = { capture: Capture; intent: QuestionIntent };
@Component({
  selector: 'app-product-questions', imports: [DatePipe, FormsModule], changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: './product-questions-view.html', styleUrl: './product-questions-view.css',
})
export class ProductQuestionsView implements OnDestroy {
  readonly scope = input.required<QuestionScope>();
  readonly accessDenied = output<void>();
  readonly recorded = output<void>();
  private readonly api = inject(ProductQuestionApi);
  private readonly session = inject(SessionState);
  private readonly capture = signal<Capture | null>(null);
  private rejected = '';
  private lists?: Subscription;
  private histories?: Subscription;
  private writes?: Subscription;
  private listGeneration = 0;
  private historyGeneration = 0;
  private writeGeneration = 0;
  private timer?: ReturnType<typeof setInterval>;
  private readonly now = signal(Date.now());
  private readonly retryUntil = signal(0);
  private readonly listData = signal<ProductQuestions | null>(null);
  private readonly historyData = signal<ProductQuestionHistory | null>(null);
  private readonly attempt = signal<Attempt | null>(null);
  private readonly readFailure = signal('');
  private readonly writeFailure = signal('');
  readonly opened = signal(false);
  readonly loading = signal(false);
  readonly reading = signal(false);
  readonly sending = signal(false);
  readonly selected = signal<string | null>(null);
  readonly after = signal<string | undefined>(undefined);
  readonly historyAfter = signal<number | undefined>(undefined);
  readonly draft = signal<Draft | null>(null);
  readonly question = signal('');
  readonly answer = signal('');
  readonly blocking = signal(false);
  readonly message = signal('');
  readonly live = computed(() => this.owns(this.capture()));
  readonly view = computed(() => this.live() ? this.listData() : null);
  readonly history = computed(() => this.live() && this.historyData()?.question.id === this.selected() ? this.historyData() : null);
  readonly pending = computed(() => this.owns(this.attempt()?.capture ?? null) ? this.attempt() : null);
  readonly error = computed(() => this.live() ? this.writeFailure() || this.readFailure() : '');
  readonly hasDraft = computed(() => !!this.draft() || !!this.attempt() || this.sending());
  readonly retryWait = computed(() => Math.max(0, Math.ceil((this.retryUntil() - this.now()) / 1000)));
  readonly questionLength = computed(() => Array.from(this.question()).length);
  readonly answerLength = computed(() => Array.from(this.answer()).length);
  readonly canSend = computed(() => {
    const d = this.draft();
    if (!this.live() || !d || this.pending() || this.sending() || this.retryWait()) return false;
    if (d.kind === 'create') return !this.loading() && !!this.view()?.can_create && d.version === this.view()?.proposal_version
      && questionText(this.question()) && (!this.answer() || questionText(this.answer()));
    const q = this.history()?.question;
    return !this.reading() && !!q && q.id === d.question.id && q.current.version === d.version && this.allowed(q, d.kind, d.blocking)
      && (!['propose', 'decide'].includes(d.kind) || questionText(this.answer()));
  });
  constructor() {
    effect(() => {
      const s = this.scope(), actor = this.session.profile()?.user_id, epoch = this.session.logoutEpoch();
      untracked(() => {
        const c = this.capture(), key = `${s.actor}/${s.proposal}/${epoch}`;
        if (!uuid(s.actor) || !uuid(s.proposal) || actor !== s.actor) { this.clear(); return; }
        if (key === this.rejected) return;
        if (!c || c.actor !== s.actor || c.proposal !== s.proposal || c.epoch !== epoch) {
          this.clear(); this.rejected = ''; this.capture.set(Object.freeze({ ...s, epoch }));
        }
      });
    });
  }
  private owns(c: Capture | null): c is Capture {
    const s = this.scope();
    return !!c && c.actor === s.actor && c.proposal === s.proposal && c.actor === this.session.profile()?.user_id && c.epoch === this.session.logoutEpoch();
  }
  private denied(e: { status?: number; error?: { error?: { code?: string } } }, read: boolean): boolean {
    return [401, 404].includes(e?.status ?? 0) || read && e?.status === 403 || e?.error?.error?.code === 'SESSION_IDENTITY_CHANGED';
  }
  private reject(): void {
    const c = this.capture(); this.rejected = c ? `${c.actor}/${c.proposal}/${c.epoch}` : '';
    this.clear(); this.accessDenied.emit();
  }
  open(): void { if (this.live()) { this.opened.set(true); this.refresh(); } }
  refresh(): void {
    if (!this.live() || this.sending()) return;
    this.loadList(this.after()); if (this.selected()) this.loadHistory(this.selected()!, this.historyAfter());
  }
  page(after?: string): void { if (this.live() && !this.sending()) this.loadList(after); }
  private loadList(after?: string): void {
    const c = this.capture(); if (!this.owns(c)) return;
    this.lists?.unsubscribe(); const generation = ++this.listGeneration;
    this.after.set(after); this.listData.set(null); this.loading.set(true); this.readFailure.set('');
    this.lists = this.api.list(c.actor, c.proposal, after).pipe(timeout(20000)).subscribe({
      next: v => {
        if (generation !== this.listGeneration || !this.owns(c)) return;
        this.loading.set(false);
        if (!validProductQuestions(v, c.actor, c.proposal, after)) { this.readFailure.set('רשימת השאלות אינה תואמת לבקשה.'); return; }
        this.listData.set(freezeSource(structuredClone(v)));
      },
      error: e => { if (generation !== this.listGeneration || !this.owns(c)) return; this.loading.set(false);
        if (this.denied(e, true)) { this.reject(); return; } this.readFailure.set('לא ניתן לטעון את שאלות המוצר כרגע. הטיוטה נשארת בעמוד.'); },
    });
  }
  select(q: Question): void {
    if (!this.view()?.questions.includes(q) || this.pending() || !this.discard()) return;
    this.selected.set(q.id); this.loadHistory(q.id);
  }
  historyPage(after?: number): void { if (this.selected() && !this.sending()) this.loadHistory(this.selected()!, after); }
  private loadHistory(question: string, after?: number): void {
    const c = this.capture(); if (!this.owns(c)) return;
    this.histories?.unsubscribe(); const generation = ++this.historyGeneration;
    this.historyData.set(null); this.reading.set(true); this.historyAfter.set(after);
    this.histories = this.api.history(c.actor, c.proposal, question, after).pipe(timeout(20000)).subscribe({
      next: v => {
        if (generation !== this.historyGeneration || !this.owns(c) || question !== this.selected()) return;
        this.reading.set(false);
        if (!validProductQuestionHistory(v, c.actor, c.proposal, question, after)) { this.readFailure.set('היסטוריית השאלה אינה תואמת לזהות ולגרסאות שהתבקשו.'); return; }
        this.historyData.set(freezeSource(structuredClone(v)));
      },
      error: e => { if (generation !== this.historyGeneration || !this.owns(c)) return; this.reading.set(false);
        if (this.denied(e, true)) { this.reject(); return; } this.readFailure.set('לא ניתן לטעון את היסטוריית השאלה כרגע.'); },
    });
  }
  beginCreate(): void {
    const page = this.view(); if (!page?.can_create || this.loading() || !this.discard()) return;
    this.draft.set({ kind: 'create', version: page.proposal_version }); this.blocking.set(false);
  }
  private allowed(q: Question, kind: Exclude<Draft['kind'], 'create'>, blocking: boolean): boolean {
    const p = q.permissions;
    return kind === 'propose' ? p.can_propose : kind === 'decide' ? p.can_decide : kind === 'reopen' ? p.can_reopen : p.can_toggle_blocking && blocking !== q.current.blocking;
  }
  begin(kind: Exclude<Draft['kind'], 'create'>): void {
    const q = this.history()?.question;
    if (!q || this.reading() || !this.allowed(q, kind, !q.current.blocking) || !this.discard()) return;
    this.draft.set({ kind, version: q.current.version, question: freezeSource(structuredClone(q)), blocking: !q.current.blocking });
    if (kind === 'propose' || kind === 'decide') this.answer.set(q.current.answer ?? '');
  }
  updateContext(): void {
    const d = this.draft(); if (!d || this.pending() || this.sending() || !this.live()) return;
    if (d.kind === 'create' && this.view()?.can_create) this.draft.set({ ...d, version: this.view()!.proposal_version });
    else if (d.kind !== 'create') {
      const q = this.history()?.question;
      if (q?.id === d.question.id && this.allowed(q, d.kind, d.blocking)) this.draft.set({ ...d, version: q.current.version, question: freezeSource(structuredClone(q)) });
    }
  }
  send(): void {
    if (!this.canSend()) return;
    const d = this.draft()!, c = this.capture()!;
    const action: ProductQuestionChange['action'] | null = d.kind === 'create' ? null
      : d.kind === 'propose' || d.kind === 'decide' ? { kind: d.kind, answer: this.answer() }
      : d.kind === 'reopen' ? { kind: 'reopen' } : { kind: 'set_blocking', blocking: d.blocking };
    const intent: QuestionIntent = d.kind === 'create'
      ? { kind: 'create', body: { request_id: crypto.randomUUID(), expected_proposal_version: d.version, text: this.question(), blocking: this.blocking(), proposed_answer: this.answer() || null } }
      : { kind: 'change', question: d.question.id, body: { request_id: crypto.randomUUID(), expected_version: d.version, action: action! } };
    this.attempt.set({ capture: c, intent: freezeSource(structuredClone(intent)) }); this.retry();
  }
  retry(): void {
    const a = this.pending(); if (!a || this.sending() || this.retryWait() || !this.owns(a.capture)) return;
    ++this.listGeneration; ++this.historyGeneration; this.lists?.unsubscribe(); this.histories?.unsubscribe(); this.loading.set(false); this.reading.set(false);
    const generation = ++this.writeGeneration, c = a.capture, intent = a.intent;
    this.sending.set(true); this.writeFailure.set(''); this.message.set('');
    const request = intent.kind === 'create' ? this.api.create(c.actor, c.proposal, intent.body) : this.api.change(c.actor, c.proposal, intent.question, intent.body);
    this.writes = request.pipe(timeout(30000)).subscribe({
      next: receipt => {
        if (generation !== this.writeGeneration || !this.owns(c)) return;
        this.sending.set(false);
        if (!validProductQuestionReceipt(receipt, c.actor, c.proposal, intent)) { this.writeFailure.set('התוצאה אינה תואמת לבקשה. הבקשה המקורית נשמרת לבדיקה.'); return; }
        this.attempt.set(null); this.draft.set(null); this.question.set(''); this.answer.set('');
        this.message.set('התקבלה קבלה לפעולה. המצב העדכני נטען בנפרד; אישור סקירה ומיזוג דורשים פעולות נפרדות.');
        this.selected.set(receipt.question_id); this.loadList(); this.loadHistory(receipt.question_id); this.recorded.emit();
      },
      error: e => {
        if (generation !== this.writeGeneration || !this.owns(c)) return;
        this.sending.set(false); if (this.denied(e, false)) { this.reject(); return; }
        const retry = e?.headers?.get?.('Retry-After') ?? '', seconds = /^[1-9]\d{0,9}$/.test(retry) && Number(retry) <= 4294967295 ? Number(retry) : e?.status === 429 ? 600 : e?.status === 503 ? 1 : 0;
        if (seconds) { clearInterval(this.timer); const now = Date.now(); this.now.set(now); this.retryUntil.set(now + seconds * 1000);
          this.timer = setInterval(() => { this.now.set(Date.now()); if (!this.retryWait()) clearInterval(this.timer); }, 1000); }
        const code = e?.error?.error?.code;
        const known = e?.status === 409 && ['QUESTION_VERSION_MISMATCH', 'QUESTION_ACTION_UNAVAILABLE', 'PROPOSAL_NOT_EDITABLE', 'QUESTION_MERGE_IN_PROGRESS'].includes(code)
          || [400, 413].includes(e?.status) && code === 'INVALID_PRODUCT_QUESTION';
        if (known) { this.attempt.set(null); this.listData.set(null); this.historyData.set(null);
          this.writeFailure.set('יש לבדוק את המצב והגרסה העדכניים לפני שליחה חדשה. הטיוטה נשארה; עדכון ההקשר נעשה במפורש.'); }
        else this.writeFailure.set(e?.status === 403 ? 'סמכות הפעולה אינה זמינה כעת. יש לבדוק היסטוריה והרשאות; הבקשה המקורית נשמרת.'
          : e?.status === 429 ? 'נדרשת המתנה. ניסיון נוסף יבדוק את אותה בקשה.' : 'תוצאת הפעולה טרם אומתה. ניתן לרענן היסטוריה או לבדוק שוב את אותה בקשה.');
      },
    });
  }
  cancel(): void { if (this.discard()) { this.writeFailure.set(''); this.message.set(''); } }
  private discard(): boolean {
    if (this.sending() || this.hasDraft() && !window.confirm('לסגור את הטיוטה או הבקשה שתוצאתה אינה ודאית? יש לבדוק היסטוריה לפני פעולה חדשה.')) return false;
    ++this.writeGeneration; this.writes?.unsubscribe(); clearInterval(this.timer); this.retryUntil.set(0);
    this.attempt.set(null); this.draft.set(null); this.question.set(''); this.answer.set(''); this.writeFailure.set(''); this.message.set(''); return true;
  }
  label(kind: string): string { return ({ create: 'שאלה חדשה', propose: 'הצעת תשובה', decide: 'הכרעת מוצר מפורשת', reopen: 'פתיחה מחדש', set_blocking: 'שינוי חסימה', OPEN: 'פתיחת שאלה', PROPOSE: 'הצעת תשובה', DECIDE: 'הכרעת מוצר', REOPEN: 'פתיחה מחדש', SET_BLOCKING: 'שינוי חסימה' } as Record<string, string>)[kind] ?? kind; }
  stateLabel(state: string): string { return ({ OPEN: 'פתוחה', PROPOSED: 'תשובה מוצעת — טרם אושרה', DECIDED: 'הוכרעה במפורש במוצר' } as Record<string, string>)[state] ?? state; }
  private clear(): void {
    ++this.listGeneration; ++this.historyGeneration; ++this.writeGeneration;
    this.lists?.unsubscribe(); this.histories?.unsubscribe(); this.writes?.unsubscribe(); clearInterval(this.timer);
    this.capture.set(null); this.listData.set(null); this.historyData.set(null); this.attempt.set(null); this.draft.set(null); this.question.set(''); this.answer.set('');
    this.opened.set(false); this.selected.set(null); this.after.set(undefined); this.historyAfter.set(undefined); this.loading.set(false); this.reading.set(false); this.sending.set(false);
    this.retryUntil.set(0); this.readFailure.set(''); this.writeFailure.set(''); this.message.set('');
  }
  ngOnDestroy(): void { this.clear(); }
}
