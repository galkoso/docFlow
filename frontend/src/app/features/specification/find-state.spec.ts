import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { SearchPage } from '../../core/api/book-api';
import { BookState } from './book-state';
import { FindState } from './find-state';

const sha = 'a'.repeat(40);
const query = 'מחיקה';
function page(start: number, total = 501, q = query): SearchPage {
  return { query: q, snapshot_sha: sha, start_at: start, total_matches: total,
    matches: Array.from({ length: Math.min(100, Math.max(0, total - start)) }, (_, index) => ({
      match_id: `m${start + index}`, document_id: `D${start + index}`, section_id: 'S1', block_id: 'B1',
      start_utf16: 0, end_utf16: 5, excerpt: query, book_order: start + index,
    })) };
}

describe('Snapshot-bound find controller', () => {
  let find: FindState;
  let http: HttpTestingController;
  beforeEach(() => {
    vi.useFakeTimers();
    TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting(), BookState, FindState] });
    find = TestBed.inject(FindState);
    http = TestBed.inject(HttpTestingController);
    find.scope('repository', sha);
  });
  afterEach(() => { find.clear(); http.verify(); vi.restoreAllMocks(); vi.useRealTimers(); });
  function request(start = 0, q = query) {
    return http.expectOne(req => req.url === '/api/v1/repositories/repository/find'
      && req.params.get('q') === q && req.params.get('commit') === sha && req.params.get('start_at') === String(start));
  }
  function initial() { find.search(query); vi.advanceTimersByTime(200); request().flush(page(0)); }

  it('restarts search when proposal identity changes at the same repository and SHA', () => {
    find.scope('repository', sha, 'p1');
    find.search(query); vi.advanceTimersByTime(200);
    const pending = http.expectOne(req => req.url === '/api/v1/proposals/p1/find');
    find.scope('repository', sha, 'p2');
    expect(pending.cancelled).toBe(true);
    expect(find.total()).toBeNull();
    vi.advanceTimersByTime(200);
    http.expectOne(req => req.url === '/api/v1/proposals/p2/find').flush(page(0));
    expect(find.total()).toBe(501);
    find.scope('repository', sha);
    expect(find.hits()).toEqual([]);
    vi.advanceTimersByTime(200); request().flush(page(0, 0));
    expect(find.total()).toBe(0);
  });

  function pending(commit = sha) {
    request().flush({ error: { code: 'SEARCH_SNAPSHOT_NOT_READY', retryable: true,
      details: { snapshot_sha: commit }, message_he: 'החיפוש נמצא בהכנה' } }, { status: 409, statusText: 'Conflict' });
  }

  it('polls a pending pinned index until ready without reporting zero matches', () => {
    vi.spyOn(document, 'visibilityState', 'get').mockReturnValue('visible');
    find.search(query);
    vi.advanceTimersByTime(200);
    pending();
    expect(find.total()).toBeNull();
    expect(find.indexing()).toBe(true);
    expect(find.error()).toBeNull();
    vi.advanceTimersByTime(1999);
    http.expectNone(req => req.url.endsWith('/find'));
    vi.advanceTimersByTime(1);
    request().flush(page(0));
    expect(find.indexing()).toBe(false);
    expect(find.total()).toBe(501);
    vi.advanceTimersByTime(10000);
    http.expectNone(req => req.url.endsWith('/find'));
  });

  it('slows hidden-tab polling and cancels it on query and version changes', () => {
    vi.spyOn(document, 'visibilityState', 'get').mockReturnValue('hidden');
    find.search(query);
    vi.advanceTimersByTime(200);
    pending();
    vi.advanceTimersByTime(9999);
    http.expectNone(req => req.url.endsWith('/find'));
    vi.advanceTimersByTime(1);
    pending();
    find.search('other');
    vi.advanceTimersByTime(200);
    request(0, 'other').flush(page(0, 0, 'other'));
    vi.advanceTimersByTime(10000);
    http.expectNone(req => req.url.endsWith('/find'));
    find.search(query);
    vi.advanceTimersByTime(200);
    pending();
    find.scope();
    vi.advanceTimersByTime(10000);
    http.expectNone(req => req.url.endsWith('/find'));
    expect(find.indexing()).toBe(false);
  });

  it('stops on terminal or mismatched progress and bounds automatic checks', () => {
    vi.spyOn(document, 'visibilityState', 'get').mockReturnValue('visible');
    find.search(query);
    vi.advanceTimersByTime(200);
    pending('b'.repeat(40));
    vi.advanceTimersByTime(10000);
    http.expectNone(req => req.url.endsWith('/find'));
    find.retry();
    request().flush({ error: { code: 'SEARCH_SNAPSHOT_FAILED', retryable: false, message_he: 'ההכנה נכשלה' } },
      { status: 409, statusText: 'Conflict' });
    vi.advanceTimersByTime(10000);
    http.expectNone(req => req.url.endsWith('/find'));
    expect(find.error()).toContain('נכשלה');
    find.retry();
    for (let attempt = 0; attempt < 90; ++attempt) { pending(); vi.advanceTimersByTime(2000); }
    pending();
    expect(find.loading()).toBe(false);
    expect(find.error()).toContain('זמן רב');
    expect(find.total()).toBeNull();
    vi.advanceTimersByTime(10000);
    http.expectNone(req => req.url.endsWith('/find'));
    find.retry();
    request().flush(page(0));
    expect(find.total()).toBe(501);
  });

  it('debounces changes, cancels superseded queries and clears hits when the snapshot changes', () => {
    find.search('old');
    vi.advanceTimersByTime(199);
    http.expectNone(req => req.url.endsWith('/find'));
    vi.advanceTimersByTime(1);
    const old = request(0, 'old');
    find.search(query);
    expect(old.cancelled).toBe(true);
    vi.advanceTimersByTime(200);
    request().flush(page(0));
    expect(find.total()).toBe(501);
    expect(find.current()?.match_id).toBe('m0');
    find.scope('repository', 'b'.repeat(40));
    expect(find.total()).toBeNull();
    expect(find.hits()).toEqual([]);
    vi.advanceTimersByTime(200);
    const next = http.expectOne(req => req.params.get('commit') === 'b'.repeat(40));
    find.clear();
    expect(next.cancelled).toBe(true);
  });

  it('wraps directly to the last result and retains at most three pages', () => {
    initial();
    find.move(-1);
    expect(find.position()).toBe(500);
    request(500).flush(page(500));
    expect(find.current()?.match_id).toBe('m500');
    expect(find.announcement()).toContain('האחרונה');
    find.move(1);
    expect(find.current()?.match_id).toBe('m0');
    expect(find.announcement()).toContain('הראשונה');
    for (const start of [100, 200, 300]) { find.select(start); request(start).flush(page(start)); }
    expect(find.pages().size).toBe(3);
    expect(find.hits()).toHaveLength(300);
    expect(find.total()).toBe(501);
    find.select(0);
    request(0).flush(page(0));
    expect(find.current()?.match_id).toBe('m0');
  });

  it('cancels an outstanding page when navigation returns to a cached page', () => {
    initial();
    find.move(-1);
    const last = request(500);
    find.move(1);
    expect(last.cancelled).toBe(true);
    expect(find.loading()).toBe(false);
    expect(find.current()?.match_id).toBe('m0');
  });

  it('rejects mismatched snapshot responses and preserves explicit search failures', () => {
    find.search(query);
    vi.advanceTimersByTime(200);
    request().flush({ ...page(0), snapshot_sha: 'b'.repeat(40) });
    expect(find.total()).toBeNull();
    expect(find.hits()).toEqual([]);
    expect(find.error()).toContain('אינן תואמות');
    find.retry();
    request().flush({ error: { message_he: 'החיפוש בגרסה זו אינו מוכן' } }, { status: 409, statusText: 'Conflict' });
    expect(find.total()).toBeNull();
    expect(find.error()).toContain('אינו מוכן');
  });

  it('clears private book and search data on denied access, and limits query code points', () => {
    const book = TestBed.inject(BookState);
    const clear = vi.spyOn(book, 'clear');
    initial();
    find.move(-1);
    request(500).flush({}, { status: 404, statusText: 'Not Found' });
    expect(clear).toHaveBeenCalledOnce();
    expect(find.hits()).toEqual([]);
    expect(find.query()).toBe('');
    find.scope('repository', sha);
    find.search('😀'.repeat(201));
    vi.advanceTimersByTime(200);
    http.expectNone(req => req.url.endsWith('/find'));
    expect(find.error()).toContain('200');
    find.search('😀'.repeat(200));
    vi.advanceTimersByTime(200);
    request(0, '😀'.repeat(200)).flush(page(0, 0, '😀'.repeat(200)));
    expect(find.total()).toBe(0);
  });
});
