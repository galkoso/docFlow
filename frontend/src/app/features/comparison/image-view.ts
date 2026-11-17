import { ChangeDetectionStrategy, Component, computed, effect, inject, input, untracked } from '@angular/core';
import { SessionState } from '../../core/auth/session-state';
import { RawScope } from './raw-state';
import { ImageScope, ImageState } from './image-state';

@Component({
  selector: 'app-comparison-image-side', providers: [ImageState], changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    @if (state.absent()) {<p>התמונה אינה קיימת בצד זה.</p>}
    @if (state.busy()) {<p role="status">טוענים ובודקים את התמונה…</p>}
    @if (state.error()) {<p role="alert">{{ state.error() }}</p>
      @if (state.authRequired()) {<button (click)="session.refresh()">בדיקת התחברות מחדש</button>}
      @else if (state.canRetry()) {<button (click)="state.retry()" [disabled]="state.busy()">ניסיון נוסף לאותה תמונה</button>}
    }
    @if (current(); as data) {
      @if (!state.loaded()) {<p role="status">מכינים את התמונה לתצוגה…</p>}
      <img [src]="data.url" [alt]="scope()?.side === 'before' ? 'תמונה לפני השינוי' : 'תמונה אחרי השינוי'" [hidden]="!state.loaded()"
        (load)="decoded($event)" (error)="failed($event)" decoding="async" referrerpolicy="no-referrer">
      <p>גרסת תצוגה: {{ data.evidence.width }} × {{ data.evidence.height }} · מקור: {{ data.evidence.source_bytes }} בתים</p>
      <details><summary>זהות מקור התמונה</summary><code>{{ data.evidence.source_sha256 }}</code></details>
    }
  `,
  styles: `:host{display:block;min-width:0}img{max-width:100%;height:auto}code{direction:ltr;display:block;overflow-wrap:anywhere}
    button{padding:.5rem;cursor:pointer}button:focus-visible{outline:3px solid #275b9b;outline-offset:3px}`,
})
export class ComparisonImageSide {
  readonly scope = input<ImageScope | null>(null);
  readonly state = inject(ImageState);
  readonly session = inject(SessionState);
  readonly current = computed(() => { const scope = this.scope(), data = this.state.data(); return scope && this.state.owns(scope) ? data : null; });
  constructor() { effect(() => { const scope = this.scope(); untracked(() => this.state.open(scope)); }); }
  decoded(event: Event): void { const image = event.target as HTMLImageElement; this.state.decoded(image.src, image.naturalWidth, image.naturalHeight); }
  failed(event: Event): void { this.state.imageFailed((event.target as HTMLImageElement).src); }
}
@Component({
  selector: 'app-comparison-image', imports: [ComparisonImageSide], changeDetection: ChangeDetectionStrategy.OnPush,
  template: `<h2>השוואת תמונה</h2><div class="pair"><section><h3>לפני — לקריאה בלבד</h3><bdi>{{ scope()?.file?.before?.path }}</bdi>
      <app-comparison-image-side [scope]="before()" /></section>
    <section><h3>אחרי</h3><bdi>{{ scope()?.file?.after?.path }}</bdi><app-comparison-image-side [scope]="after()" /></section></div>`,
  styles: `:host{display:block;margin-block:1.5rem}.pair{display:grid;grid-template-columns:minmax(0,1fr) minmax(0,1fr);gap:1rem;direction:rtl}
    section{min-width:0;padding:1rem;border:1px solid #c3cbd5}bdi{overflow-wrap:anywhere}@media(max-width:650px){.pair{grid-template-columns:1fr}}`,
})
export class ComparisonImage {
  readonly scope = input<RawScope | null>(null);
  readonly before = computed<ImageScope | null>(() => this.scope() ? { file: this.scope()!, side: 'before' } : null);
  readonly after = computed<ImageScope | null>(() => this.scope() ? { file: this.scope()!, side: 'after' } : null);
}
