import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { SessionState } from '../../core/auth/session-state';
import { actor, proposal, snapshot, other, metadata, prStatus } from '../../../testing/review-fixtures';
import { ReviewPrStatusView } from './review-pr-status';

describe('Shared PR status view', () => {
  const url = `/api/v1/proposals/${proposal}/review/snapshots/${snapshot}/pr-sync`;
  function create() {
    const profile = signal<{ user_id: string } | null>({ user_id: actor }), logoutEpoch = signal(0);
    TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting(), { provide: SessionState, useValue: { profile, logoutEpoch } }] });
    const fixture = TestBed.createComponent(ReviewPrStatusView), view = fixture.componentInstance, http = TestBed.inject(HttpTestingController), denied = vi.fn();
    fixture.componentRef.setInput('actor', actor); fixture.componentRef.setInput('proposal', proposal); fixture.componentRef.setInput('snapshot', metadata());
    view.accessDenied.subscribe(denied); fixture.detectChanges();
    return { fixture, view, http, profile, logoutEpoch, denied };
  }
  afterEach(() => vi.useRealTimers());
  function stopped(mode: 'RETRY_UNSTARTED' | 'OBSERVE_STARTED' = 'OBSERVE_STARTED') {
    const value = prStatus(); value.recovery = mode; value.pr = null;
    value.sync = { phase: mode === 'OBSERVE_STARTED' ? 'RECONCILING' : 'FAILED', automatic: false };
    value.uncertain_write = mode === 'OBSERVE_STARTED'; return value;
  }
  it.each(['RETRY_UNSTARTED', 'OBSERVE_STARTED'] as const)('admits an explicit %s recovery and observes status separately', mode => {
    const { fixture, view, http } = create(); http.expectOne(url).flush(stopped(mode)); fixture.detectChanges();
    http.expectNone(req => req.method === 'POST'); view.recover(); view.recover();
    const request = http.expectOne(`${url}/recover`), body = request.request.body;
    expect(request.request.headers.get('X-Expected-Actor')).toBe(actor); expect(body.expected_fingerprint).toBe(metadata().fingerprint);
    expect(body.request_id).toMatch(/^[a-f0-9-]{36}$/); expect(Object.keys(body).sort()).toEqual(['expected_fingerprint', 'request_id']);
    request.flush({ proposal_id: proposal, snapshot_id: snapshot, request_id: body.request_id, mode, replayed: false });
    expect(view.recovering()).toBe(false); expect(view.recoveryPending()).toBe(false); expect(view.view()).toBeNull();
    http.expectOne(url).flush(prStatus()); fixture.detectChanges(); expect(fixture.nativeElement.textContent).toContain('תוצאת העבודה נבדקת בנפרד');
    fixture.destroy(); http.verify();
  });
  it('replays one immutable request after a lost reply and never posts from GET refresh or automatically', () => {
    const { fixture, view, http } = create(); http.expectOne(url).flush(stopped()); view.recover();
    const first = http.expectOne(`${url}/recover`), body = first.request.body; first.error(new ProgressEvent('error'));
    expect(view.recoveryPending()).toBe(true); http.expectNone(req => req.method === 'POST');
    view.refresh(); http.expectOne(url).flush(stopped()); http.expectNone(req => req.method === 'POST');
    view.recover(); const replay = http.expectOne(`${url}/recover`); expect(replay.request.body).toEqual(body);
    replay.flush({ proposal_id: proposal, snapshot_id: snapshot, request_id: body.request_id, mode: 'OBSERVE_STARTED', replayed: true });
    http.expectOne(url).flush(prStatus()); expect(view.recoveryPending()).toBe(false); fixture.destroy(); http.verify();
  });
  it('recovers stopped work from an earlier submission of the same snapshot without inventing new work', () => {
    const { fixture, view, http } = create(), value = stopped(); value.sync = null; value.dispatch = { phase: 'AWAITING' };
    http.expectOne(url).flush(value); fixture.detectChanges();
    expect(view.view()?.sync).toBeNull(); expect(fixture.nativeElement.textContent).toContain('עבודה קודמת');
    view.recover(); const request = http.expectOne(`${url}/recover`);
    expect(request.request.body.expected_fingerprint).toBe(value.fingerprint); request.error(new ProgressEvent('error'));
    fixture.destroy(); http.verify();
  });
  it('keeps a recovery context conflict local and retains the same request for inspection', () => {
    const { fixture, view, http, denied } = create(); http.expectOne(url).flush(stopped()); view.recover();
    http.expectOne(`${url}/recover`).flush({ error: { code: 'PR_RECOVERY_NOT_STOPPED' } }, { status: 409, statusText: 'Conflict' });
    expect(denied).not.toHaveBeenCalled(); expect(view.recoveryPending()).toBe(true); expect(view.error()).toContain('השתנו');
    fixture.destroy(); http.verify();
  });
  it('does not accept a recovery receipt for another request or scope', () => {
    const { fixture, view, http } = create(); http.expectOne(url).flush(stopped()); view.recover();
    const request = http.expectOne(`${url}/recover`);
    request.flush({ proposal_id: proposal, snapshot_id: other, request_id: request.request.body.request_id, mode: 'OBSERVE_STARTED', replayed: false });
    expect(view.recoveryPending()).toBe(true); expect(view.view()).toBeNull(); http.expectNone(url); fixture.destroy(); http.verify();
  });
  it.each(['actor', 'logout'] as const)('ignores a recovery reply after %s changes before effects run', fault => {
    const { fixture, view, http, profile, logoutEpoch } = create(); http.expectOne(url).flush(stopped()); view.recover();
    const request = http.expectOne(`${url}/recover`);
    if (fault === 'actor') profile.set({ user_id: other }); else logoutEpoch.set(1);
    request.flush({ proposal_id: proposal, snapshot_id: snapshot, request_id: request.request.body.request_id, mode: 'OBSERVE_STARTED', replayed: false });
    expect(view.view()).toBeNull(); expect(view.recoveryMessage()).toBe(''); http.expectNone(url); fixture.destroy(); http.verify();
  });
  it('keeps an unavailable status local and retries only a GET', () => {
    const { fixture, view, http, denied } = create();
    http.expectOne(url).flush({}, { status: 503, statusText: 'Unavailable' }); fixture.detectChanges();
    expect(denied).not.toHaveBeenCalled(); expect(view.view()).toBeNull(); expect(view.busy()).toBe(false);
    expect(fixture.nativeElement.textContent).toContain('גרסת הסקירה השמורה עדיין זמינה');
    view.refresh(); const request = http.expectOne(url); expect(request.request.method).toBe('GET'); request.flush(prStatus());
    expect(view.view()).not.toBeNull(); fixture.destroy(); http.verify();
  });
  it.each(['actor', 'logout'])('drops a response arriving after %s changes before effects run', fault => {
    const { fixture, view, http, profile, logoutEpoch, denied } = create(), request = http.expectOne(url);
    if (fault === 'actor') profile.set({ user_id: other }); else logoutEpoch.update(value => value + 1);
    request.flush(prStatus()); expect(view.view()).toBeNull(); expect(denied).not.toHaveBeenCalled();
    fixture.destroy(); http.verify();
  });
  it('clears a stored link synchronously when the session disappears', () => {
    const { fixture, view, http, profile } = create(); http.expectOne(url).flush(prStatus()); expect(view.view()).not.toBeNull();
    profile.set(null); expect(view.view()).toBeNull(); fixture.detectChanges(); expect(fixture.nativeElement.querySelector('a')).toBeNull();
    fixture.destroy(); http.verify();
  });
  it('cancels in-flight work when selecting another snapshot and ignores the old scope', () => {
    const { fixture, http } = create(), old = http.expectOne(url), value = metadata(); value.snapshot_id = other;
    fixture.componentRef.setInput('snapshot', value); fixture.detectChanges(); expect(old.cancelled).toBe(true);
    const fresh = http.expectOne(url.replace(snapshot, other)); fresh.flush({}, { status: 503, statusText: 'Unavailable' });
    fixture.destroy(); http.verify();
  });
  it('polls pending work without overlap and stops after a terminal observation', async () => {
    vi.useFakeTimers(); const { fixture, http } = create(), value = prStatus(); value.pr = null; value.sync = { phase: 'QUEUED', automatic: true };
    http.expectOne(url).flush(value); await vi.advanceTimersByTimeAsync(10000);
    const next = http.expectOne(url); await vi.advanceTimersByTimeAsync(1000); http.expectNone(url);
    next.flush(prStatus()); await vi.advanceTimersByTimeAsync(10000); http.expectNone(url);
    fixture.destroy(); http.verify();
  });
  it('renders only a validated link and distinguishes stored provider observation from current readiness', () => {
    const { fixture, http } = create(), request = http.expectOne(url);
    expect(request.request.method).toBe('GET'); expect(request.request.headers.get('X-Expected-Actor')).toBe(actor);
    request.flush(prStatus()); fixture.detectChanges();
    const element = fixture.nativeElement as HTMLElement, link = element.querySelector('a')!;
    expect(link.href).toBe(prStatus().pr!.url); expect(link.rel).toBe('noopener noreferrer');
    expect(element.textContent).toContain('תיעוד'); expect(element.textContent).toContain('נבדקים בנפרד');
    expect(element.querySelector('bdi')!.textContent).toContain('#9'); http.expectNone(req => req.method !== 'GET'); fixture.destroy(); http.verify();
  });
  it('drops the previous link on a local 503 and retries with GET without denying snapshot access', () => {
    const { fixture, view, http, denied } = create(); http.expectOne(url).flush(prStatus()); view.refresh();
    expect(view.view()).toBeNull(); http.expectOne(url).flush({}, { status: 503, statusText: 'Unavailable' }); fixture.detectChanges();
    expect(denied).not.toHaveBeenCalled(); expect(fixture.nativeElement.textContent).toContain('עדיין זמינה');
    view.refresh(); http.expectOne(url).flush(prStatus()); fixture.destroy(); http.verify();
  });
  it.each([401, 403, 404, 409])('purges state and propagates scoped denial %s', status => {
    const { fixture, view, http, denied } = create(); http.expectOne(url).flush({}, { status, statusText: 'Denied' });
    expect(view.view()).toBeNull(); expect(denied).toHaveBeenCalledTimes(1); fixture.destroy(); http.verify();
  });
  it.each(['actor', 'logout', 'snapshot'])('rejects late evidence after a %s change before effects run', change => {
    const { fixture, view, http, profile, logoutEpoch, denied } = create(), request = http.expectOne(url);
    if (change === 'actor') profile.set({ user_id: other });
    if (change === 'logout') logoutEpoch.set(1);
    if (change === 'snapshot') fixture.componentRef.setInput('snapshot', { ...metadata(), snapshot_id: other });
    request.flush(prStatus()); expect(view.view()).toBeNull(); expect(denied).not.toHaveBeenCalled(); fixture.destroy(); http.verify();
  });
  it('polls one GET at a time for planned work, stops on terminal evidence, and cancels on destruction', () => {
    vi.useFakeTimers();
    const { fixture, view, http } = create(), queued = { ...prStatus(), pr: null, sync: { phase: 'QUEUED' as const, automatic: true } };
    http.expectOne(url).flush(queued); vi.advanceTimersByTime(10000);
    const next = http.expectOne(url); vi.advanceTimersByTime(2000); http.expectNone(url);
    next.flush(prStatus()); vi.advanceTimersByTime(10000); http.expectNone(url);
    view.refresh(); const last = http.expectOne(url); fixture.destroy(); expect(last.cancelled).toBe(true); vi.advanceTimersByTime(10000); http.expectNone(url); http.verify();
  });
  it('rejects unsafe provider evidence without publishing a link or denying the saved snapshot', () => {
    const { fixture, view, http, denied } = create(), value = prStatus(); value.pr!.url = 'javascript:alert(1)';
    http.expectOne(url).flush(value); fixture.detectChanges();
    expect(view.view()).toBeNull(); expect(fixture.nativeElement.querySelector('a')).toBeNull(); expect(denied).not.toHaveBeenCalled(); fixture.destroy(); http.verify();
  });
});
