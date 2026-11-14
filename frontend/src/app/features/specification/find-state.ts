import { computed, inject, Injectable, signal } from '@angular/core';
import { HttpErrorResponse } from '@angular/common/http';
import { Subscription } from 'rxjs';
import { BookApi, SearchPage } from '../../core/api/book-api';
import { BookState, bookError } from './book-state';

/** Counts come from the complete server snapshot; only three result pages are retained. */
@Injectable()
export class FindState {
  private readonly api = inject(BookApi);
  private readonly book = inject(BookState);
  private repository?: string;
  private commit?: string;
  private request?: Subscription;
  private timer?: ReturnType<typeof setTimeout>;
  private generation = 0;
  readonly query = signal('');
  readonly total = signal<number | null>(null);
  readonly position = signal(0);
  readonly loading = signal(false);
  readonly error = signal<string | null>(null);
  readonly announcement = signal('');
  readonly pages = signal(new Map<number, SearchPage>());
  readonly current = computed(() => {
    const index = this.position();
    return this.pages().get(Math.floor(index / 100) * 100)?.matches[index % 100] ?? null;
  });
  readonly hits = computed(() => [...this.pages().values()].flatMap(page => page.matches));

  scope(repository?: string, commit?: string): void {
    if (this.repository === repository && this.commit === commit) return;
    this.repository = repository;
    this.commit = commit;
    this.restart();
  }

  search(query: string): void {
    if (query === this.query()) return;
    this.query.set(query);
    this.restart();
  }

  clear(): void {
    this.query.set('');
    this.repository = undefined;
    this.commit = undefined;
    this.restart();
  }

  retry(): void { this.fetch(Math.floor(this.position() / 100) * 100); }

  move(delta: number): void {
    const total = this.total();
    if (!total) return;
    const next = (this.position() + delta + total) % total;
    this.announcement.set(delta > 0 && next === 0 ? 'חזרנו לתוצאה הראשונה' : delta < 0 && next === total - 1 ? 'עברנו לתוצאה האחרונה' : '');
    this.select(next);
  }

  select(position: number): void {
    const total = this.total();
    if (total == null || position < 0 || position >= total) return;
    this.position.set(position);
    const start = Math.floor(position / 100) * 100;
    if (!this.pages().has(start)) this.fetch(start);
    else {
      ++this.generation;
      this.request?.unsubscribe();
      this.loading.set(false);
      this.error.set(null);
      this.pages.update(pages => {
        const next = new Map(pages);
        const page = next.get(start)!;
        next.delete(start);
        next.set(start, page);
        return next;
      });
    }
  }

  private restart(): void {
    ++this.generation;
    this.request?.unsubscribe();
    clearTimeout(this.timer);
    this.pages.set(new Map());
    this.total.set(null);
    this.position.set(0);
    this.error.set(null);
    this.announcement.set('');
    this.loading.set(false);
    if (!this.query() || !this.repository || !this.commit) return;
    if (Array.from(this.query()).length > 200) { this.error.set('אפשר לחפש עד 200 תווים.'); return; }
    this.loading.set(true);
    this.timer = setTimeout(() => this.fetch(0), 200);
  }

  private fetch(start: number): void {
    const repository = this.repository;
    const commit = this.commit;
    const query = this.query();
    if (!repository || !commit || !query || Array.from(query).length > 200) return;
    this.request?.unsubscribe();
    const generation = ++this.generation;
    this.loading.set(true);
    this.error.set(null);
    this.request = this.api.find(repository, commit, query, start).subscribe({
      next: page => {
        if (generation !== this.generation) return;
        if (page.snapshot_sha !== commit || page.query !== query || page.start_at !== start
          || (this.total() != null && page.total_matches !== this.total())) {
          this.loading.set(false);
          this.pages.set(new Map());
          this.total.set(null);
          this.error.set('תוצאות החיפוש אינן תואמות לגרסה. יש לטעון את הספר מחדש.');
          return;
        }
        this.total.set(page.total_matches);
        this.pages.update(pages => {
          const next = new Map(pages);
          next.delete(start);
          next.set(start, page);
          while (next.size > 3) next.delete(next.keys().next().value!);
          return next;
        });
        this.loading.set(false);
      },
      error: error => {
        if (generation !== this.generation) return;
        this.loading.set(false);
        this.error.set(bookError(error));
        if (error instanceof HttpErrorResponse && [401, 403, 404].includes(error.status)) {
          this.book.clear();
          this.book.error.set('הגישה לספר אינה זמינה. יש להתחבר או לטעון אותו מחדש.');
          this.clear();
        }
      },
    });
  }
}
