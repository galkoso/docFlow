import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import type { ReadableFileResponse } from '../../core/api/comparison-api';
import { id } from './comparison.testing';
import { readableResponse, readableScope } from './readable.testing';
import { ReadableState } from './readable-state';

describe('Bounded readable metadata', () => {
  let state: ReadableState, http: HttpTestingController;
  beforeEach(() => {
    TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting(), ReadableState] });
    state = TestBed.inject(ReadableState); http = TestBed.inject(HttpTestingController);
  });
  afterEach(() => { state.clear(); http.verify(); });
  const request = () => http.expectOne(req => req.url.endsWith('/readable'));
  function open(): void { state.open(readableScope()); request().flush(readableResponse()); }
  it('accepts the Rust-generated unit pairing without downloading any source/event body', () => {
    const scope = readableScope(); state.open(scope); const first = request();
    expect(first.request.headers.get('X-Expected-Actor')).toBe(scope.actor); expect(first.request.params.get('limit')).toBe('50');
    first.flush(readableResponse()); expect(state.error()).toBe(''); expect(state.data()?.page.rows.length).toBeGreaterThan(1);
    const row = state.data()!.page.rows[1]; state.select(row.change.id); expect(state.selected()?.change.id).toBe(row.change.id);
    state.open(readableScope()); http.expectNone(req => req.url.includes('/diff/files/'));
  });
  it('rejects mixed file/side evidence and false change, cursor or unit claims', () => {
    const corrupt: Array<(value: ReadableFileResponse) => void> = [
      v => v.snapshot_id = id(8), v => v.page.before.sha256 = '9'.repeat(64), v => v.page.after.source_bytes++,
      v => v.page.rows[1].change.raw_changed = !v.page.rows[1].change.raw_changed,
      v => v.page.rows[1].before!.source_end_byte = Number.MAX_SAFE_INTEGER,
      v => v.page.rows[1].change.before_index = 99, v => v.page.rows[1].change.id = v.page.rows[0].change.id,
      v => v.page.next_offset = 0, v => v.page.rows.pop(), v => v.page.before.units_total = 0,
    ];
    for (const change of corrupt) {
      state.clear(); state.open(readableScope()); const response = readableResponse(); change(response); request().flush(response);
      expect(state.data()).toBeNull(); expect(state.error()).not.toBe('');
    }
  });
  it('keeps an unavailable projection explicit instead of treating it as no changes', () => {
    state.open(readableScope()); const value = readableResponse();
    Object.assign(value.page.before, { state: 'RAW_REQUIRED', document_id: null, units_total: 0, error_code: 'READABLE_EVENT_LIMIT' });
    Object.assign(value.page, { ready: false, total: 0, rows: [], next_offset: null }); request().flush(value);
    expect(state.error()).toBe(''); expect(state.data()?.page.ready).toBe(false); expect(state.data()?.page.raw_changed).toBe(true);
  });
  it('pins immutable side metadata across observations and retries the same page after failures', () => {
    open(); const selected = state.data()!.page.rows[1]; state.select(selected.change.id);
    state.page(50); const page = request(); expect(page.request.params.get('offset')).toBe('50');
    page.error(new ProgressEvent('error')); expect(state.data()).toBeNull(); expect(state.selected()).toBeNull();
    state.retry(); const retry = request(); expect(retry.request.params.toString()).toBe(page.request.params.toString());
    const changed = readableResponse(); changed.page.offset = 50; changed.page.rows = []; changed.page.before.document_id = 'DOC-OTHER';
    changed.file.before!.document_id = 'DOC-OTHER'; retry.flush(changed); expect(state.data()).toBeNull();
  });
  it('cancels obsolete selections and clears private metadata after identity failure', () => {
    state.open(readableScope()); const first = request(); state.open(null); expect(first.cancelled).toBe(true);
    open(); state.select(state.data()!.page.rows[1].change.id); state.retry();
    request().flush({ error: { code: 'SESSION_IDENTITY_CHANGED' } }, { status: 409, statusText: 'Conflict' });
    expect(state.selected()).toBeNull(); expect(state.authRequired()).toBe(true); state.retry();
    http.expectNone(req => req.url.endsWith('/readable'));
  });
});
