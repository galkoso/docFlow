import { ChangeDetectionStrategy, Component, computed, effect, inject, input, OnDestroy, output, signal, untracked } from '@angular/core';
import { Subscription, timeout } from 'rxjs';
import { ReviewApi, type ReviewCoverage, type ReviewCoverageRequest, type ReviewSnapshot, type ReviewUnits,
  type ReviewUnitCoverage } from '../../core/api/review-api';
import { SessionState } from '../../core/auth/session-state';
import { reviewAreas } from './official-review-evidence';
import { validReviewUnits } from './review-units-evidence';
import { validReviewCoverage } from './review-coverage-evidence';
import { freezeSource } from './review-source-evidence';

@Component({
  selector: 'app-review-coverage', changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: './review-coverage-view.html', styleUrl: './review-coverage-view.css',
})
export class ReviewCoverageView implements OnDestroy {
  readonly actor = input.required<string>();
  readonly proposal = input.required<string>();
  readonly snapshot = input.required<ReviewSnapshot>();
  readonly page = input.required<ReviewUnits>();
  readonly domain = input.required<ReviewCoverageRequest['domain']>();
  readonly refreshToken = input<string | null>(null);
  readonly accessDenied = output<void>();
  private readonly api = inject(ReviewApi);
  private readonly session = inject(SessionState);
  private request?: Subscription;
  private generation = 0;
  private activeKey: string | null = null;
  private seenToken: string | null = null;
  private deniedKey: string | null = null;
  private readonly data = signal<{ key: string; value: ReviewCoverage } | null>(null);
  readonly busy = signal(false);
  private readonly failure = signal<{ key: string; message: string } | null>(null);
  private readonly key = computed(() => {
    const actor = this.actor(), proposal = this.proposal(), snapshot = this.snapshot(), page = this.page(), domain = this.domain();
    if (actor !== this.session.profile()?.user_id || !reviewAreas.includes(domain) || !validReviewUnits(page, actor, proposal, snapshot, page.offset)
      || snapshot.metadata.context.author_id === actor || !snapshot.metadata.assignments.some(a => a.reviewer_id === actor && a.domain === domain)) return null;
    return JSON.stringify([actor, proposal, snapshot.snapshot_id, snapshot.fingerprint, snapshot.metadata.summary.head_sha,
      snapshot.file_count, snapshot.metadata.context.author_id, snapshot.metadata.assignments,
      page.offset, page.total, page.available, page.units, domain, this.session.logoutEpoch()]);
  });
  readonly view = computed(() => { const data = this.data(); return data && data.key === this.key() ? data.value : null; });
  readonly error = computed(() => { const failure = this.failure(); return failure?.key === this.key() ? failure.message : ''; });
  constructor() {
    effect(() => {
      const key = this.key(), token = this.refreshToken();
      untracked(() => {
        if (key !== this.activeKey) {
          this.cancel(); this.data.set(null); this.failure.set(null); this.activeKey = key; this.seenToken = token;
          if (key) this.load(key);
        } else if (key && token && token !== this.seenToken) { this.seenToken = token; this.load(key); }
      });
    });
  }
  refresh(): void { const key = this.key(); if (key && !this.busy()) this.load(key); }
  item(id: string): ReviewUnitCoverage | undefined { return this.view()?.units.find(u => u.unit_id === id); }
  statusLabel(value: ReviewUnitCoverage['before']): string {
    return { absent: 'לא קיים בצד זה', unsupported: 'תיעוד צפייה טרם זמין', unviewed: 'טרם תועדה קריאה', partial: 'תועדה קריאה חלקית', viewed: 'תועדה קריאה מלאה' }[value];
  }
  private load(key: string): void {
    if (key === this.deniedKey || key !== this.key()) return;
    this.cancel();
    const generation = this.generation, actor = this.actor(), proposal = this.proposal(), domain = this.domain();
    const snapshot = freezeSource(structuredClone(this.snapshot())), page = freezeSource(structuredClone(this.page()));
    this.data.set(null); this.failure.set(null); this.busy.set(true);
    this.request = this.api.coverage(actor, proposal, snapshot.snapshot_id, { expected_fingerprint: snapshot.fingerprint, domain, offset: page.offset, limit: 20 })
      .pipe(timeout(20_000)).subscribe({
        next: value => {
          if (generation !== this.generation || key !== this.key()) return;
          this.busy.set(false);
          if (!validReviewCoverage(value, actor, proposal, snapshot, page, domain)) {
            this.failure.set({ key, message: 'נתוני הקריאה אינם תואמים לגרסה, לתחום או לפריטים המוצגים.' }); return;
          }
          this.data.set({ key, value: freezeSource(structuredClone(value)) });
        },
        error: error => {
          if (generation !== this.generation || key !== this.key()) return;
          this.busy.set(false);
          if ([401, 403, 404].includes(error?.status)) { this.deniedKey = key; this.data.set(null); this.accessDenied.emit(); return; }
          this.failure.set({ key, message: 'לא ניתן לבדוק את מצב הקריאה כרגע. אפשר לנסות שוב במפורש.' });
        },
      });
  }
  private cancel(): void { ++this.generation; this.request?.unsubscribe(); this.request = undefined; this.busy.set(false); }
  ngOnDestroy(): void { this.cancel(); this.data.set(null); }
}
