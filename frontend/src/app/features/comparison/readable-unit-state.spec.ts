import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { readableContract, unitResponse, unitScope } from './readable.testing';
import { ReadableUnitState } from './readable-unit-state';

describe('Private readable content windows', () => {
  let state: ReadableUnitState, http: HttpTestingController;
  beforeEach(() => {
    TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting(), ReadableUnitState] });
    state = TestBed.inject(ReadableUnitState); http = TestBed.inject(HttpTestingController);
  });
  afterEach(() => { state.clear(); http.verify(); });
  const request = () => http.expectOne(req => req.url.includes('/readable/units/'));
  it('loads only the selected unit, traverses exact cursors and keeps one source window', () => {
    const scope = unitScope('list'); state.open(scope); const first = request();
    expect(first.request.headers.get('X-Expected-Actor')).toBe(scope.file.actor);
    expect(first.request.params.get('side')).toBe('before'); first.flush(unitResponse(scope));
    const windows = readableContract.units.find(item => item.side === scope.side && item.unit.index === scope.unit.index)!.windows;
    for (let index = 1; index < windows.length; index++) {
      state.next(); expect(state.events()).toEqual([]); expect(state.data()).toBeNull();
      const next = request(); expect(next.request.params.get('event')).toBe(String(windows[index].cursor.event));
      expect(next.request.params.get('offset')).toBe(String(windows[index].cursor.offset)); next.flush(unitResponse(scope, index));
      expect(state.error()).toBe(''); expect(state.data()).not.toBeNull();
    }
    state.back(); request().flush(unitResponse(scope, windows.length - 2)); expect(state.data()).not.toBeNull();
  });
  it('retries only the failed cursor and keeps the boundary proof until it succeeds', () => {
    const scope = unitScope('list'); state.open(scope); request().flush(unitResponse(scope)); state.next();
    const next = request(); next.error(new ProgressEvent('error')); expect(state.data()).toBeNull();
    state.retry(); const retry = request(); expect(retry.request.params.toString()).toBe(next.request.params.toString());
    const mixed = unitResponse(scope, 1);
    for (const stack of [mixed.window!.start_stack, mixed.window!.end_stack]) stack[0].block_id = 'other';
    retry.flush(mixed); expect(state.data()).toBeNull(); expect(state.error()).not.toBe('');
    state.retry(); request().flush(unitResponse(scope, 1)); expect(state.data()).not.toBeNull();
  });
  it('keeps protected source explicit without inventing an empty readable document', () => {
    const scope = unitScope('protected'); state.open(scope); request().flush(unitResponse(scope));
    expect(state.error()).toBe(''); expect(state.data()?.state).toBe('RAW_REQUIRED'); expect(state.events()).toEqual([]);
  });
  it('destroys content and cancels requests on replacement or identity loss', () => {
    const scope = unitScope(); state.open(scope); const first = request(); state.open(unitScope('code')); expect(first.cancelled).toBe(true);
    request().flush(unitResponse(unitScope('code'))); expect(state.events().length).toBeGreaterThan(0); state.open(null);
    expect(state.events()).toEqual([]); expect(state.data()).toBeNull();
    state.open(scope); const outstanding = request(); state.ngOnDestroy(); expect(outstanding.cancelled).toBe(true);
  });
  it('stops retrying after an identity error until the owner supplies a refreshed scope', () => {
    const scope = unitScope(); state.open(scope);
    request().flush({ error: { code: 'SESSION_IDENTITY_CHANGED' } }, { status: 409, statusText: 'Conflict' });
    expect(state.authRequired()).toBe(true); state.retry(); http.expectNone(req => req.url.includes('/readable/units/'));
  });
  it('honors an explicit raw fallback on a later window without confusing it with a missing boundary', () => {
    const scope = unitScope('list'); state.open(scope); request().flush(unitResponse(scope)); state.next();
    const fallback = unitResponse(scope, 1); fallback.state = 'RAW_REQUIRED'; fallback.window = null; fallback.error_code = 'READABLE_WINDOW_TOO_LARGE';
    request().flush(fallback); expect(state.error()).toBe(''); expect(state.data()?.state).toBe('RAW_REQUIRED');
    expect(state.events()).toEqual([]); state.back(); request().flush(unitResponse(scope)); expect(state.events().length).toBeGreaterThan(0);
  });

});
