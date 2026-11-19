import { ChangeDetectionStrategy, Component, computed, effect, inject, input, OnDestroy, output, signal, untracked } from '@angular/core';
import { DatePipe } from '@angular/common';
import { FormsModule } from '@angular/forms';
import { Subscription, timeout } from 'rxjs';
import {
  DiscussionApi, type DiscussionAction, type DiscussionComment, type DiscussionComments, type DiscussionIntent,
  type DiscussionThread, type DiscussionThreads, type OpenDiscussion,
} from '../../core/api/discussion-api';
import { ReviewApi, type ReviewBookDocument, type ReviewDocuments, type ReviewSnapshot } from '../../core/api/review-api';
import type { ComparisonChange } from '../../core/api/comparison-api';
import { SessionState } from '../../core/auth/session-state';
import { integer } from '../comparison/comparison-evidence';
import { validDiscussionComments, validDiscussionReceipt, validDiscussionThreads } from './discussion-evidence';
import { validReviewDocuments } from './review-documents-evidence';

export interface DiscussionScope { actor: string; proposal: string; snapshot: ReviewSnapshot | null; file: ComparisonChange | null; }
type Draft = { kind: 'open'; scope: DiscussionScope; version: number; book: ReviewBookDocument | null }
  | { kind: 'reply'; thread: string; version: number; replyTo: string | null }
  | { kind: 'edit'; thread: string; version: number; comment: string; commentVersion: number };
type Attempt = { scope: DiscussionScope; epoch: number; intent: DiscussionIntent };
type Mode = 'proposal' | 'snapshot' | 'document' | 'section' | 'lines' | 'book_document' | 'book_section' | 'book_lines';
function frozen<T>(value: T): T {
  if (value && typeof value === 'object') { Object.values(value).forEach(frozen); Object.freeze(value); }
  return value;
}
@Component({
  selector: 'app-discussions', imports: [DatePipe, FormsModule], changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: './discussions-view.html', styleUrl: './discussions-view.css',
})
export class DiscussionsView implements OnDestroy {
  readonly scope = input.required<DiscussionScope>();
  readonly accessDenied = output<void>();
  private readonly api = inject(DiscussionApi);
  private readonly reviews = inject(ReviewApi);
  private readonly session = inject(SessionState);
  private owner: DiscussionScope | null = null;
  private epoch = -1;
  private listGeneration = 0;
  private detailGeneration = 0;
  private writeGeneration = 0;
  private bookGeneration = 0;
  private listRead?: Subscription;
  private detailRead?: Subscription;
  private write?: Subscription;
  private bookRead?: Subscription;
  private readonly bookData = signal<ReviewDocuments | null>(null);
  readonly bookLoading = signal(false);
  readonly bookError = signal('');
  readonly book = computed(() => this.owns(this.owner, this.epoch) ? this.bookData() : null);
  private timer?: ReturnType<typeof setInterval>;
  private readonly now = signal(Date.now());
  private readonly retryUntil = signal(0);
  private readonly list = signal<DiscussionThreads | null>(null);
  private readonly detail = signal<DiscussionComments | null>(null);
  readonly selected = signal<string | null>(null);
  readonly loading = signal(false);
  readonly reading = signal(false);
  readonly sending = signal(false);
  readonly error = signal('');
  readonly message = signal('');
  readonly draft = signal<Draft | null>(null);
  readonly body = signal('');
  readonly blocking = signal(false);
  readonly mode = signal<Mode>('proposal');
  readonly side = signal<'before' | 'after'>('after');
  readonly sectionId = signal('');
  readonly startLine = signal(1);
  readonly endLine = signal(1);
  readonly pending = signal<Attempt | null>(null);
  readonly length = computed(() => Array.from(this.body()).length);
  readonly hasDraft = computed(() => !!this.draft() || !!this.pending() || this.sending());
  readonly retryWait = computed(() => Math.max(0, Math.ceil((this.retryUntil() - this.now()) / 1000)));
  readonly view = computed(() => this.owns(this.owner, this.epoch) ? this.list() : null);
  readonly comments = computed(() => this.owns(this.owner, this.epoch) && this.detail()?.thread.id === this.selected() ? this.detail() : null);
  readonly targetFile = computed(() => { const d = this.draft(); return d?.kind === 'open' ? d.scope.file : this.scope().file; });
  readonly canSend = computed(() => {
    const d = this.draft(), list = this.view();
    if (!d || !list?.can_comment || this.sending() || this.pending() || this.retryWait() || !this.body().trim()
      || this.body().includes('\0') || this.length() > 10000) return false;
    if (d.kind === 'open') return !!this.origin(d) && (!this.blocking() || list.can_open_blocking_thread)
      && (this.mode() !== 'proposal' || d.version === list.proposal_version);
    const page = this.comments();
    if (!page?.thread.permissions.can_comment || page.thread.id !== d.thread || page.thread.version !== d.version) return false;
    return d.kind !== 'edit' || page.comments.some(c => c.id === d.comment && c.version === d.commentVersion && c.can_edit);
  });
  constructor() {
    effect(() => {
      const scope = this.scope(), live = this.session.profile()?.user_id, epoch = this.session.logoutEpoch();
      untracked(() => { this.clear(); this.owner = scope; this.epoch = epoch; if (live === scope.actor) this.loadList(); });
    });
  }
  private owns(scope: DiscussionScope | null, epoch: number): boolean {
    return !!scope && this.scope() === scope && this.session.profile()?.user_id === scope.actor && this.session.logoutEpoch() === epoch;
  }
  private denied(e: { status?: number; error?: { error?: { code?: string } } }): boolean {
    return [401, 404].includes(e?.status ?? 0) || e?.error?.error?.code === 'SESSION_IDENTITY_CHANGED';
  }
  private rejectAccess(): void { this.clear(); this.accessDenied.emit(); }
  loadBook(offset = 0): void {
    const scope = this.owner, epoch = this.epoch, snapshot = scope?.snapshot;
    if (!snapshot || !this.owns(scope, epoch) || this.sending() || !integer(offset, 1000)) return;
    this.bookRead?.unsubscribe(); const generation = ++this.bookGeneration;
    this.bookData.set(null); this.bookLoading.set(true); this.bookError.set('');
    this.bookRead = this.reviews.documents(scope!.actor, scope!.proposal, snapshot.snapshot_id, offset).pipe(timeout(20000)).subscribe({
      next: value => {
        if (generation !== this.bookGeneration || !this.owns(scope, epoch)) return;
        this.bookLoading.set(false);
        if (!validReviewDocuments(value, scope!.actor, scope!.proposal, snapshot, offset)) { this.bookError.set('רשימת המסמכים אינה תואמת לגרסת הסקירה שנבחרה.'); return; }
        this.bookData.set(value);
      },
      error: e => {
        if (generation !== this.bookGeneration || !this.owns(scope, epoch)) return;
        this.bookLoading.set(false); if (this.denied(e)) { this.rejectAccess(); return; }
        this.bookError.set('לא ניתן לטעון את מסמכי הספר כרגע.');
      },
    });
  }
  bookHref(entry: ReviewBookDocument): string | null {
    const scope = this.scope(), snapshot = scope.snapshot;
    return snapshot ? `/changes/${encodeURIComponent(scope.proposal)}/book?commit=${encodeURIComponent(snapshot.metadata.summary.head_sha)}&doc=${encodeURIComponent(entry.source.document_id!)}` : null;
  }
  beginBook(entry: ReviewBookDocument): void {
    if (!this.book()?.available || !this.book()!.documents.includes(entry)) return;
    this.openDraft(entry);
  }
  refresh(): void { if (!this.sending()) { this.loadList(); if (this.selected()) this.loadComments(this.selected()!); } }
  page(after?: string): void { if (this.discard()) { this.selected.set(null); this.detail.set(null); this.loadList(after); } }
  private loadList(after?: string): void {
    const scope = this.owner, epoch = this.epoch;
    if (!this.owns(scope, epoch)) return;
    this.listRead?.unsubscribe(); const generation = ++this.listGeneration;
    this.loading.set(true); this.list.set(null);
    this.listRead = this.api.threads(scope!.actor, scope!.proposal, after).pipe(timeout(20000)).subscribe({
      next: value => {
        if (generation !== this.listGeneration || !this.owns(scope, epoch)) return;
        this.loading.set(false);
        if (!validDiscussionThreads(value, scope!.actor, scope!.proposal, after)) { this.error.set('פרטי הדיונים אינם תואמים לבקשה.'); return; }
        this.list.set(value);
      },
      error: e => {
        if (generation !== this.listGeneration || !this.owns(scope, epoch)) return;
        this.loading.set(false); if (this.denied(e)) { this.rejectAccess(); return; }
        this.error.set('לא ניתן לטעון את הדיונים כרגע.');
      },
    });
  }
  select(thread: DiscussionThread): void {
    if (!this.view()?.threads.some(t => t.id === thread.id) || !this.discard()) return;
    this.selected.set(thread.id); this.loadComments(thread.id, thread.version);
  }
  private loadComments(thread: string, version?: number, after?: string): void {
    const scope = this.owner, epoch = this.epoch;
    if (!this.owns(scope, epoch)) return;
    this.detailRead?.unsubscribe(); const generation = ++this.detailGeneration;
    this.detail.set(null); this.reading.set(true);
    this.detailRead = this.api.comments(scope!.actor, scope!.proposal, thread, version, after).pipe(timeout(20000)).subscribe({
      next: value => {
        if (generation !== this.detailGeneration || !this.owns(scope, epoch) || this.selected() !== thread) return;
        this.reading.set(false);
        if (!validDiscussionComments(value, scope!.actor, scope!.proposal, thread, version, after)) { this.error.set('פרטי ההודעות אינם תואמים לדיון ולגרסה.'); return; }
        this.detail.set(value);
      },
      error: e => {
        if (generation !== this.detailGeneration || !this.owns(scope, epoch) || this.selected() !== thread) return;
        this.reading.set(false); if (this.denied(e)) { this.rejectAccess(); return; }
        this.error.set(e?.status === 409 ? 'הדיון השתנה. יש לטעון את ההודעות העדכניות לפני המשך.' : 'לא ניתן לטעון את הודעות הדיון כרגע.');
      },
    });
  }
  commentPage(after?: string): void {
    const page = this.comments(); if (page && this.discard()) this.loadComments(page.thread.id, page.thread.version, after);
  }
  beginOpen(): void {
    this.openDraft();
  }
  private openDraft(book?: ReviewBookDocument): void {
    const v = this.view(); if (!v?.can_comment || !this.discard()) return;
    this.draft.set({ kind: 'open', scope: this.scope(), version: v.proposal_version,
      book: book ? frozen({ ...book, source: { ...book.source } }) : null });
    this.mode.set(book ? 'book_document' : this.scope().snapshot ? 'snapshot' : 'proposal'); this.blocking.set(false);
    this.side.set(this.scope().file?.after ? 'after' : 'before'); this.sectionId.set(''); this.startLine.set(1); this.endLine.set(1);
  }
  reply(comment?: DiscussionComment): void {
    const p = this.comments(); if (!p?.thread.permissions.can_comment || comment && !p.comments.includes(comment) || !this.discard()) return;
    this.draft.set({ kind: 'reply', thread: p.thread.id, version: p.thread.version, replyTo: comment?.id ?? null });
  }
  edit(comment: DiscussionComment): void {
    const p = this.comments(); if (!p || !p.comments.includes(comment) || !comment.can_edit || comment.body == null || !this.discard()) return;
    this.draft.set({ kind: 'edit', thread: p.thread.id, version: p.thread.version, comment: comment.id, commentVersion: comment.version });
    this.body.set(comment.body);
  }
  updateContext(): void {
    if (this.pending() || this.sending()) return;
    const d = this.draft(), p = this.comments();
    if (d?.kind === 'open' && this.mode() === 'proposal' && this.view()) this.draft.set({ ...d, version: this.view()!.proposal_version });
    else if (d?.kind === 'reply' && p?.thread.id === d.thread) this.draft.set({ ...d, version: p.thread.version });
    else if (d?.kind === 'edit' && p?.thread.id === d.thread) {
      const c = p.comments.find(c => c.id === d.comment && c.can_edit);
      if (c) this.draft.set({ ...d, version: p.thread.version, commentVersion: c.version });
    }
  }
  private origin(d: Extract<Draft, { kind: 'open' }>): OpenDiscussion['origin'] | null {
    if (this.mode() === 'proposal') return { kind: 'proposal', expected_proposal_version: d.version };
    const snapshot = d.scope.snapshot;
    if (!snapshot) return null;
    const common = { kind: 'snapshot' as const, snapshot_id: snapshot.snapshot_id, expected_fingerprint: snapshot.fingerprint };
    if (this.mode() === 'snapshot') return { ...common, anchor: { kind: 'proposal' } };
    const isBook = this.mode().startsWith('book_'), mode = isBook ? this.mode().slice(5) : this.mode();
    const book = isBook ? d.book : null, file = d.scope.file, side = file?.[this.side()];
    if (isBook ? !book : !file || !side || !['document', 'catalog'].includes(file.kind)) return null;
    let selection: { kind: 'document' } | { kind: 'section'; section_id: string } | { kind: 'lines'; start_line: number; end_line: number };
    if (mode === 'document') { if (!book && (file?.kind !== 'document' || !side?.document_id)) return null; selection = { kind: 'document' }; }
    else if (mode === 'section') {
      if (!book && (file?.kind !== 'document' || !side?.document_id) || !/^[A-Z0-9-]{1,120}$/.test(this.sectionId())) return null;
      selection = { kind: 'section', section_id: this.sectionId() };
    } else if (mode === 'lines' && integer(this.startLine(), Number.MAX_SAFE_INTEGER, 1)
      && integer(this.endLine(), Number.MAX_SAFE_INTEGER, this.startLine()) && this.endLine() - this.startLine() < 500) {
      selection = { kind: 'lines', start_line: this.startLine(), end_line: this.endLine() };
    } else return null;
    return { ...common, anchor: book ? { kind: 'book_document', entry_id: book.id, selection }
      : { kind: 'file', file_id: file!.id, side: this.side(), selection } };
  }
  send(): void {
    if (!this.canSend()) return;
    const d = this.draft()!; let intent: DiscussionIntent;
    if (d.kind === 'open') intent = { kind: 'open', body: { request_id: crypto.randomUUID(), origin: this.origin(d)!, blocking: this.blocking(), body: this.body() } };
    else {
      const mutation: DiscussionAction['mutation'] = d.kind === 'reply' ? { kind: 'reply', body: this.body(), reply_to_id: d.replyTo }
        : { kind: 'edit', body: this.body(), comment_id: d.comment, expected_comment_version: d.commentVersion };
      intent = { kind: 'action', thread: d.thread, body: { request_id: crypto.randomUUID(), expected_thread_version: d.version, mutation } };
    }
    this.capture(intent);
  }
  act(kind: 'resolve' | 'reopen' | 'set_blocking' | 'tombstone', comment?: DiscussionComment): void {
    const p = this.comments(); if (!p || this.hasDraft() || comment && !p.comments.includes(comment)) return;
    const permission = p.thread.permissions;
    if (kind === 'resolve' && !permission.can_resolve || kind === 'reopen' && !permission.can_reopen
      || kind === 'set_blocking' && !permission.can_toggle_blocking || kind === 'tombstone' && (!comment?.can_tombstone
        || !window.confirm('למחוק את תוכן ההודעה? ההיסטוריה והחסימה של הדיון יישמרו.'))) return;
    const mutation: DiscussionAction['mutation'] = kind === 'tombstone'
      ? { kind, comment_id: comment!.id, expected_comment_version: comment!.version }
      : kind === 'set_blocking' ? { kind, blocking: !p.thread.blocking } : { kind };
    this.capture({ kind: 'action', thread: p.thread.id, body: { request_id: crypto.randomUUID(), expected_thread_version: p.thread.version, mutation } });
  }
  private capture(intent: DiscussionIntent): void {
    this.pending.set({ scope: this.scope(), epoch: this.epoch, intent: frozen(intent) }); this.retry();
  }
  retry(): void {
    const attempt = this.pending();
    if (!attempt || this.sending() || this.retryWait() || !this.owns(attempt.scope, attempt.epoch) || !this.view()?.can_comment) return;
    ++this.listGeneration; ++this.detailGeneration; this.listRead?.unsubscribe(); this.detailRead?.unsubscribe();
    ++this.bookGeneration; this.bookRead?.unsubscribe(); this.bookLoading.set(false);
    this.loading.set(false); this.reading.set(false); const generation = ++this.writeGeneration;
    this.sending.set(true); this.error.set(''); this.message.set('');
    this.write = this.api.write(attempt.scope.actor, attempt.scope.proposal, attempt.intent).pipe(timeout(90000)).subscribe({
      next: value => {
        if (generation !== this.writeGeneration || !this.owns(attempt.scope, attempt.epoch)) return;
        this.sending.set(false);
        if (!validDiscussionReceipt(value, attempt.scope.actor, attempt.scope.proposal, attempt.intent)) {
          this.error.set('תוצאת הפעולה אינה תואמת לבקשה. הבקשה המקורית נשמרת לבדיקה.'); return;
        }
        this.pending.set(null); this.draft.set(null); this.body.set(''); this.message.set('פעולת הדיון נשמרה. החלטות סקירה רשמיות נבדקות בנפרד.');
        this.selected.set(value.thread_id); this.loadList(); this.loadComments(value.thread_id);
      },
      error: e => {
        if (generation !== this.writeGeneration || !this.owns(attempt.scope, attempt.epoch)) return;
        this.sending.set(false); if (this.denied(e)) { this.rejectAccess(); return; }
        const wait = e?.headers?.get?.('Retry-After') ?? '';
        if (/^[1-9]\d{0,9}$/.test(wait) && Number(wait) <= 4294967295) {
          clearInterval(this.timer); this.now.set(Date.now()); this.retryUntil.set(Date.now() + Number(wait) * 1000);
          this.timer = setInterval(() => { this.now.set(Date.now()); if (!this.retryWait()) clearInterval(this.timer); }, 1000);
        }
        const code = e?.error?.error?.code;
        if (e?.status === 409 && ['DISCUSSION_CONTEXT_CHANGED', 'DISCUSSION_NOT_READY', 'COMMENT_TOMBSTONED'].includes(code)
          || [400, 413].includes(e?.status) && code === 'INVALID_DISCUSSION') {
          this.pending.set(null); this.error.set('הפעולה לא נשמרה. הטקסט נשאר בעמוד; יש לבדוק את ההקשר ולעדכן אותו במפורש לפני שליחה חדשה.');
        } else this.error.set(e?.status === 429 ? 'נדרשת המתנה. ניסיון נוסף יבדוק את אותה בקשה.'
          : e?.status === 403 ? 'הפעולה נחסמה לפי ההרשאות הנוכחיות. יש לבדוק היסטוריה והרשאות לפני ניסיון נוסף.'
          : 'תוצאת הפעולה טרם אומתה. אפשר לרענן היסטוריה או לבדוק שוב את אותה בקשה.');
      },
    });
  }
  cancel(): void { if (this.discard()) this.error.set(''); }
  private discard(): boolean {
    if (this.sending() || this.hasDraft() && !window.confirm('לעזוב את טיוטת ההודעה או הבקשה שתוצאתה טרם אומתה? יש לבדוק היסטוריה לפני פתיחת פעולה חדשה.')) return false;
    ++this.writeGeneration; this.write?.unsubscribe(); this.pending.set(null); this.draft.set(null); this.body.set(''); return true;
  }
  private clear(): void {
    ++this.listGeneration; ++this.detailGeneration; ++this.writeGeneration;
    this.listRead?.unsubscribe(); this.detailRead?.unsubscribe(); this.write?.unsubscribe(); clearInterval(this.timer);
    ++this.bookGeneration; this.bookRead?.unsubscribe(); this.bookData.set(null); this.bookLoading.set(false); this.bookError.set('');
    this.list.set(null); this.detail.set(null); this.selected.set(null); this.draft.set(null); this.pending.set(null); this.body.set('');
    this.loading.set(false); this.reading.set(false); this.sending.set(false); this.retryUntil.set(0); this.error.set(''); this.message.set('');
  }
  ngOnDestroy(): void { this.clear(); }
}
