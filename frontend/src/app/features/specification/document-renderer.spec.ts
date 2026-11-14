import { TestBed } from '@angular/core/testing';
import { DocumentRenderer } from './document-renderer';
import { externalLink, renderTree, RenderEvent, SearchHit } from './render-model';

const event = (kind: RenderEvent['kind'], values: Partial<RenderEvent> = {}): RenderEvent =>
  ({ kind, source_start_byte: 0, source_end_byte: 1, ...values });
const open = (tag: RenderEvent['tag'], values: Partial<RenderEvent> = {}) => event('open', { tag, ...values });
const close = (tag: RenderEvent['tag']) => event('close', { tag });
const text = (value: string, offset = 0) => event('text', { text: value, block_id: 'b1', offset_utf16: offset });

describe('Canonical document renderer', () => {
  beforeEach(() => TestBed.configureTestingModule({ imports: [DocumentRenderer] }));

  it('creates native table, list and code elements without component wrappers', async () => {
    const fixture = TestBed.createComponent(DocumentRenderer);
    fixture.componentRef.setInput('events', [
      open('table'), open('table_head'), open('table_cell'), text('כותרת'), close('table_cell'), close('table_head'),
      open('table_row'), open('table_cell'), text('תא'), close('table_cell'), close('table_row'), close('table'),
      open('list', { list_start: 3 }), open('item'), text('פריט'), close('item'), close('list'),
      open('code_block'), text('<script>alert(1)</script>'), close('code_block'),
    ]);
    await fixture.whenStable();
    const element: HTMLElement = fixture.nativeElement;
    expect(element.querySelector('table > thead > tr > th')?.textContent).toBe('כותרת');
    expect(element.querySelector('table > tr > td')?.textContent).toBe('תא');
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

  it('fails closed on malformed event nesting and excessive depth', async () => {
    expect(() => renderTree([open('paragraph'), close('table')])).toThrow('RENDER_UNBALANCED');
    expect(() => renderTree(Array.from({ length: 65 }, () => open('quote')))).toThrow('RENDER_DEPTH_LIMIT');
    const fixture = TestBed.createComponent(DocumentRenderer);
    fixture.componentRef.setInput('events', [open('paragraph')]);
    await fixture.whenStable();
    expect(fixture.nativeElement.querySelector('[role=alert]')?.textContent).toContain('מבנה התוכן אינו תקין');
  });
});
