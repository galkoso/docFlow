import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { SessionState } from '../../core/auth/session-state';
import type { OfficialRecorded, OfficialRequest } from '../../core/api/review-api';
import { actor, product, proposal, snapshot, other, metadata, officialHistory } from '../../../testing/review-fixtures';
import { OfficialReviewsView } from './official-reviews-view';

describe('Official snapshot review controls', () => {
  const url = `/api/v1/proposals/${proposal}/review/snapshots/${snapshot}/decisions`;
  function create(updated = false) {
    const profile = signal<{ user_id: string } | null>({ user_id: product }), logoutEpoch = signal(0), source = metadata();
    if (updated) { source.metadata.summary.recorded_base_sha = source.metadata.summary.target_sha; source.metadata.summary.merge_base_sha = source.metadata.summary.target_sha; source.metadata.summary.base_outdated = false; }
    TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting(), { provide: SessionState, useValue: { profile, logoutEpoch } }] });
    const fixture = TestBed.createComponent(OfficialReviewsView), view = fixture.componentInstance, http = TestBed.inject(HttpTestingController);
    fixture.componentRef.setInput('actor', product); fixture.componentRef.setInput('proposal', proposal); fixture.componentRef.setInput('snapshot', source);
    const denied = vi.fn(), recorded = vi.fn(); view.accessDenied.subscribe(denied); view.recorded.subscribe(recorded); fixture.detectChanges();
    const history = () => ({ ...officialHistory(product), approve_all_available: updated });
    const load = () => { http.expectOne(url).flush(history()); fixture.detectChanges(); };
    return { fixture, view, http, profile, logoutEpoch, denied, recorded, load, history };
  }
  function receipt(body: OfficialRequest): OfficialRecorded {
    return { id: other, request_id: body.request_id, proposal_id: proposal, snapshot_id: snapshot, reviewer_id: product,
      domain: body.domain, decision: body.decision, head_sha: metadata().metadata.summary.head_sha, target_sha: metadata().metadata.summary.target_sha,
      policy_version: 1, configuration_version: 2, recorded_at: '2026-11-19T00:00:00Z', replayed: false };
  }
  afterEach(() => vi.useRealTimers());
  it('loads history with GET, warns before approve-all, requires an explanation and submits only on explicit action', () => {
    const { fixture, view, http, load, history, recorded } = create(true); load(); http.expectNone(req => req.method === 'POST');
    view.decision.set('APPROVE_ALL'); fixture.detectChanges(); expect(fixture.nativeElement.textContent).toContain('ייתכן שיש תוכן שטרם צפית בו');
    view.send(); http.expectNone(req => req.method === 'POST'); view.explanation.set('Checked all saved changes'); view.send(); view.send();
    const post = http.expectOne(url); expect(post.request.method).toBe('POST'); expect(post.request.headers.get('X-Expected-Actor')).toBe(product);
    expect(post.request.body.expected_fingerprint).toBe(metadata().fingerprint); expect(post.request.body.decision).toBe('APPROVE_ALL');
    post.flush(receipt(post.request.body)); expect(view.pending()).toBeNull(); expect(view.hasDraft()).toBe(false); expect(recorded).toHaveBeenCalledOnce();
    http.expectOne(url).flush(history()); fixture.destroy(); http.verify();
  });
  it('keeps one immutable request after a lost response and never resubmits from history refresh', () => {
    const { fixture, view, http, load, history } = create(); load(); view.explanation.set('A review note'); view.send();
    const first = http.expectOne(url), body = first.request.body; first.error(new ProgressEvent('error')); expect(view.hasDraft()).toBe(true);
    view.explanation.set('An attempted edit'); view.send(); http.expectNone(req => req.method === 'POST');
    view.refresh(); http.expectOne(url).flush(history()); http.expectNone(req => req.method === 'POST');
    view.retry(); const replay = http.expectOne(url); expect(replay.request.body).toEqual(body); replay.flush({ ...receipt(body), replayed: true });
    http.expectOne(url).flush(history()); fixture.destroy(); http.verify();
  });
  it('honors Retry-After without an automatic POST and retries the same body only after the wait', () => {
    vi.useFakeTimers(); const { fixture, view, http, load } = create(); load(); view.explanation.set('A review note'); view.send();
    const request = http.expectOne(url), body = request.request.body;
    request.flush({ error: { code: 'REVIEW_PROVIDER_RATE_LIMIT' } }, { status: 429, statusText: 'Wait', headers: { 'Retry-After': '13' } });
    expect(view.retryWait()).toBe(13); view.retry(); http.expectNone(url); vi.advanceTimersByTime(13000); http.expectNone(url);
    view.retry(); const repeated = http.expectOne(url); expect(repeated.request.body).toEqual(body); fixture.destroy(); expect(repeated.cancelled).toBe(true); http.verify();
  });
  it.each(['actor', 'logout', 'snapshot'])('ignores late submission success after %s changes before effects run', fault => {
    const { fixture, view, http, load, profile, logoutEpoch, recorded } = create(); load(); view.explanation.set('Private note'); view.send();
    const request = http.expectOne(url);
    if (fault === 'actor') profile.set({ user_id: actor });
    if (fault === 'logout') logoutEpoch.set(1);
    if (fault === 'snapshot') fixture.componentRef.setInput('snapshot', { ...metadata(), snapshot_id: other });
    request.flush(receipt(request.request.body)); expect(recorded).not.toHaveBeenCalled(); expect(view.view()).toBeNull(); http.expectNone(url);
    fixture.destroy(); http.verify();
  });
  it.each([401, 403, 404])('clears drafts and propagates scoped denial %s', status => {
    const { fixture, view, http, load, denied } = create(); load(); view.explanation.set('Private note'); view.refresh();
    http.expectOne(url).flush({}, { status, statusText: 'Denied' }); expect(view.explanation()).toBe(''); expect(view.view()).toBeNull();
    expect(denied).toHaveBeenCalledOnce(); fixture.destroy(); http.verify();
  });
  it('retains unsent notes when history is unavailable without denying the shared snapshot', () => {
    const { fixture, view, http, load, denied } = create(); load(); view.explanation.set('Unsent note'); view.refresh();
    http.expectOne(url).flush({}, { status: 503, statusText: 'Unavailable' }); expect(view.explanation()).toBe('Unsent note'); expect(denied).not.toHaveBeenCalled();
    fixture.destroy(); http.verify();
  });
  it('rejects mixed receipt scope and preserves the original uncertain request', () => {
    const { fixture, view, http, load, recorded } = create(); load(); view.explanation.set('A review note'); view.send();
    const request = http.expectOne(url); request.flush({ ...receipt(request.request.body), snapshot_id: other });
    expect(view.pending()).not.toBeNull(); expect(recorded).not.toHaveBeenCalled(); http.expectNone(url); fixture.destroy(); http.verify();
  });
});
