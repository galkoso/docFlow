import { ChangeDetectionStrategy, Component, computed, effect, inject, input, OnDestroy, output, signal, untracked } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { Subscription, timeout } from 'rxjs';
import { ReviewApi, type ReviewUnits, type ReviewSnapshot } from '../../core/api/review-api';
import { SessionState } from '../../core/auth/session-state';
import { digest, integer, sha, uuid } from '../comparison/comparison-evidence';
import { ReviewSourceState } from './review-source-state';
import { validReviewUnits, type ReviewUnit } from './review-units-evidence';

@Component({
  selector: 'app-review-units', imports: [FormsModule], providers: [ReviewSourceState], changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: './review-units-view.html', styleUrl: './review-units-view.css',
})
export class ReviewUnitsView implements OnDestroy {
  readonly actor = input.required<string>();
  readonly proposal = input.required<string>();
  readonly snapshot = input.required<ReviewSnapshot>();
  readonly offset = input(0);
  readonly domains = input<('PRODUCT' | 'DEVELOPMENT' | 'DESIGN')[]>([]);
  readonly source = inject(ReviewSourceState);
  readonly hasPending = this.source.hasPending;
  readonly domain = signal<'PRODUCT' | 'DEVELOPMENT' | 'DESIGN'>('PRODUCT');
  private capturedRoot: string | null = null;
  private readonly rootKey = computed(() => {
    const actor = this.actor(), snapshot = this.snapshot(), s = snapshot?.metadata?.summary;
    return actor === this.session.profile()?.user_id && s ? JSON.stringify([actor, this.proposal(), snapshot.snapshot_id,
      snapshot.fingerprint, s.head_sha, s.merge_base_sha, s.repository_id, this.session.logoutEpoch()]) : null;
  });
  readonly sourceVisible = computed(() => !!this.source.scope() && this.rootKey() === this.capturedRoot && this.rootKey() != null);
  readonly offsetSelected = output<number>();
  readonly accessDenied = output<void>();
  private readonly api = inject(ReviewApi);
  private readonly session = inject(SessionState);
  private request?: Subscription;
  private generation = 0;
  readonly opened = signal(false);
  readonly busy = signal(false);
  private readonly failure = signal<{ key: string; message: string } | null>(null);
  private readonly deniedKey = signal<string | null>(null);
  private readonly page = signal<{ key: string; value: ReviewUnits } | null>(null);
  private readonly key = computed(() => {
    const actor = this.actor(), proposal = this.proposal(), snapshot = this.snapshot(), offset = this.offset(), s = snapshot?.metadata?.summary;
    if (actor !== this.session.profile()?.user_id || !uuid(actor) || !uuid(proposal) || snapshot?.proposal_id !== proposal
      || !uuid(snapshot.snapshot_id) || !digest(snapshot.fingerprint) || !s || !sha(s.head_sha) || !sha(s.merge_base_sha)
      || !uuid(s.repository_id) || !integer(offset, 40000)) return null;
    return JSON.stringify([actor, proposal, snapshot.snapshot_id, snapshot.fingerprint, s.head_sha, s.merge_base_sha, s.repository_id,
      offset, this.session.logoutEpoch()]);
  });
  readonly view = computed(() => {
    const page = this.page(), key = this.key(); return key && this.deniedKey() !== key && page?.key === key ? page.value : null;
  });
  readonly error = computed(() => { const failure = this.failure(); return failure?.key === this.key() ? failure?.message ?? '' : ''; });
  readonly sides = ['before', 'after'] as const;
  constructor() {
    effect(() => { const root = this.rootKey(); untracked(() => { if (root !== this.capturedRoot) this.source.clear(); }); });
    effect(() => { if (this.source.denied()) untracked(() => this.accessDenied.emit()); });
    effect(() => {
      const domains = this.domains();
      untracked(() => { if (!this.source.hasPending() && domains.length && !domains.includes(this.domain())) this.domain.set(domains[0]); });
    });
    effect(() => {
      const key = this.key(), offset = this.offset();
      untracked(() => {
        this.cancel(); this.page.set(null); this.failure.set(null);
        if (offset > 0) this.opened.set(true);
        if (key && this.opened() && key !== this.deniedKey()) this.load(key);
      });
    });
  }
  show(): void {
    const key = this.key(); if (key && !this.opened()) { this.opened.set(true); this.load(key); }
  }
  refresh(): void { const key = this.key(); if (key && !this.busy() && this.deniedKey() !== key) this.load(key); }
  move(offset: number): void {
    const page = this.view();
    if (page && !this.busy() && integer(offset, 40000) && (offset === Math.max(0, page.offset - 20) || offset === page.next_offset)) {
      this.offsetSelected.emit(offset);
    }
  }
  private load(key: string): void {
    this.cancel(); const generation = this.generation;
    this.page.set(null); this.failure.set(null); this.busy.set(true);
    this.request = this.api.units(this.actor(), this.proposal(), this.snapshot().snapshot_id, this.offset()).pipe(timeout(20_000)).subscribe({
      next: value => {
        if (generation !== this.generation || key !== this.key()) return;
        this.busy.set(false);
        if (!validReviewUnits(value, this.actor(), this.proposal(), this.snapshot(), this.offset())) {
          this.failure.set({ key, message: 'פרטי רשימת הסקירה אינם תואמים לגרסה המוצגת. יש לנסות שוב.' }); return;
        }
        this.page.set({ key, value });
      },
      error: error => {
        if (generation !== this.generation || key !== this.key()) return;
        this.busy.set(false);
        if ([401, 403, 404].includes(error?.status)) { this.deniedKey.set(key); this.page.set(null); this.accessDenied.emit(); return; }
        this.failure.set({ key, message: 'לא ניתן לטעון את פריטי הסקירה כרגע. אפשר לנסות שוב.' });
      },
    });
  }
  label(unit: ReviewUnit): string {
    const r = unit.requirement;
    return r.kind === 'section' ? 'סעיף שהשתנה' : ({ non_document: 'שינוי בקובץ', non_section: 'פרטי המסמך ותוכן מחוץ לסעיפים',
      layout: 'סדר הסעיפים', unstructured: 'סקירת הקובץ המלא', section_limit: 'סקירת הקובץ המלא', manifest_limit: 'סקירת הקובץ המלא' })[r.reason];
  }
  bookHref(unit: ReviewUnit, side: 'before' | 'after'): string | null {
    if (!this.view()?.units.includes(unit)) return null;
    const source = unit[side]?.source, snapshot = this.snapshot(), s = snapshot.metadata.summary;
    if (!source?.document_id || !source.path.startsWith('docs/') || !source.path.endsWith('.md')) return null;
    const params = new URLSearchParams({ commit: side === 'after' ? s.head_sha : s.merge_base_sha, doc: source.document_id });
    if (unit.requirement.kind === 'section') params.set('section', unit.requirement.section_id);
    if (side === 'before') params.set('repo', s.repository_id);
    return `${side === 'after' ? `/changes/${encodeURIComponent(this.proposal())}/book` : '/specification'}?${params}`;
  }
  readSource(unit: ReviewUnit, side: 'before' | 'after'): void {
    const root = this.rootKey(), snapshot = this.snapshot(), s = snapshot.metadata.summary;
    if (!root || !this.view()?.units.includes(unit) || !unit[side]?.range || !this.domains().includes(this.domain()) || this.source.hasPending()) return;
    this.capturedRoot = root;
    this.source.open({ actor: this.actor(), proposal: this.proposal(), snapshot: snapshot.snapshot_id, fingerprint: snapshot.fingerprint,
      head: s.head_sha, base: s.merge_base_sha, unit, side, domain: this.domain() }, () => this.rootKey() === root);
  }
  chooseDomain(value: string): void {
    if (!this.domains().includes(value as 'PRODUCT' | 'DEVELOPMENT' | 'DESIGN') || this.source.hasPending()) return;
    this.source.clear(); this.domain.set(value as 'PRODUCT' | 'DEVELOPMENT' | 'DESIGN');
  }
  closeSource(): void {
    if (this.source.hasPending() && !window.confirm('יש בקשת תיעוד ממתינה או שתוצאתה אינה ידועה. לסגור בלי לבדוק שוב את אותה בקשה?')) return;
    this.source.clear();
  }
  domainLabel(value: string): string { return ({ PRODUCT: 'מוצר', DEVELOPMENT: 'פיתוח', DESIGN: 'עיצוב' } as Record<string, string>)[value] ?? value; }
  private cancel(): void { this.generation++; this.request?.unsubscribe(); this.request = undefined; this.busy.set(false); }
  ngOnDestroy(): void { this.cancel(); this.page.set(null); this.source.clear(); }
}
