import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { SessionState } from '../../core/auth/session-state';
import { ReviewSourceState, REVIEW_SOURCE_DIGEST } from './review-source-state';
import { abcHash, sourceScope, sourceWindow, visitReceipt } from '../../../testing/review-source-fixtures';
import { product, proposal, snapshot, other } from '../../../testing/review-fixtures';

describe('Verified review source and frozen acknowledgement state', () => {
  const root = `/api/v1/proposals/${proposal}/review/snapshots/${snapshot}`, getUrl = `${root}/units/${sourceScope().unit.id}/source`;
  function create(hash = async () => abcHash, selected = sourceScope()) {
    const profile = signal<{ user_id: string } | null>({ user_id: product }), logoutEpoch = signal(0), live = signal(true);
    TestBed.configureTestingModule({ providers: [ReviewSourceState, provideHttpClient(), provideHttpClientTesting(),
      { provide: REVIEW_SOURCE_DIGEST, useValue: hash }, { provide: SessionState, useValue: { profile, logoutEpoch } }] });
    const state = TestBed.inject(ReviewSourceState), http = TestBed.inject(HttpTestingController), scope = selected;
    state.open(scope, () => live()); return { state, http, scope, profile, logoutEpoch, live };
  }
  async function settle() { for (let i = 0; i < 8; i++) await Promise.resolve(); }
  afterEach(() => vi.useRealTimers());
  it('verifies a captured delivery, records only an explicit action, and accepts only its exact receipt', async () => {
    const { state, http, scope } = create(); const read = http.expectOne(req => req.url === getUrl);
    expect(read.request.headers.get('X-Expected-Actor')).toBe(product); read.flush(sourceWindow()); await settle();
    expect(state.view()?.text).toBe('abc'); http.expectNone(req => req.method === 'POST'); expect(state.canAcknowledge()).toBe(true);
    scope.unit.after!.source.path = 'docs/later.md'; expect(state.scope()!.unit.after!.source.path).toBe('docs/a.md');
    state.acknowledge(); state.acknowledge(); const post = http.expectOne(`${root}/visits`), body = post.request.body;
    expect(Object.isFrozen(body)).toBe(true); expect(Object.isFrozen(state.pending()!.window.source.source)).toBe(true);
    expect(body.expected_fingerprint).toBe(state.scope()!.fingerprint); expect(body.delivery_id).toBe(state.view()!.delivery_id);
    post.flush(visitReceipt(body)); expect(state.hasPending()).toBe(false); expect(state.receipt()).not.toBeNull();
    state.acknowledge(); http.expectNone(req => req.method === 'POST'); state.clear(); http.verify();
  });
  it('retains one nonce after a lost response or malformed receipt and blocks reads or retargeting until explicit retry', async () => {
    const { state, http } = create(); http.expectOne(req => req.url === getUrl).flush(sourceWindow()); await settle();
    state.acknowledge(); const first = http.expectOne(`${root}/visits`), body = first.request.body; first.error(new ProgressEvent('error'));
    state.refresh(); state.next(); state.open({ ...sourceScope(), domain: 'DEVELOPMENT' }, () => true);
    expect(state.scope()!.domain).toBe('PRODUCT'); http.expectNone(req => req.method === 'GET');
    state.retry(); const retry = http.expectOne(`${root}/visits`); expect(retry.request.body).toBe(body);
    retry.flush({ ...visitReceipt(body), actor_id: other }); expect(state.pending()!.body).toBe(body); expect(state.receipt()).toBeNull();
    state.retry(); const exact = http.expectOne(`${root}/visits`); expect(exact.request.body).toBe(body);
    exact.flush({ ...visitReceipt(body), replayed: true }); expect(state.pending()).toBeNull(); state.clear(); http.verify();
  });
  it.each([401, 403, 404])('purges private source and pending writes on %s access loss', async status => {
    const { state, http } = create(); http.expectOne(req => req.url === getUrl).flush(sourceWindow()); await settle(); state.acknowledge();
    http.expectOne(`${root}/visits`).flush({}, { status, statusText: 'Denied' });
    expect(state.denied()).toBe(true); expect(state.view()).toBeNull(); expect(state.scope()).toBeNull(); expect(state.pending()).toBeNull(); http.verify();
  });
  it('hides stale source immediately while asynchronous digest verification is still running', async () => {
    let finish!: (value: string) => void; const { state, http, live } = create(() => new Promise(resolve => { finish = resolve; }));
    http.expectOne(req => req.url === getUrl).flush(sourceWindow()); live.set(false); finish(abcHash); await settle();
    expect(state.view()).toBeNull(); state.acknowledge(); http.expectNone(req => req.method === 'POST'); TestBed.tick();
    expect(state.scope()).toBeNull(); http.verify();
  });
  it.each(['actor', 'logout'])('rejects an already queued receipt after %s changes before cleanup runs', async fault => {
    const { state, http, profile, logoutEpoch } = create(); http.expectOne(req => req.url === getUrl).flush(sourceWindow()); await settle();
    state.acknowledge(); const post = http.expectOne(`${root}/visits`), body = post.request.body;
    if (fault === 'actor') profile.set({ user_id: other }); else logoutEpoch.update(n => n + 1);
    post.flush(visitReceipt(body)); expect(state.view()).toBeNull(); expect(state.receipt()).toBeNull(); TestBed.tick();
    expect(state.scope()).toBeNull(); expect(state.pending()).toBeNull(); http.verify();
  });
  it('waits without automatic retry and replays the original nonce even after the client delivery timer expires', async () => {
    vi.useFakeTimers(); const { state, http } = create(); const w = { ...sourceWindow(), expires_at: new Date(Date.now() + 1000).toISOString() };
    http.expectOne(req => req.url === getUrl).flush(w); await settle(); state.acknowledge();
    const first = http.expectOne(`${root}/visits`), body = first.request.body;
    first.flush({}, { status: 429, statusText: 'Limited', headers: { 'Retry-After': '2' } }); state.retry(); http.expectNone(`${root}/visits`);
    vi.advanceTimersByTime(2100); expect(state.expired()).toBe(true); expect(state.retryWait()).toBe(0); http.expectNone(`${root}/visits`);
    state.retry(); const retry = http.expectOne(`${root}/visits`); expect(retry.request.body).toBe(body);
    retry.flush({ ...visitReceipt(body, sourceScope(), w), recorded_at: new Date(Date.parse(w.expires_at) - 500).toISOString(), replayed: true }); expect(state.pending()).toBeNull(); state.clear(); http.verify();
  });
  it('requires a new source delivery after known expiry without retrying the expired acknowledgement automatically', async () => {
    const { state, http } = create(); http.expectOne(req => req.url === getUrl).flush(sourceWindow()); await settle(); state.acknowledge();
    http.expectOne(`${root}/visits`).flush({}, { status: 410, statusText: 'Gone' }); expect(state.pending()).toBeNull(); expect(state.canAcknowledge()).toBe(false);
    state.acknowledge(); http.expectNone(`${root}/visits`); state.refresh(); http.expectOne(req => req.url === getUrl).flush({ ...sourceWindow(), delivery_id: other });
    await settle(); expect(state.view()!.delivery_id).toBe(other); http.expectNone(`${root}/visits`); state.clear(); http.verify();
  });
  it('pages only through verified cursors and rereads a previous window without acknowledging either automatically', async () => {
    const scope = sourceScope(); scope.unit.after!.source.bytes = 65541; scope.unit.after!.range!.end_byte = 65541;
    const { state, http } = create(async () => abcHash, scope);
    const first = { ...sourceWindow(scope), end_byte: 65537, text: 'a'.repeat(65536), next_offset: 65536 };
    http.expectOne(req => req.url === getUrl).flush(first); await settle(); expect(state.canBack()).toBe(false);
    state.next(); const next = http.expectOne(req => req.url === getUrl); expect(next.request.params.get('offset')).toBe('65536');
    next.flush({ ...sourceWindow(scope), offset: 65536, start_byte: 65537, text: 'tail', delivery_id: other }); await settle();
    expect(state.canBack()).toBe(true); expect(state.view()!.next_offset).toBeNull(); http.expectNone(`${root}/visits`);
    state.back(); const previous = http.expectOne(req => req.url === getUrl); expect(previous.request.params.get('offset')).toBe('0');
    previous.flush(first); await settle(); expect(state.canBack()).toBe(false); expect(state.view()!.text.length).toBe(65536);
    http.expectNone(`${root}/visits`); state.clear(); http.verify();
  });
  it('bounds stalled reads and cancels delivery requests without recording an action', () => {
    vi.useFakeTimers(); const { state, http } = create(), pending = http.expectOne(req => req.url === getUrl);
    vi.advanceTimersByTime(20001); expect(pending.cancelled).toBe(true); expect(state.busy()).toBe(false); expect(state.view()).toBeNull();
    http.expectNone(req => req.method === 'POST'); state.clear(); http.verify();
  });
});
