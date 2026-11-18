import { COMPARISON_BODY_API } from '../../core/api/comparison-body-api';
import { inject, Injectable, OnDestroy, signal } from '@angular/core';
import { HttpErrorResponse } from '@angular/common/http';
import { Subscription, timeout } from 'rxjs';
import { EventCursor, ReadableUnitResponse } from '../../core/api/comparison-api';
import type { RenderEvent } from '../specification/render-model';
import { scopeKey, unitIdentity, validUnit } from './readable-evidence';
import { stackIdentity, UnitScope, validUnitResponse } from './readable-window';

/** One unit/side and one event window. Captured cursors survive transient errors;
 * source, container state and subscriptions never survive a scope change. */
@Injectable()
export class ReadableUnitState implements OnDestroy {
  private readonly api = inject(COMPARISON_BODY_API);
  private scope: UnitScope | null = null;
  private readonly key = signal('');
  private generation = 0;
  private request?: Subscription;
  private cursor: EventCursor = { event: 0, offset: 0 };
  private history: EventCursor[] = [];
  private boundary: string | null = null;
  readonly data = signal<ReadableUnitResponse | null>(null);
  readonly events = signal<RenderEvent[]>([]);
  readonly busy = signal(false);
  readonly error = signal('');
  readonly authRequired = signal(false);
  readonly canBack = signal(false);
  open(scope: UnitScope | null): void {
    if (!scope) { this.clear(); return; }
    const fileKey = scopeKey(scope.file), source = scope.file.file[scope.side];
    if (!fileKey || !source || !validUnit(scope.unit, { state: 'READY', sha256: source.sha256, source_bytes: source.bytes,
      document_id: source.document_id, units_total: 10002, error_code: null })) {
      this.clear(); this.error.set('פרטי יחידת ההשוואה אינם תקינים.'); return;
    }
    const key = JSON.stringify([fileKey, scope.side, unitIdentity(scope.unit)]);
    if (key === this.key()) return;
    this.clear(); this.scope = structuredClone(scope); this.key.set(key); this.fetch();
  }
  owns(scope: UnitScope): boolean {
    return this.key() === JSON.stringify([scopeKey(scope.file), scope.side, unitIdentity(scope.unit)]);
  }
  next(): void {
    const window = this.data()?.window;
    if (!window?.next || this.busy() || this.history.length >= 2048) return;
    this.boundary = JSON.stringify([stackIdentity(window.end_stack), window.end_ordered_lists]);
    this.history.push({ ...this.cursor }); this.canBack.set(true); this.fetch(window.next);
  }
  back(): void {
    const previous = this.history.at(-1); if (!previous || this.busy()) return;
    this.history.pop(); this.boundary = null; this.canBack.set(this.history.length > 0); this.fetch(previous);
  }
  retry(): void { if (this.scope && !this.busy() && !this.authRequired()) this.fetch(this.cursor); }
  clear(): void {
    ++this.generation; this.request?.unsubscribe(); this.request = undefined; this.scope = null; this.key.set('');
    this.cursor = { event: 0, offset: 0 }; this.history = []; this.boundary = null;
    this.data.set(null); this.events.set([]); this.busy.set(false); this.error.set(''); this.authRequired.set(false); this.canBack.set(false);
  }
  ngOnDestroy(): void { this.clear(); }
  private fetch(cursor: EventCursor = this.cursor): void {
    const scope = this.scope; if (!scope || this.busy()) return;
    const generation = ++this.generation; this.cursor = { ...cursor };
    this.data.set(null); this.events.set([]); this.busy.set(true); this.error.set('');
    this.request = this.api.readableUnit(scope.file.actor, scope.file.proposal, scope.file.snapshot, scope.file.file.id,
      scope.side, scope.unit.index, cursor, 32768).pipe(timeout(30_000)).subscribe({
      next: response => {
        if (generation !== this.generation || this.scope !== scope) return;
        this.busy.set(false);
        const events = validUnitResponse(response, scope, cursor), window = response?.window;
        if (events == null || response.state === 'READY' && this.boundary && (!window || JSON.stringify([stackIdentity(window.start_stack), window.start_ordered_lists]) !== this.boundary)) {
          this.error.set('מקטע התוכן אינו תואם ליחידה ולגרסה שנבחרו. אפשר לבדוק שוב את אותו מקטע.'); return;
        }
        this.boundary = null; this.events.set(events); this.data.set(response);
      },
      error: error => {
        if (generation !== this.generation || this.scope !== scope) return;
        this.busy.set(false);
        this.authRequired.set(error instanceof HttpErrorResponse && (error.status === 401 || error.error?.error?.code === 'SESSION_IDENTITY_CHANGED'));
        const message: unknown = error instanceof HttpErrorResponse ? error.error?.error?.message_he : null;
        this.error.set(typeof message === 'string' && message.length <= 2000 ? message : 'לא ניתן לקרוא כרגע את התוכן. אפשר לבדוק שוב את אותו מקטע.');
      },
    });
  }
}
