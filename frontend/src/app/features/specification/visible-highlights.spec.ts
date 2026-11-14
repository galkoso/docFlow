import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { Highlight, MatchWindow } from '../../core/api/book-api';
import { VisibleHighlights } from './visible-highlights';
import { textChunks } from './render-model';

const context = { repository: 'repo', commit: 'a'.repeat(40), document: 'D1' };
const hit: Highlight = { match_id: 'b'.repeat(64), block_id: 'B0', start_utf16: 0, end_utf16: 5 };
const window = (index: number): MatchWindow => ({ block_id: `B${index}`, start_utf16: 0, end_utf16: 1024 });

describe('Visible highlight windows', () => {
  let state: VisibleHighlights;
  let http: HttpTestingController;
  beforeEach(() => {
    vi.useFakeTimers();
    TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting(), VisibleHighlights] });
    state = TestBed.inject(VisibleHighlights);
    http = TestBed.inject(HttpTestingController);
    state.configure(context, 'מחיקה');
  });
  afterEach(() => { state.ngOnDestroy(); http.verify(); vi.useRealTimers(); });
  function request() { return http.expectOne(req => req.url.endsWith('/D1/highlights')); }
  function reply(matches: Highlight[] = []) { return { snapshot_sha: context.commit, document_id: 'D1', query: 'מחיקה', matches }; }
  function windows(request: { request: { params: { get(name: string): string | null } } }): MatchWindow[] {
    return JSON.parse(atob(request.request.params.get('windows')!.replaceAll('-', '+').replaceAll('_', '/')));
  }

  it('debounces, batches at most 32 windows and keeps a single request outstanding', () => {
    state.viewports(Array.from({ length: 33 }, (_, index) => ({ window: window(index), near: true })));
    vi.advanceTimersByTime(199);
    http.expectNone(req => req.url.endsWith('/highlights'));
    vi.advanceTimersByTime(1);
    const first = request();
    expect(windows(first)).toHaveLength(32);
    expect(first.request.params.get('commit')).toBe(context.commit);
    vi.advanceTimersByTime(1000);
    http.expectNone(req => req.url.endsWith('/highlights'));
    first.flush(reply([hit]));
    vi.advanceTimersByTime(50);
    const second = request();
    expect(windows(second)).toEqual([window(32)]);
    const other = { ...hit, block_id: 'B32', match_id: 'c'.repeat(64) };
    second.flush(reply([other]));
    expect(state.hits()).toEqual([hit, other]);
    state.viewport(window(0), false);
    expect(state.hits()).toEqual([other]);
    state.viewport(window(0), true);
    vi.advanceTimersByTime(200);
    http.expectNone(req => req.url.endsWith('/highlights'));
    expect(state.hits()).toHaveLength(2);
  });

  it('cancels on query/version changes and clears private data on destruction', () => {
    state.viewport(window(0), true);
    vi.advanceTimersByTime(200);
    const old = request();
    state.configure(context, 'new');
    expect(old.cancelled).toBe(true);
    vi.advanceTimersByTime(200);
    const next = request();
    expect(next.request.params.get('q')).toBe('new');
    next.flush({ ...reply([hit]), query: 'new' });
    expect(state.hits()).toEqual([hit]);
    state.configure({ ...context, commit: 'd'.repeat(40) }, 'new');
    expect(state.hits()).toEqual([]);
    vi.advanceTimersByTime(200);
    const version = request();
    expect(version.request.params.get('commit')).toBe('d'.repeat(40));
    state.ngOnDestroy();
    expect(version.cancelled).toBe(true);
    expect(state.hits()).toEqual([]);
  });

  it('rejects mismatched results and reports denied access without keeping highlights', () => {
    state.viewport(window(0), true);
    vi.advanceTimersByTime(200);
    request().flush({ ...reply([hit]), snapshot_sha: 'd'.repeat(40) });
    expect(state.hits()).toEqual([]);
    expect(state.error()).toContain('אינם תואמים');
    state.retry();
    vi.advanceTimersByTime(50);
    request().flush({}, { status: 404, statusText: 'Not Found' });
    expect(state.denied()).toBe(true);
    expect(state.hits()).toEqual([]);
  });

  it('retains at most 128 offscreen windows and cleans marks for an empty query', () => {
    const entries = Array.from({ length: 129 }, (_, index) => ({ window: window(index), near: true }));
    state.viewports(entries);
    vi.advanceTimersByTime(200);
    for (let index = 0; index < 5; index++) { request().flush(reply()); vi.advanceTimersByTime(50); }
    state.viewports(entries.map(entry => ({ ...entry, near: false })));
    state.viewports([{ window: window(0), near: true }, { window: window(128), near: true }]);
    vi.advanceTimersByTime(50);
    const evicted = request();
    expect(windows(evicted)).toEqual([window(0)]);
    evicted.flush(reply([hit]));
    expect(state.hits()).toEqual([hit]);
    state.configure(context, '');
    expect(state.hits()).toEqual([]);
    vi.advanceTimersByTime(500);
    http.expectNone(req => req.url.endsWith('/highlights'));
  });
});

describe('Grapheme-safe text windows', () => {
  it('preserves text and UTF-16 positions across emoji, niqqud and large graphemes', () => {
    const text = 'א'.repeat(1023) + '👩‍👩‍👧‍👦' + 'מְחִיקָה'.repeat(200);
    const chunks = textChunks({ kind: 'text', block_id: 'B1', offset_utf16: 7, text, source_start_byte: 0, source_end_byte: 1 });
    expect(chunks.map(chunk => chunk.event.text).join('')).toBe(text);
    expect(chunks[0].event.text).toBe('א'.repeat(1023));
    expect(chunks[1].event.text?.startsWith('👩‍👩‍👧‍👦')).toBe(true);
    let offset = 7;
    for (const chunk of chunks) {
      expect(chunk.event.offset_utf16).toBe(offset);
      expect(chunk.window!.end_utf16 - chunk.window!.start_utf16).toBeLessThanOrEqual(1024);
      offset += chunk.event.text!.length;
    }
    const giant = textChunks({ kind: 'text', block_id: 'B1', offset_utf16: 0, text: 'א' + '\u05b0'.repeat(40000), source_start_byte: 0, source_end_byte: 1 });
    expect(giant).toHaveLength(1);
    expect(giant[0].window?.end_utf16).toBe(1024);
    expect(giant[0].event.text).toHaveLength(40001);
  });
});
