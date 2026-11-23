import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting, HttpTestingController } from '@angular/common/http/testing';
import { SessionState } from '../../core/auth/session-state';
import { ReviewCoverageView } from './review-coverage-view';
import { metadata, product, proposal, snapshot, other, reviewUnits } from '../../../testing/review-fixtures';
import { reviewCoverage } from '../../../testing/review-coverage-fixtures';

describe('Own review coverage read panel', () => {
  const url = `/api/v1/proposals/${proposal}/review/snapshots/${snapshot}/coverage`;
  function create() {
    const profile = signal<{ user_id: string } | null>({ user_id: product }), logoutEpoch = signal(0);
    TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting(),
      { provide: SessionState, useValue: { profile, logoutEpoch } }] });
    const fixture = TestBed.createComponent(ReviewCoverageView), view = fixture.componentInstance, http = TestBed.inject(HttpTestingController), denied = vi.fn();
    view.accessDenied.subscribe(denied); fixture.componentRef.setInput('actor', product); fixture.componentRef.setInput('proposal', proposal);
    fixture.componentRef.setInput('snapshot', metadata()); fixture.componentRef.setInput('page', reviewUnits(product)); fixture.componentRef.setInput('domain', 'PRODUCT');
    fixture.detectChanges(); return { fixture, view, http, profile, logoutEpoch, denied };
  }
  afterEach(() => vi.useRealTimers());
  it('reads exact context and shows read counts without implying approval', () => {
    const { fixture, view, http } = create(), request = http.expectOne(req => req.url === url);
    expect(request.request.headers.get('X-Expected-Actor')).toBe(product); expect(request.request.params.get('expected_fingerprint')).toBe(metadata().fingerprint);
    expect(request.request.params.get('offset')).toBe('0'); expect(request.request.params.get('limit')).toBe('20'); request.flush(reviewCoverage()); fixture.detectChanges();
    expect(view.view()?.unviewed_units).toBe(2); expect(fixture.nativeElement.textContent).toContain('נתוני קריאה בלבד');
    http.expectNone(req => req.method === 'POST'); fixture.destroy(); http.verify();
  });
  it('refreshes only for a new confirmed receipt token and never posts or optimistically changes coverage', () => {
    const { fixture, view, http } = create(); http.expectOne(req => req.url === url).flush(reviewCoverage());
    fixture.componentRef.setInput('refreshToken', other); fixture.detectChanges(); expect(view.view()).toBeNull();
    http.expectOne(req => req.url === url).flush(reviewCoverage()); fixture.componentRef.setInput('refreshToken', null); fixture.detectChanges();
    http.expectNone(url); expect(view.view()?.viewed_units).toBe(0); http.expectNone(req => req.method === 'POST'); fixture.destroy(); http.verify();
  });
  it.each(['actor', 'logout', 'domain', 'page', 'snapshot'])('rejects a queued response immediately after %s changes', fault => {
    const { fixture, view, http, profile, logoutEpoch } = create(), pending = http.expectOne(req => req.url === url);
    if (fault === 'actor') profile.set({ user_id: other });
    if (fault === 'logout') logoutEpoch.update(n => n + 1);
    if (fault === 'domain') fixture.componentRef.setInput('domain', 'DESIGN');
    if (fault === 'page') fixture.componentRef.setInput('page', reviewUnits(product, 20, 22));
    if (fault === 'snapshot') fixture.componentRef.setInput('snapshot', { ...metadata(), fingerprint: 'a'.repeat(64) });
    pending.flush(reviewCoverage()); expect(view.view()).toBeNull(); fixture.destroy(); http.verify();
  });
  it.each([401, 403, 404])('reports %s access denial and prevents same-context retry', status => {
    const { fixture, view, http, denied } = create(); http.expectOne(req => req.url === url).flush({}, { status, statusText: 'Denied' });
    expect(denied).toHaveBeenCalledOnce(); view.refresh(); http.expectNone(url); expect(view.view()).toBeNull(); fixture.destroy(); http.verify();
  });
  it('cancels a prior domain read before requesting another assigned domain', () => {
    const { fixture, view, http } = create(), first = http.expectOne(req => req.url === url);
    const s = metadata(); s.metadata.assignments.push({ reviewer_id: product, domain: 'DESIGN', required: true });
    fixture.componentRef.setInput('snapshot', s); fixture.componentRef.setInput('domain', 'DESIGN'); fixture.detectChanges();
    expect(first.cancelled).toBe(true); const next = http.expectOne(req => req.url === url);
    expect(next.request.params.get('domain')).toBe('DESIGN'); next.flush(reviewCoverage(reviewUnits(product), 'DESIGN'));
    expect(view.view()?.domain).toBe('DESIGN'); fixture.destroy(); http.verify();
  });
  it('rejects inconsistent evidence and makes transport failures retryable without keeping stale completion', () => {
    const { fixture, view, http } = create(); http.expectOne(req => req.url === url).flush({ ...reviewCoverage(), viewed_units: 2 });
    expect(view.view()).toBeNull(); expect(view.error()).toContain('אינם תואמים'); view.refresh();
    http.expectOne(req => req.url === url).flush({}, { status: 503, statusText: 'Unavailable' }); expect(view.view()).toBeNull();
    view.refresh(); http.expectOne(req => req.url === url).flush(reviewCoverage()); expect(view.view()).not.toBeNull(); fixture.destroy(); http.verify();
  });
  it('shows unknown legacy coverage without a zero-of-zero completion claim', () => {
    const { fixture, view, http } = create(), first = http.expectOne(req => req.url === url);
    const page = { ...reviewUnits(product), available: false, total: 0, next_offset: null, units: [] };
    fixture.componentRef.setInput('page', page); fixture.detectChanges(); expect(first.cancelled).toBe(true);
    http.expectOne(req => req.url === url).flush(reviewCoverage(page)); fixture.detectChanges();
    expect(view.view()?.available).toBe(false); expect(fixture.nativeElement.textContent).toContain('אין נתוני קריאה מאומתים');
    expect(fixture.nativeElement.textContent).not.toContain('מתוך 0'); fixture.destroy(); http.verify();
  });
  it('bounds a stalled read and waits for explicit retry', () => {
    vi.useFakeTimers(); const { fixture, view, http } = create(), pending = http.expectOne(req => req.url === url);
    vi.advanceTimersByTime(20001); expect(pending.cancelled).toBe(true); expect(view.busy()).toBe(false); http.expectNone(url);
    fixture.destroy(); http.verify();
  });
});
