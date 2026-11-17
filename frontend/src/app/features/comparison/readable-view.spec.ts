import { TestBed } from '@angular/core/testing';
import { signal } from '@angular/core';
import { By } from '@angular/platform-browser';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { SessionState } from '../../core/auth/session-state';
import { ReadableView } from './readable-view';
import { ReadableSide } from './readable-side';
import { readableResponse, readableScope, unitResponse, unitScope } from './readable.testing';
import { repository } from './comparison.testing';

// The production view creates DOM through the existing inert GFM renderer.
describe('Readable before and after view', () => {
  async function create(added = false) {
    const source = readableScope(), response = readableResponse();
    if (added) {
      source.file.before = null; source.file.status = 'added'; source.file.matched_by = null; response.file = source.file;
      response.page.before = { state: 'ABSENT', sha256: null, document_id: null, source_bytes: 0, units_total: 0, error_code: null };
      for (const row of response.page.rows) {
        row.before = null; Object.assign(row.change, { before_index: null, status: 'added', matched_by: null, raw_changed: true,
          render_changed: row.after?.semantic_sha256 != null ? true : null, position_changed: false });
      }
    }
    const refresh = vi.fn();
    await TestBed.configureTestingModule({ imports: [ReadableView], providers: [provideHttpClient(), provideHttpClientTesting(),
      { provide: SessionState, useValue: { profile: signal({ user_id: readableScope().actor }), refresh } }],
    }).compileComponents();
    const fixture = TestBed.createComponent(ReadableView), view = fixture.componentInstance, http = TestBed.inject(HttpTestingController);
    fixture.componentRef.setInput('scope', source); fixture.componentRef.setInput('repository', repository);
    const settle = () => { fixture.detectChanges(); TestBed.tick(); fixture.detectChanges(); };
    settle(); http.expectOne(req => req.url.endsWith('/readable')).flush(response); settle();
    function select(kind: string) {
      const row = view.state.data()!.page.rows.find(row => (row.after ?? row.before)?.kind === kind)!;
      view.state.select(row.change.id); settle();
      for (const side of ['before', 'after'] as const) {
        if (!row[side]) continue;
        const scope = unitScope(kind, side); scope.file = source;
        http.expectOne(req => req.url.endsWith(`/units/${row[side]!.index}`) && req.params.get('side') === side).flush(unitResponse(scope));
      }
      settle();
    }
    const sides = () => fixture.debugElement.queryAll(By.directive(ReadableSide)).map(element => element.componentInstance as ReadableSide);
    const destroy = () => { fixture.destroy(); http.verify(); };
    return { fixture, view, http, select, settle, sides, destroy, refresh };
  }
  it('renders before and after as safe formatted paragraphs, links and headings', async () => {
    const c = await create(); c.select('paragraph');
    const renderers = c.fixture.nativeElement.querySelectorAll('app-document-renderer');
    expect(renderers).toHaveLength(2); expect(renderers[0].textContent).toContain('Original'); expect(renderers[1].textContent).toContain('Changed');
    expect(renderers[0].querySelector('strong').textContent).toBe('bold');
    expect(renderers[0].querySelector('a').getAttribute('rel')).toContain('noopener');
    c.select('heading'); expect(c.fixture.nativeElement.querySelector('app-document-renderer h2').textContent).toBe('כותרת'); c.destroy();
  });
  it('preserves list numbering and code language through bounded continuation windows and renders GFM tables', async () => {
    const c = await create(); c.select('list');
    expect(c.fixture.nativeElement.querySelector('ol').start).toBe(3); c.sides()[0].state.next(); c.settle();
    const scope = unitScope('list'); c.http.expectOne(req => req.url.endsWith(`/units/${scope.unit.index}`)).flush(unitResponse(scope, 1)); c.settle();
    expect(c.fixture.nativeElement.querySelector('ol').start).toBe(3); expect(c.fixture.nativeElement.textContent).toContain('המשך תוכן');
    c.select('code'); expect(c.fixture.nativeElement.querySelector('pre code')).not.toBeNull();
    expect(c.fixture.nativeElement.textContent).toContain('rust'); c.select('table');
    expect(c.fixture.nativeElement.querySelectorAll('table')).toHaveLength(2);
    expect(c.fixture.nativeElement.querySelector('th').textContent).toBe('Left'); c.destroy();
  });
  it('keeps protected HTML inert and gives an explicit route to raw source', async () => {
    const c = await create(); const raw = vi.fn(); c.view.rawRequested.subscribe(raw); c.select('protected');
    expect(c.fixture.nativeElement.querySelector('script')).toBeNull();
    expect(c.fixture.nativeElement.textContent).toContain('דורשת צפייה במקור הגולמי');
    const buttons = Array.from(c.fixture.nativeElement.querySelectorAll('button')) as HTMLButtonElement[];
    buttons.find(button => button.textContent?.includes('צפייה במקור הגולמי'))!.click(); expect(raw).toHaveBeenCalledOnce(); c.destroy();
  });
  it('cancels both private sides on scope loss and hides stale content before another file can render', async () => {
    const c = await create(); c.select('code'); c.sides()[0].state.next(); const request = c.http.expectOne(req => req.url.includes('/units/'));
    c.fixture.componentRef.setInput('scope', null); c.settle(); expect(request.cancelled).toBe(true);
    expect(c.fixture.nativeElement.querySelector('app-document-renderer')).toBeNull(); expect(c.view.state.data()).toBeNull(); c.destroy();
  });
  it('shows an absent before side explicitly and never requests invented content for a new document', async () => {
    const c = await create(true); c.select('paragraph');
    expect(c.fixture.nativeElement.textContent).toContain('היחידה אינה קיימת בצד זה');
    expect(c.fixture.nativeElement.querySelectorAll('app-document-renderer')).toHaveLength(1);
    c.http.expectNone(req => req.params.get('side') === 'before'); c.destroy();
  });

  it('emphasizes changed words only after both complete paired units arrive and clears them on selection or scope loss', async () => {
    const c = await create(); c.select('paragraph');
    const element: HTMLElement = c.fixture.nativeElement;
    expect([...element.querySelectorAll('del.diff-removed')].map(part => part.textContent).join('')).toBe('Original');
    expect([...element.querySelectorAll('ins.diff-added')].map(part => part.textContent).join('')).toBe('Changed');
    expect(element.querySelector('strong del, strong ins')).toBeNull();
    expect(element.textContent).toContain('− לפני השינוי'); expect(element.textContent).toContain('+ אחרי השינוי');
    c.select('list'); expect(element.querySelector('ins.diff-added, del.diff-removed')).toBeNull();
    c.select('heading'); expect(element.querySelector('ins.diff-added')?.textContent).toBe('חדשה');
    c.fixture.componentRef.setInput('scope', null); c.settle(); expect(element.querySelector('ins.diff-added, del.diff-removed')).toBeNull(); c.destroy();
  });

  it('does not reuse a visible window or word emphasis across actors even for the same source hashes', async () => {
    const c = await create(); c.select('paragraph');
    const scope = readableScope(); scope.actor = '00000000-0000-4000-8000-000000000099';
    c.fixture.componentRef.setInput('scope', scope); c.settle();
    expect(c.fixture.nativeElement.querySelector('app-document-renderer')).toBeNull();
    const metadata = c.http.expectOne(req => req.url.endsWith('/readable'));
    expect(metadata.request.headers.get('X-Expected-Actor')).toBe(scope.actor); c.destroy();
  });

});
