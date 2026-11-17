import { ChangeDetectionStrategy, Component, computed, effect, inject, input, output, untracked } from '@angular/core';
import { SessionState } from '../../core/auth/session-state';
import { AssetContext, TextChange } from '../specification/render-model';
import { DocumentRenderer } from '../specification/document-renderer';
import { ReadableUnitState } from './readable-unit-state';
import { UnitScope } from './readable-window';

@Component({
  selector: 'app-readable-side', imports: [DocumentRenderer], providers: [ReadableUnitState], changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    @if (!scope()) {<p>היחידה אינה קיימת בצד זה.</p>}
    @if (state.busy()) {<p role="status">טוענים תוכן…</p>}
    @if (state.error()) {<p role="alert">{{ state.error() }}</p>
      @if (state.authRequired()) {<button (click)="session.refresh()">בדיקת התחברות מחדש</button>}
      @else {<button (click)="state.retry()">בדיקה נוספת של אותו מקטע</button>}
    }
    @if (current(); as result) {
      @if (result.state === 'RAW_REQUIRED') {<p>יחידה זו דורשת צפייה במקור הגולמי. <code>{{ result.error_code }}</code></p>
        <button (click)="rawRequested.emit()">צפייה במקור הגולמי</button>
      } @else if (result.window; as window) {
        @if (window.cursor.event || window.cursor.offset) {<p class="continuation">המשך תוכן מאותה יחידה</p>}
        <app-document-renderer [events]="state.events()" [changes]="changes()" [assetContext]="assetContext()"
          [sectionPrefix]="'diff-' + result.side + '-' + result.unit_index + '-'" />
        @for (code of window.code_info; track code.event_index) {<p>שפת קוד: <bdi>{{ code.info || 'ללא ציון שפה' }}</bdi></p>}
        @if (window.next) {<p>התוכן ממשיך במקטע הבא.</p>}
      }
    }
    @if (scope()) {<div class="actions"><button (click)="state.back()" [disabled]="state.busy() || !state.canBack()">מקטע קודם</button>
      <button (click)="state.next()" [disabled]="state.busy() || !state.data()?.window?.next">מקטע הבא</button></div>}
  `,
  styles: `:host{display:block;min-width:0}.actions{display:flex;gap:.5rem;flex-wrap:wrap;margin-block:1rem}
    button{padding:.5rem;cursor:pointer}button:focus-visible{outline:3px solid #275b9b;outline-offset:3px}.continuation{font-style:italic}`,
})
export class ReadableSide {
  readonly scope = input<UnitScope | null>(null);
  readonly assetContext = input<AssetContext | null>(null);
  readonly changes = input<TextChange[]>([]);
  readonly rawRequested = output<void>();
  readonly state = inject(ReadableUnitState);
  readonly session = inject(SessionState);
  readonly current = computed(() => {
    const scope = this.scope(), response = this.state.data(); return scope && this.state.owns(scope) ? response : null;
  });
  constructor() { effect(() => { const scope = this.scope(); untracked(() => this.state.open(scope)); }); }
}
