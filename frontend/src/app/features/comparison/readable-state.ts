import { COMPARISON_BODY_API } from '../../core/api/comparison-body-api';
import { inject, Injectable, OnDestroy, signal } from '@angular/core';
import { HttpErrorResponse } from '@angular/common/http';
import { Subscription, timeout } from 'rxjs';
import { ReadableFileResponse, ReadableRow } from '../../core/api/comparison-api';
import { digest, integer } from './comparison-evidence';
import { pageIdentity, rowIdentity, scopeKey, validPage } from './readable-evidence';
import type { RawScope } from './raw-state';

/** A page of unit metadata and one selected pair; source/event bodies belong to
 * separate, bounded side controllers and never accumulate in this inventory. */
@Injectable()
export class ReadableState implements OnDestroy {
  private readonly api = inject(COMPARISON_BODY_API);
  private scope: RawScope | null = null;
  private readonly key = signal('');
  private generation = 0;
  private request?: Subscription;
  private offset = 0;
  private row: string | null = null;
  private metadata: string | null = null;
  readonly data = signal<ReadableFileResponse | null>(null);
  readonly selected = signal<ReadableRow | null>(null);
  readonly busy = signal(false);
  readonly error = signal('');
  readonly authRequired = signal(false);
  open(scope: RawScope | null, row: string | null = null): void {
    if (!scope) { this.clear(); return; }
    const key = scopeKey(scope);
    if (key && key === this.key()) {
      if (row === this.row) return;
      if (row != null && !digest(row)) { this.clear(); this.error.set('מזהה יחידת ההשוואה אינו תקין.'); return; }
      this.row = row; this.selected.set(null);
      if (row && this.data()?.page.rows.some(unit => unit.change.id === row)) { this.select(row); return; }
      if (row == null && this.offset === 0 && this.data()) return;
      this.offset = 0; this.fetch(); return;
    }
    this.clear();
    if (!key || row != null && !digest(row)) { this.error.set('לא ניתן להציג השוואה קריאה לקובץ שנבחר.'); return; }
    this.scope = structuredClone(scope); this.key.set(key); this.row = row; this.fetch();
  }
  owns(scope: RawScope): boolean { return scopeKey(scope) === this.key(); }
  page(offset: number): void {
    if (!this.scope || !integer(offset, 20004) || this.busy() || this.authRequired()) return;
    this.offset = offset; this.row = null; this.fetch();
  }
  select(id: string): void {
    const row = this.data()?.page.rows.find(row => row.change.id === id);
    if (row) this.selected.set(structuredClone(row));
  }
  retry(): void { if (this.scope && !this.busy() && !this.authRequired()) this.fetch(); }
  clear(): void {
    ++this.generation; this.request?.unsubscribe(); this.request = undefined; this.scope = null; this.key.set(''); this.offset = 0; this.row = null; this.metadata = null;
    this.data.set(null); this.selected.set(null); this.busy.set(false); this.error.set(''); this.authRequired.set(false);
  }
  ngOnDestroy(): void { this.clear(); }
  private fetch(): void {
    const scope = this.scope; if (!scope) return;
    this.request?.unsubscribe(); const generation = ++this.generation, offset = this.offset, row = this.row;
    const known = new Map(this.data()?.page.rows.map(row => [row.change.id, rowIdentity(row)]) ?? []);
    const selected = this.selected(); if (selected) known.set(selected.change.id, rowIdentity(selected));
    this.data.set(null); this.busy.set(true); this.error.set('');
    const request = row ? this.api.readableRow(scope.actor, scope.proposal, scope.snapshot, scope.file.id, row)
      : this.api.readable(scope.actor, scope.proposal, scope.snapshot, scope.file.id, offset, 50);
    this.request = request.pipe(timeout(30_000)).subscribe({
      next: response => {
        if (generation !== this.generation || this.scope !== scope) return;
        this.busy.set(false);
        if (!validPage(response, scope, row ? null : offset)
          || row != null && !response.page.rows.some(unit => unit.change.id === row) || this.metadata != null && pageIdentity(response.page) !== this.metadata
          || response.page.rows.some(row => known.has(row.change.id) && known.get(row.change.id) !== rowIdentity(row))) {
          this.selected.set(null); this.error.set('פרטי ההשוואה הקריאה אינם תואמים לגרסה שנבחרה. אפשר לבדוק שוב אותה גרסה.'); return;
        }
        this.metadata = pageIdentity(response.page); this.offset = response.page.offset; this.data.set(structuredClone(response));
        if (row) this.select(row);
      },
      error: error => {
        if (generation !== this.generation || this.scope !== scope) return;
        this.busy.set(false); this.selected.set(null);
        this.authRequired.set(error instanceof HttpErrorResponse && (error.status === 401 || error.error?.error?.code === 'SESSION_IDENTITY_CHANGED'));
        const message: unknown = error instanceof HttpErrorResponse ? error.error?.error?.message_he : null;
        this.error.set(typeof message === 'string' && message.length <= 2000 ? message : 'לא ניתן לטעון כרגע את פרטי ההשוואה. אפשר לבדוק שוב אותה גרסה.');
      },
    });
  }
}
