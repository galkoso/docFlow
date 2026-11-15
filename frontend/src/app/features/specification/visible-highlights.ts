import { inject, Injectable, OnDestroy, signal } from '@angular/core';
import { HttpErrorResponse } from '@angular/common/http';
import { Subscription } from 'rxjs';
import { BookApi, Highlight, MatchWindow } from '../../core/api/book-api';
import { bookError } from './book-state';
import { TEXT_WINDOW_UTF16 } from './render-model';

export interface HighlightContext { repository: string; commit: string; document: string; proposal?: string; }
export const windowKey = (window: MatchWindow) => `${window.block_id}:${window.start_utf16}:${window.end_utf16}`;
const intersects = (hit: Highlight, window: MatchWindow) => hit.block_id === window.block_id
  && hit.end_utf16 > window.start_utf16 && hit.start_utf16 < window.end_utf16;

/** Renderer-scoped viewport data. Global counts/navigation remain in FindState. */
@Injectable()
export class VisibleHighlights implements OnDestroy {
  private readonly api = inject(BookApi);
  private context: HighlightContext | null = null;
  private query = '';
  private generation = 0;
  private readyAt = 0;
  private request?: Subscription;
  private timer?: ReturnType<typeof setTimeout>;
  private active = false;
  private visible = new Map<string, MatchWindow>();
  private cache = new Map<string, Highlight[]>();
  readonly hits = signal<Highlight[]>([]);
  readonly loading = signal(false);
  readonly error = signal<string | null>(null);
  readonly denied = signal(false);

  configure(context: HighlightContext | null, query: string): void {
    if (context?.repository === this.context?.repository && context?.commit === this.context?.commit
      && context?.document === this.context?.document && context?.proposal === this.context?.proposal && query === this.query) return;
    this.cancel();
    this.context = context;
    this.query = query;
    this.cache.clear();
    this.hits.set([]);
    this.error.set(null);
    this.denied.set(false);
    this.readyAt = Date.now() + 200;
    this.schedule();
  }

  viewport(window: MatchWindow, near: boolean): void {
    this.viewports([{ window, near }]);
  }

  viewports(entries: { window: MatchWindow; near: boolean }[]): void {
    for (const { window, near } of entries) {
      if (window.end_utf16 - window.start_utf16 > TEXT_WINDOW_UTF16 || window.start_utf16 < 0 || window.end_utf16 <= window.start_utf16) {
        this.error.set('לא ניתן לחשב סימונים עבור טווח התוכן שהתקבל.'); return;
      }
      const key = windowKey(window);
      if (near) this.visible.set(key, window);
      else this.visible.delete(key);
    }
    this.trimCache();
    this.publish();
    this.schedule();
  }

  unobserve(): void {
    this.cancel();
    this.visible.clear();
    this.publish();
  }

  retry(): void { this.error.set(null); this.schedule(); }
  ngOnDestroy(): void {
    this.cancel();
    this.visible.clear();
    this.cache.clear();
    this.hits.set([]);
    this.context = null;
    this.query = '';
  }

  private cancel(): void {
    ++this.generation;
    clearTimeout(this.timer);
    this.request?.unsubscribe();
    this.active = false;
    this.loading.set(false);
  }

  private schedule(): void {
    clearTimeout(this.timer);
    if (!this.context || !this.query || Array.from(this.query).length > 200 || this.error() || this.active) return;
    if (![...this.visible.keys()].some(key => !this.cache.has(key))) { this.loading.set(false); return; }
    this.loading.set(true);
    this.timer = setTimeout(() => this.fetch(), Math.max(50, this.readyAt - Date.now()));
  }

  private fetch(): void {
    const context = this.context;
    if (!context || this.active || this.error()) return;
    const windows = [...this.visible].filter(([key]) => !this.cache.has(key)).slice(0, 32).map(([, window]) => window);
    if (!windows.length) { this.loading.set(false); return; }
    const generation = this.generation;
    const query = this.query;
    this.active = true;
    this.request = this.api.highlights(context.repository, context.commit, context.document, query, windows, context.proposal).subscribe({
      next: response => {
        if (generation !== this.generation) return;
        this.active = false;
        if (response.snapshot_sha !== context.commit || response.document_id !== context.document || response.query !== query
          || response.matches.some(hit => !/^[a-f0-9]{64}$/.test(hit.match_id) || hit.start_utf16 >= hit.end_utf16
            || !windows.some(window => intersects(hit, window)))) {
          this.cache.clear();
          this.hits.set([]);
          this.loading.set(false);
          this.error.set('הסימונים אינם תואמים לגרסה. יש לטעון את הספר מחדש.');
          return;
        }
        for (const window of windows) {
          const key = windowKey(window);
          this.cache.delete(key);
          this.cache.set(key, response.matches.filter(hit => intersects(hit, window)));
        }
        this.trimCache();
        this.publish();
        this.schedule();
      },
      error: error => {
        if (generation !== this.generation) return;
        this.active = false;
        this.loading.set(false);
        this.error.set(bookError(error));
        if (error instanceof HttpErrorResponse && [401, 403, 404].includes(error.status)) {
          this.cache.clear(); this.hits.set([]); this.denied.set(true);
        }
      },
    });
  }

  private publish(): void {
    const unique = new Map<string, Highlight>();
    for (const key of this.visible.keys()) for (const hit of this.cache.get(key) ?? []) unique.set(hit.match_id, hit);
    this.hits.set([...unique.values()]);
  }

  private trimCache(): void {
    const offscreen = [...this.cache.keys()].filter(key => !this.visible.has(key));
    for (const key of offscreen.slice(0, Math.max(0, offscreen.length - 128))) this.cache.delete(key);
  }
}
