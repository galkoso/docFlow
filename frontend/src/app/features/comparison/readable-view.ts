import { ChangeDetectionStrategy, Component, computed, effect, inject, input, output, untracked, viewChildren } from '@angular/core';
import { SessionState } from '../../core/auth/session-state';
import { AssetContext } from '../specification/render-model';
import { RawScope } from './raw-state';
import { sameFile, scopeKey } from './readable-evidence';
import { ReadableState } from './readable-state';
import { ReadableSide } from './readable-side';
import { UnitScope } from './readable-window';
import { wordChanges, WordChanges } from './word-changes';

@Component({
  selector: 'app-readable-view', imports: [ReadableSide], providers: [ReadableState], changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <h2>השוואה קריאה — <bdi>{{ scope()?.file?.after?.path ?? scope()?.file?.before?.path }}</bdi></h2>
    @if (state.busy()) {<p role="status">טוענים את יחידות המסמך…</p>}
    @if (state.error()) {<button (click)="rowSelected.emit(null)">תחילת רשימת היחידות</button><p role="alert">{{ state.error() }}</p>
      @if (state.authRequired()) {<button (click)="session.refresh()">בדיקת התחברות מחדש</button>}
      @else {<button (click)="state.retry()">בדיקה נוספת של אותה גרסה</button>}
    }
    @if (trusted()) {
    @if (state.data(); as result) {
      @if (!result.page.ready) {<p>לא ניתן להציג את כל המסמך בתצוגה קריאה. התוכן נשאר זמין במקור הגולמי.</p>
        <p>לפני: {{ result.page.before.error_code }} · אחרי: {{ result.page.after.error_code }}</p>
        <button (click)="rawRequested.emit()">צפייה במקור הגולמי</button>
      } @else {
        <nav aria-label="יחידות ההשוואה">
          @for (row of result.page.rows; track row.change.id) {
            <button class="unit" (click)="choose(row.change.id)" [attr.aria-pressed]="state.selected()?.change?.id === row.change.id">
              {{ label(row.after?.kind ?? row.before!.kind) }} · {{ label(row.change.status) }}
              @if (row.after?.heading_id ?? row.before?.heading_id; as heading) {<bdi>{{ heading }}</bdi>}
              <small>לפני {{ row.before ? row.before.index + 1 : 'לא קיים' }} · אחרי {{ row.after ? row.after.index + 1 : 'לא קיים' }}</small>
              @if (row.change.position_changed) {<span>מיקום היחידה השתנה</span>}
              @if (row.change.raw_changed && row.change.render_changed === false) {<span>המקור השתנה, התצוגה זהה</span>}
            </button>
          }
        </nav>
        <p>{{ result.page.rows.length }} יחידות מוצגות מתוך {{ result.page.total }}</p>
        <div class="actions"><button (click)="previous(result.page.offset)" [disabled]="state.busy() || !result.page.offset">יחידות קודמות</button>
          <button (click)="state.page(result.page.next_offset!)" [disabled]="state.busy() || result.page.next_offset == null">יחידות הבאות</button></div>
        @if (!result.page.total) {<p>אין יחידות להצגה.</p>}
      }
    }
    @if (state.selected(); as row) {
      <p>היחידה שנבחרה: {{ label(row.after?.kind ?? row.before!.kind) }} · {{ label(row.change.status) }}</p>
      @if (words()?.precise === false) {<p>מוצג שינוי ברמת היחידה. לא זוהתה התאמת מילים ברורה.</p>}
      <div class="pair"><section [class.removed]="row.change.status === 'modified' || row.change.status === 'deleted'">
        <h3>לפני — לקריאה בלבד @if (row.change.status === 'modified' || row.change.status === 'deleted') {<span>− {{ row.change.status === 'deleted' ? 'הוסר' : 'לפני השינוי' }}</span>}</h3><app-readable-side [scope]="before()" [changes]="words()?.before ?? []" [assetContext]="beforeAssets()"
          (rawRequested)="rawRequested.emit()" /></section>
        <section [class.added]="row.change.status === 'modified' || row.change.status === 'added'">
          <h3>אחרי @if (row.change.status === 'modified' || row.change.status === 'added') {<span>+ {{ row.change.status === 'added' ? 'נוסף' : 'אחרי השינוי' }}</span>}</h3><app-readable-side [scope]="after()" [changes]="words()?.after ?? []" [assetContext]="afterAssets()" (rawRequested)="rawRequested.emit()" /></section></div>
    }
    }
  `,
  styles: `:host{display:block;margin-block:1.5rem}.pair{display:grid;grid-template-columns:minmax(0,1fr) minmax(0,1fr);gap:1rem;direction:rtl}
    section.removed{background:#fff4f2}section.added{background:#f0fbf3}
    section{min-width:0;padding:.8rem;border:1px solid #c3cbd5}.unit{display:inline-flex;flex-wrap:wrap;gap:.5rem;margin:.2rem;padding:.6rem;text-align:start}
    .unit[aria-pressed=true]{outline:2px solid #275b9b}.actions{display:flex;gap:.5rem;flex-wrap:wrap}small{display:block}
    button{cursor:pointer;padding:.5rem}button:focus-visible{outline:3px solid #275b9b;outline-offset:3px}
    @media(max-width:650px){.pair{grid-template-columns:1fr}}`,
})
export class ReadableView {
  readonly scope = input<RawScope | null>(null);
  readonly repository = input<string | null>(null);
  readonly row = input<string | null>(null);
  readonly rowSelected = output<string | null>();
  readonly rawRequested = output<void>();
  readonly state = inject(ReadableState);
  private readonly sides = viewChildren(ReadableSide);
  readonly session = inject(SessionState);
  readonly trusted = computed(() => {
    const scope = this.scope(), response = this.state.data(); return !!scope && !!response && this.state.owns(scope) && sameFile(response, scope);
  });
  readonly words = computed<WordChanges | null>(() => {
    const row = this.state.selected(), file = this.scope();
    if (!this.trusted() || !file || row?.change.status !== 'modified') return null;
    const sides = this.sides();
    const complete = (side: 'before' | 'after') => {
      const expected = this.side(side), child = sides.find(child => child.scope()?.side === side);
      const response = child?.state.data(), window = response?.window;
      return expected && child && scopeKey(child.scope()!.file) === scopeKey(file) && child.state.owns(expected)
        && response?.state === 'READY' && window && window.cursor.event === 0 && window.cursor.offset === 0 && window.next == null ? child : null;
    };
    const before = complete('before'), after = complete('after');
    return before && after ? wordChanges(before.state.events(), after.state.events()) : null;
  });
  readonly before = computed(() => this.side('before'));
  readonly after = computed(() => this.side('after'));
  readonly beforeAssets = computed(() => this.assets('before'));
  readonly afterAssets = computed(() => this.assets('after'));
  constructor() { effect(() => { const scope = this.scope(), row = this.row(); untracked(() => this.state.open(scope, row)); }); }
  choose(id: string): void { this.state.select(id); this.rowSelected.emit(id); }
  previous(offset: number): void { this.state.page(Math.max(0, offset - 50)); }
  label(value: string): string {
    return ({ metadata: 'נתוני מסמך', heading: 'כותרת', paragraph: 'פסקה', list: 'רשימה', quote: 'ציטוט', code: 'קוד', table: 'טבלה',
      protected: 'מקור מוגן', trailing: 'מקור מסיים', unchanged: 'זהה', modified: 'השתנה', added: 'נוסף', deleted: 'הוסר' } as Record<string, string>)[value] ?? value;
  }
  private side(side: 'before' | 'after'): UnitScope | null {
    const file = this.scope(), unit = this.state.selected()?.[side]; return this.trusted() && file && unit ? { file, side, unit } : null;
  }
  private assets(side: 'before' | 'after'): AssetContext | null {
    const scope = this.scope(), repository = this.repository();
    return scope && repository ? { repository, proposal: scope.proposal, commit: side === 'before' ? scope.base : scope.head } : null;
  }
}
