import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import type { RawFileResponse } from '../../core/api/comparison-api';
import { actor, snapshot, id } from './comparison.testing';
import { RawState } from './raw-state';

import { rawScope, rawResponse } from './raw.testing';
function lastResponse(): RawFileResponse {
  const result = rawResponse(); result.raw.cursor = { chunk: 1, offset: 0 }; result.raw.next = null;
  result.raw.segments[0] = { ...result.raw.segments[0], kind: 'insert', chunk_index: 1,
    old_start_byte: 3, old_end_byte: 3, new_start_byte: 0, new_end_byte: 3, old_start_column_bytes: 3,
    old_text: '', new_text: 'new' }; return result;
}

describe('Private bounded raw windows', () => {
  let state: RawState, http: HttpTestingController;
  beforeEach(() => {
    TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting(), RawState] });
    state = TestBed.inject(RawState); http = TestBed.inject(HttpTestingController);
  });
  afterEach(() => { state.clear(); http.verify(); });
  const request = () => http.expectOne(req => req.url.includes('/diff/files/'));
  function open(): void { state.open(rawScope()); request().flush(rawResponse()); }

  it('captures actor and exact file identity, retaining only one bounded source window', () => {
    const scope = rawScope(); state.open(scope); scope.actor = id(9); scope.file.after!.path = 'docs/changed.md';
    const first = request(); expect(first.request.headers.get('X-Expected-Actor')).toBe(actor);
    expect(first.request.params.get('max_bytes')).toBe('32768'); first.flush(rawResponse());
    state.open(rawScope()); http.expectNone(req => req.url.includes('/diff/files/'));
    state.next(); expect(state.data()).toBeNull(); const next = request();
    expect(next.request.params.get('chunk')).toBe('1'); expect(next.request.params.get('snapshot')).toBe(snapshot);
    next.flush(lastResponse()); expect(state.data()?.raw.segments[0].new_text).toBe('new');
    state.back(); request().flush(rawResponse()); expect(state.data()?.raw.cursor.chunk).toBe(0); expect(state.canBack()).toBe(false);
  });

  it('retries failed navigation at the same cursor without adding another history entry', () => {
    open(); state.next(); const next = request(); next.error(new ProgressEvent('error'));
    expect(state.data()).toBeNull(); expect(state.canBack()).toBe(true); state.retry();
    const retry = request(); expect(retry.request.params.toString()).toBe(next.request.params.toString()); retry.flush(lastResponse());
    state.back(); request().flush(rawResponse()); expect(state.canBack()).toBe(false);
  });

  it('rejects mixed identity, hashes, byte ranges, chunk cursors and inconsistent metadata', () => {
    const corrupt: Array<(v: RawFileResponse) => void> = [
      v => v.snapshot_id = id(9), v => v.head_sha = '9'.repeat(40), v => v.content_id = '9'.repeat(64),
      v => v.file.after!.path = 'docs/other.md', v => v.raw.old_sha256 = '9'.repeat(64),
      v => v.raw.old_bytes = 4, v => v.raw.segments[0].old_end_byte = 2,
      v => v.raw.segments[0].old_text = 'אב', v => v.raw.segments[0].kind = 'equal',
      v => v.raw.next = { chunk: 0, offset: 1 }, v => v.raw.cursor.offset = 1,
      v => v.raw.returned_text_bytes = 4, v => v.raw.segments[0].continues_chunk = true,
    ];
    for (const mutate of corrupt) {
      state.clear(); state.open(rawScope()); const result = rawResponse(); mutate(result); request().flush(result);
      expect(state.data()).toBeNull(); expect(state.error()).not.toBe('');
    }
    state.clear(); open(); state.next(); const result = lastResponse(); result.raw.old_lines = 2; request().flush(result);
    expect(state.data()).toBeNull();
  });

  it('checks continuity across page boundaries, including after a failed response and retry', () => {
    open(); state.next(); const bad = lastResponse(); bad.raw.segments[0].old_start_byte = 2; bad.raw.segments[0].old_end_byte = 2;
    request().flush(bad); expect(state.data()).toBeNull(); state.retry(); request().flush(bad); expect(state.data()).toBeNull();
    state.retry(); request().flush(lastResponse()); expect(state.data()).not.toBeNull();
  });

  it('accepts UTF-8 continuation windows only at the exact advertised chunk offset', () => {
    const scope = rawScope(); scope.file.before!.bytes = 4; scope.file.after!.bytes = 4; state.open(scope);
    const part = rawResponse(); part.file = structuredClone(scope.file); part.raw.old_bytes = 4; part.raw.new_bytes = 4;
    part.raw.total_chunks = 1; part.raw.segments[0] = { ...part.raw.segments[0], kind: 'equal', old_text: 'א', new_text: 'א',
      old_end_byte: 2, new_end_byte: 2, continues_chunk: true };
    part.raw.returned_text_bytes = 4; part.raw.next = { chunk: 0, offset: 2 }; request().flush(part);
    expect(state.data()).not.toBeNull(); state.next(); const last = structuredClone(part); last.raw.cursor = { chunk: 0, offset: 2 };
    last.raw.next = null; last.raw.segments[0] = { ...last.raw.segments[0], old_text: 'ב', new_text: 'ב', old_start_byte: 2,
      old_end_byte: 4, new_start_byte: 2, new_end_byte: 4, old_start_column_bytes: 2, new_start_column_bytes: 2, continues_chunk: false };
    request().flush(last); expect(state.data()).not.toBeNull();
  });

  it('distinguishes an empty added file from a missing side without inventing source', () => {
    const scope = rawScope(); scope.file.before = null; scope.file.status = 'added'; scope.file.matched_by = null;
    scope.file.after!.bytes = 0; scope.file.after!.sha256 = 'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855';
    state.open(scope); const empty = rawResponse(); empty.file = scope.file;
    Object.assign(empty.raw, { old_bytes: 0, new_bytes: 0, old_lines: 0, new_lines: 0, total_chunks: 0, next: null,
      returned_text_bytes: 0, segments: [], old_sha256: scope.file.after!.sha256, new_sha256: scope.file.after!.sha256 });
    request().flush(empty); expect(state.data()?.file.before).toBeNull(); expect(state.data()?.file.after?.bytes).toBe(0);
  });

  it('cancels and destroys private source on scope loss or replacement', () => {
    state.open(rawScope()); const first = request(); state.open({ ...rawScope(), snapshot: id(9) });
    expect(first.cancelled).toBe(true); const second = request(); state.open(null);
    expect(second.cancelled).toBe(true); expect(state.data()).toBeNull(); expect(state.canBack()).toBe(false);
  });

  it('requires authentication refresh after identity errors and rejects non-text files locally', () => {
    open(); state.next(); request().flush({ error: { code: 'SESSION_IDENTITY_CHANGED' } }, { status: 409, statusText: 'Conflict' });
    expect(state.authRequired()).toBe(true); expect(state.data()).toBeNull(); state.retry();
    http.expectNone(req => req.url.includes('/diff/files/'));
    state.clear(); const scope = rawScope(); scope.file.kind = 'asset'; scope.file.matched_by = 'path';
    scope.file.before!.path = 'assets/a.png'; scope.file.after!.path = 'assets/a.png'; state.open(scope);
    expect(state.error()).not.toBe(''); http.expectNone(req => req.url.includes('/diff/files/'));
  });
});
