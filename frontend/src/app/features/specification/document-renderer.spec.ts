import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { DocumentRenderer } from './document-renderer';
import { assetSource, externalLink, renderTree, RenderEvent, SearchHit } from './render-model';

const event = (kind: RenderEvent['kind'], values: Partial<RenderEvent> = {}): RenderEvent =>
  ({ kind, source_start_byte: 0, source_end_byte: 1, ...values });
const open = (tag: RenderEvent['tag'], values: Partial<RenderEvent> = {}) => event('open', { tag, ...values });
const close = (tag: RenderEvent['tag']) => event('close', { tag });
const text = (value: string, offset = 0) => event('text', { text: value, block_id: 'b1', offset_utf16: offset });

describe('Canonical document renderer', () => {
  beforeEach(() => TestBed.configureTestingModule({ imports: [DocumentRenderer], providers: [provideHttpClient(), provideHttpClientTesting()] }));

  it('creates native table, list and code elements without component wrappers', async () => {
    const fixture = TestBed.createComponent(DocumentRenderer);
    fixture.componentRef.setInput('events', [
      open('table', { alignments: ['center'] }), open('table_head'), open('table_cell'), text('כותרת'), close('table_cell'), close('table_head'),
      open('table_row'), open('table_cell'), text('תא'), close('table_cell'), close('table_row'), close('table'),
      open('list', { list_start: 3 }), open('item'), text('פריט'), close('item'), close('list'),
      open('code_block'), text('<script>alert(1)</script>'), close('code_block'),
    ]);
    await fixture.whenStable();
    const element: HTMLElement = fixture.nativeElement;
    expect(element.querySelector('table > thead > tr > th')?.textContent).toBe('כותרת');
    expect(element.querySelector('table > tr > td')?.textContent).toBe('תא');
    expect(element.querySelector('th')?.style.textAlign).toBe('center');
    expect(element.querySelector('td')?.style.textAlign).toBe('center');
    expect(element.querySelector('ol')?.start).toBe(3);
    expect(element.querySelector('pre > code')?.textContent).toContain('<script>');
    expect(element.querySelector('script')).toBeNull();
  });

  it('never creates unsafe links, raw HTML, or remote image requests', async () => {
    const fixture = TestBed.createComponent(DocumentRenderer);
    fixture.componentRef.setInput('events', [
      open('link', { destination: 'javascript:alert(1)' }), text('unsafe'), close('link'),
      open('link', { destination: 'https://example.org/doc' }), text('safe'), close('link'),
      event('image', { destination: 'https://tracker.example/pixel', text: '<img onerror=alert(1)>' }),
      event('unsupported'),
    ]);
    await fixture.whenStable();
    const element: HTMLElement = fixture.nativeElement;
    expect(element.querySelectorAll('a')).toHaveLength(1);
    expect(element.querySelector('a')?.rel).toBe('noopener noreferrer');
    expect(element.querySelector('a')?.target).toBe('_blank');
    expect(element.querySelector('img, script, iframe')).toBeNull();
    expect(element.textContent).toContain('<img onerror=alert(1)>');
    expect(externalLink('https://user:password@example.org')).toBeNull();
    expect(externalLink(' https://example.org')).toBeNull();
  });

  it('highlights server UTF-16 ranges across formatting without splitting emoji or niqqud', async () => {
    const fixture = TestBed.createComponent(DocumentRenderer);
    const hit: SearchHit = { block_id: 'b1', document_id: 'd1', section_id: 's1', match_id: 'm1',
      start_utf16: 3, end_utf16: 12, book_order: 0, excerpt: '🙂 מְחיקה עכשיו' };
    fixture.componentRef.setInput('events', [open('paragraph'), text('🙂 מְ'), open('strong'), text('חיקה', 5), close('strong'),
      text(' עכשיו', 9), close('paragraph')]);
    fixture.componentRef.setInput('hits', [hit]);
    fixture.componentRef.setInput('activeMatch', 'm1');
    await fixture.whenStable();
    const element: HTMLElement = fixture.nativeElement;
    expect(element.querySelector('p')?.textContent).toBe('🙂 מְחיקה עכשיו');
    expect([...element.querySelectorAll('mark')].map(mark => mark.textContent).join('')).toBe('מְחיקה עכ');
    expect(element.querySelectorAll('mark.active')).toHaveLength(3);
  });

  it('loads only the authorized commit-pinned image proxy and exposes retry on failure', async () => {
    const fixture = TestBed.createComponent(DocumentRenderer);
    const context = { repository: '00000000-0000-0000-0000-000000000001', commit: 'a'.repeat(40) };
    const destination = `assets/${'b'.repeat(64)}.png`;
    fixture.componentRef.setInput('assetContext', context);
    fixture.componentRef.setInput('events', [event('image', { destination, text: 'תרשים הרשאות' })]);
    await fixture.whenStable();
    const element: HTMLElement = fixture.nativeElement;
    const image = element.querySelector('img')!;
    expect(image.getAttribute('src')).toBe(`/api/v1/repositories/${context.repository}/assets/${'b'.repeat(64)}.png?commit=${context.commit}`);
    expect(image.alt).toBe('תרשים הרשאות');
    expect(image.getAttribute('loading')).toBe('lazy');
    image.dispatchEvent(new Event('error'));
    await fixture.whenStable();
    expect(element.querySelector('img')).toBeNull();
    expect(element.textContent).toContain('לא ניתן לטעון את התמונה');
    element.querySelector('button')!.click();
    await fixture.whenStable();
    expect(element.querySelector('img')).not.toBeNull();
    fixture.componentRef.setInput('assetContext', { ...context, commit: 'c'.repeat(40) });
    await fixture.whenStable();
    expect(element.querySelector('img')?.getAttribute('src')).toContain(`commit=${'c'.repeat(40)}`);
    expect(assetSource(context, 'assets/../secret.png')).toBeNull();
    expect(assetSource(context, 'https://tracker.example/pixel')).toBeNull();
    expect(assetSource({ ...context, commit: 'main' }, destination)).toBeNull();
    const proposal = '00000000-0000-0000-0000-000000000002';
    expect(assetSource({ ...context, proposal }, destination))
      .toBe(`/api/v1/proposals/${proposal}/assets/${'b'.repeat(64)}.png?commit=${context.commit}`);
    expect(assetSource({ ...context, proposal: '../main' }, destination)).toBeNull();
  });

  it('fails closed on malformed event nesting and excessive depth', async () => {
    expect(() => renderTree([open('paragraph'), close('table')])).toThrow('RENDER_UNBALANCED');
    expect(() => renderTree(Array.from({ length: 65 }, () => open('quote')))).toThrow('RENDER_DEPTH_LIMIT');
    const fixture = TestBed.createComponent(DocumentRenderer);
    fixture.componentRef.setInput('events', [open('paragraph')]);
    await fixture.whenStable();
    expect(fixture.nativeElement.querySelector('[role=alert]')?.textContent).toContain('מבנה התוכן אינו תקין');
  });

  it('marks every returned visible match even when it is absent from retained navigation pages', async () => {
    let callback!: IntersectionObserverCallback;
    const observed = new Set<Element>();
    const disconnect = vi.fn();
    vi.stubGlobal('IntersectionObserver', class {
      constructor(value: IntersectionObserverCallback, options: IntersectionObserverInit) {
        callback = value; expect(options.rootMargin).toBe('300px 0px');
      }
      observe(element: Element) { observed.add(element); }
      disconnect = disconnect;
    });
    try {
      const fixture = TestBed.createComponent(DocumentRenderer);
      const http = TestBed.inject(HttpTestingController);
      const context = { repository: 'repository', commit: 'a'.repeat(40), document: 'D1' };
      fixture.componentRef.setInput('highlightContext', context);
      fixture.componentRef.setInput('query', 'מחיקה');
      fixture.componentRef.setInput('events', [open('paragraph'), text('🙂 מְ'), open('strong'), text('חיקה', 5), close('strong'),
        text(' מחיקה', 9), close('paragraph')]);
      await fixture.whenStable();
      expect(observed.size).toBe(3);
      callback([...observed].map(target => ({ target, isIntersecting: true }) as IntersectionObserverEntry), {} as IntersectionObserver);
      await new Promise(resolve => setTimeout(resolve, 220));
      const request = http.expectOne(req => req.url.endsWith('/D1/highlights'));
      const windows = JSON.parse(atob(request.request.params.get('windows')!.replaceAll('-', '+').replaceAll('_', '/')));
      expect(windows).toHaveLength(3);
      const matches = [{ match_id: 'b'.repeat(64), block_id: 'b1', start_utf16: 3, end_utf16: 9 },
        { match_id: 'c'.repeat(64), block_id: 'b1', start_utf16: 10, end_utf16: 15 }];
      request.flush({ snapshot_sha: context.commit, document_id: 'D1', query: 'מחיקה', matches });
      fixture.componentRef.setInput('activeMatch', matches[1].match_id);
      await fixture.whenStable();
      const element: HTMLElement = fixture.nativeElement;
      expect([...element.querySelectorAll('mark')].map(mark => mark.textContent).join('')).toBe('מְחיקהמחיקה');
      expect(element.querySelector('mark.active')?.textContent).toBe('מחיקה');
      expect(element.querySelector('strong > span > mark')?.textContent).toBe('חיקה');
      expect(fixture.componentInstance.hits()).toEqual([]);
      fixture.componentRef.setInput('query', '');
      await fixture.whenStable();
      expect(element.querySelector('mark')).toBeNull();
      fixture.destroy();
      expect(disconnect).toHaveBeenCalledOnce();
      http.verify();
    } finally { vi.unstubAllGlobals(); }
  });

  it('keeps line-break matches addressable and visibly distinct from plain text', async () => {
    const fixture = TestBed.createComponent(DocumentRenderer);
    fixture.componentRef.setInput('events', [open('paragraph'), text('alpha'),
      event('break', { text: '\n', block_id: 'b1', offset_utf16: 5, hard_break: true }), text('beta', 6), close('paragraph')]);
    fixture.componentRef.setInput('hits', [{ block_id: 'b1', document_id: 'D1', section_id: 'S1', match_id: 'line',
      start_utf16: 5, end_utf16: 6, book_order: 0, excerpt: 'alpha\nbeta' }]);
    fixture.componentRef.setInput('activeMatch', 'line');
    await fixture.whenStable();
    const element: HTMLElement = fixture.nativeElement;
    expect(element.querySelector('p')?.textContent).toBe('alpha\nbeta');
    expect(element.querySelectorAll('br')).toHaveLength(1);
    expect(element.querySelector('mark.active.line-break')?.getAttribute('data-match-id')).toBe('line');
  });
});
