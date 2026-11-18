import { ChangeDetectionStrategy, Component, computed, effect, inject, input, output, signal, untracked } from '@angular/core';
import { SessionState } from '../../core/auth/session-state';
import { COMPARISON_BODY_API } from '../../core/api/comparison-body-api';
import { SharedReviewBodyApi } from '../../core/api/shared-review-body-api';
import { RawState, type RawScope } from '../comparison/raw-state';
import { sameFile } from '../comparison/readable-evidence';
import { ReadableView } from '../comparison/readable-view';
import type { ReviewMediaContext } from '../comparison/review-media-evidence';
import { integer } from '../comparison/comparison-evidence';
import { ComparisonImage } from '../comparison/image-view';

@Component({
  selector: 'app-shared-review-body', imports: [ReadableView, ComparisonImage],
  providers: [RawState, SharedReviewBodyApi, { provide: COMPARISON_BODY_API, useExisting: SharedReviewBodyApi }],
  changeDetection: ChangeDetectionStrategy.OnPush, templateUrl: './shared-review-body.html', styleUrl: './shared-review-body.css',
})
export class SharedReviewBody {
  readonly scope = input<RawScope | null>(null);
  readonly mediaValidationVersion = input<number | null>(null);
  readonly mode = input<'raw' | 'readable'>('readable');
  readonly row = input<string | null>(null);
  readonly modeSelected = output<'raw' | 'readable'>();
  readonly rowSelected = output<string | null>();
  readonly accessDenied = output<void>();
  readonly raw = inject(RawState);
  private readonly api = inject(SharedReviewBodyApi);
  private readonly session = inject(SessionState);
  readonly showEqual = signal(true);
  readonly active = computed(() => {
    const scope = this.scope(); return scope && scope.actor === this.session.profile()?.user_id && !this.api.denied() ? scope : null;
  });
  readonly mediaContext = computed<ReviewMediaContext | null>(() => {
    const scope = this.active(), validationVersion = this.mediaValidationVersion();
    if (!scope || !integer(validationVersion, 2147483647, 1)) return null;
    return { actor: scope.actor, proposal: scope.proposal, snapshot: scope.snapshot, content: scope.content, head: scope.head, base: scope.base, validationVersion };
  });
  readonly rawView = computed(() => { const scope = this.active(), data = this.raw.data(); return scope && data && sameFile(data, scope) ? data : null; });
  readonly segments = computed(() => this.rawView()?.raw.segments.filter(segment => this.showEqual() || segment.kind !== 'equal') ?? []);
  constructor() {
    effect(() => { const scope = this.active(), mode = this.mode(); untracked(() => this.raw.open(mode === 'raw' ? scope : null)); });
    effect(() => { if (this.api.denied()) untracked(() => { this.raw.clear(); this.accessDenied.emit(); }); });
  }
  setMode(mode: 'raw' | 'readable'): void { if (this.active()) this.modeSelected.emit(mode); }
  selectRow(row: string | null): void { if (this.active()) this.rowSelected.emit(row); }
  move(action: 'next' | 'back' | 'retry'): void { if (this.active()) this.raw[action](); }
}
