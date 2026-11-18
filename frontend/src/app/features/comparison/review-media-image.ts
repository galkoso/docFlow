import { afterRenderEffect, ChangeDetectionStrategy, Component, computed, effect, ElementRef, inject, input, untracked } from '@angular/core';
import { SessionState } from '../../core/auth/session-state';
import { ReviewMediaState } from './review-media-state';
import { reviewMediaKey, type ReviewMediaContext, type ReviewMediaScope } from './review-media-evidence';

@Component({
  selector: 'app-review-media-image', providers: [ReviewMediaState], changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    @if (scope()) {
      @if (state.busy()) {<span role="status">טוענים ובודקים תמונה: {{ alt() || 'ללא תיאור' }}…</span>}
      @if (state.error()) {<span role="alert">{{ state.error() }}</span>}
      @if (!current() && !state.busy()) {<button type="button" (click)="load()">{{ state.started() ? 'ניסיון נוסף לתמונה' : 'טעינת תמונה' }}: {{ alt() || 'ללא תיאור' }}</button>}
      @if (current(); as data) {
        @if (!state.loaded()) {<span role="status">מכינים תמונה לתצוגה…</span>}
        <img [src]="data.url" [alt]="alt() || 'תמונה ללא תיאור'" [title]="title()" [hidden]="!state.loaded()"
          decoding="async" referrerpolicy="no-referrer" (load)="decoded($event)" (error)="failed($event)">
      }
    } @else {<span role="img" [attr.aria-label]="alt() || 'תמונה'">תמונה: {{ alt() || 'ללא תיאור' }} — ההפניה אינה זמינה</span>}
  `,
  styles: `:host{display:inline-block;max-width:100%;min-width:0;overflow-wrap:anywhere}img{display:block;max-width:100%;height:auto;margin-block:1rem}img[hidden]{display:none}
    button{min-height:44px;padding:.5rem;cursor:pointer}button:focus-visible{outline:3px solid #275b9b;outline-offset:3px}`,
})
export class ReviewMediaImage {
  readonly context = input<ReviewMediaContext | null>(null);
  readonly path = input<string | null>(null);
  readonly alt = input('');
  readonly title = input('');
  readonly state = inject(ReviewMediaState);
  private readonly session = inject(SessionState);
  private readonly element = inject<ElementRef<HTMLElement>>(ElementRef);
  readonly scope = computed<ReviewMediaScope | null>(() => {
    const context = this.context(), path = this.path();
    const scope = context && path ? { ...context, path } : null;
    return scope && !this.state.denied() && scope.actor === this.session.profile()?.user_id && reviewMediaKey(scope) ? scope : null;
  });
  readonly current = computed(() => { const scope = this.scope(); return scope && this.state.owns(scope) ? this.state.data() : null; });
  constructor() {
    effect(() => { const scope = this.scope(); untracked(() => this.state.open(scope)); });
    afterRenderEffect(onCleanup => {
      const scope = this.scope();
      if (!scope || typeof IntersectionObserver === 'undefined') return;
      let stopped = false;
      const observer = new IntersectionObserver(entries => {
        if (!stopped && entries.some(entry => entry.isIntersecting) && this.scope() && reviewMediaKey(this.scope()) === reviewMediaKey(scope)) {
          stopped = true; observer.disconnect(); this.load();
        }
      }, { rootMargin: '300px 0px' });
      observer.observe(this.element.nativeElement);
      onCleanup(() => { stopped = true; observer.disconnect(); });
    });
  }
  load(): void { const scope = this.scope(); if (scope && this.state.owns(scope)) this.state.load(); }
  decoded(event: Event): void { const image = event.target as HTMLImageElement; this.state.decoded(image.src, image.naturalWidth, image.naturalHeight); }
  failed(event: Event): void { this.state.failedImage((event.target as HTMLImageElement).src); }
}
