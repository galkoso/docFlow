import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { Book, BookDocument } from '../../core/api/book-api';
import { BookState } from './book-state';

const repository = '00000000-0000-0000-0000-000000000001';
const sha = 'a'.repeat(40);
const book: Book = { repository_id: repository, snapshot_sha: sha, branch: 'main', updated_at: '2026-11-14T00:00:00Z',
  version_selected: false, findings: [], documents: [
    { document_id: 'D1', path: 'docs/one.md', title: 'ראשון', sections: [] },
    { document_id: 'D2', path: 'docs/two.md', title: 'שני', sections: [] },
  ] };
const document: BookDocument = { document_id: 'D1', snapshot_sha: sha, title: 'ראשון', blocks: [], render_events: [] };
const base = `/api/v1/repositories/${repository}`;

describe('Disposable pinned book state', () => {
  let state: BookState;
  let http: HttpTestingController;
  beforeEach(() => {
    TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting(), BookState] });
    state = TestBed.inject(BookState);
    http = TestBed.inject(HttpTestingController);
  });
  afterEach(() => { state.clear(); http.verify(); });

  function open(selected = book) {
    state.open();
    http.expectOne('/api/v1/repositories').flush({ repositories: [{ repository_id: repository, default_branch: 'main' }] });
    http.expectOne(`${base}/specification`).flush(selected);
  }

  it('pins each chapter to the metadata SHA and deduplicates concurrent loads', () => {
    open();
    state.loadDocument('D1');
    const request = http.expectOne(`${base}/documents/D1?commit=${sha}`);
    request.flush(document);
    state.loadDocument('D1');
    http.expectNone(`${base}/documents/D1?commit=${sha}`);
    state.loadDocument('D2');
    http.expectOne(`${base}/documents/D2?commit=${sha}`).flush({ ...document, document_id: 'D2' });
    expect(Object.keys(state.documents())).toEqual(['D1', 'D2']);
  });

  it('cancels old requests and clears private content on identity or selection reset', () => {
    open();
    const request = http.expectOne(`${base}/documents/D1?commit=${sha}`);
    state.clear();
    expect(request.cancelled).toBe(true);
    expect(state.book()).toBeNull();
    expect(state.documents()).toEqual({});
    expect(state.repositories()).toEqual([]);
    expect(state.loadingDocuments().size).toBe(0);
  });

  it('rejects mixed-version content and removes cached content if the session expires', () => {
    open();
    http.expectOne(`${base}/documents/D1?commit=${sha}`).flush({ ...document, snapshot_sha: 'b'.repeat(40) });
    expect(state.documents()['D1']).toBeUndefined();
    expect(state.documentErrors()['D1']).toContain('גרסה שונה');
    state.loadDocument('D1');
    http.expectOne(`${base}/documents/D1?commit=${sha}`).flush(document);
    state.loadDocument('D2');
    http.expectOne(`${base}/documents/D2?commit=${sha}`).flush({}, { status: 401, statusText: 'Unauthorized' });
    expect(state.book()).toBeNull();
    expect(state.documents()).toEqual({});
    expect(state.error()).toContain('החיבור פג');
  });

  it('keeps an explicit inaccessible repository selected rather than showing another one', () => {
    state.open('foreign', sha);
    http.expectOne('/api/v1/repositories').flush({ repositories: [{ repository_id: repository, default_branch: 'main' }] });
    http.expectOne(`/api/v1/repositories/foreign/specification?commit=${sha}`).flush(
      { error: { message_he: 'המאגר לא נמצא' } }, { status: 404, statusText: 'Not Found' });
    http.expectNone(`${base}/specification`);
    expect(state.book()).toBeNull();
    expect(state.error()).toBe('המאגר לא נמצא');
  });

  it('bounds requests to two and cached chapters to eight while retaining measured placeholders', () => {
    const documents = Array.from({ length: 10 }, (_, index) => ({ ...book.documents[0], document_id: `D${index + 1}` }));
    open({ ...book, documents });
    for (let index = 1; index <= 9; index++) state.viewport(`D${index}`, true);
    const initial = http.match(request => request.url.includes('/documents/'));
    expect(initial.map(request => request.request.url.split('/').at(-1))).toEqual(['D1', 'D2']);
    initial[0].flush(document);
    initial[1].flush({ ...document, document_id: 'D2' });
    for (let index = 3; index <= 8; index++) {
      http.expectOne(`${base}/documents/D${index}?commit=${sha}`).flush({ ...document, document_id: `D${index}` });
    }
    http.expectNone(`${base}/documents/D9?commit=${sha}`);
    expect(Object.keys(state.documents())).toHaveLength(8);
    state.measure('D2', 1200);
    state.viewport('D2', false);
    http.expectOne(`${base}/documents/D9?commit=${sha}`).flush({ ...document, document_id: 'D9' });
    expect(state.documents()['D2']).toBeUndefined();
    expect(state.documents()['D9']).toBeDefined();
    expect(state.heights()['D2']).toBe(1200);
    expect(Object.keys(state.documents())).toHaveLength(8);
    state.clear();
    expect(state.heights()).toEqual({});
  });
});
