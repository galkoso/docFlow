import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { SessionState } from '../../core/auth/session-state';
import { product, proposal, metadata, other, snapshot } from '../../../testing/review-fixtures';
import { storedReadiness } from '../../../testing/readiness-fixtures';
import { ReviewReadinessView, validStoredReadiness } from './review-readiness-view';

describe('Stored readiness display', () => {
  let transport: HttpTestingController | null = null;
  it('accepts stored pending evidence without inventing provider verification', () => {
    expect(validStoredReadiness(storedReadiness(), product, proposal, metadata())).toBe(true);
  });
  it.each(['actor', 'snapshot', 'fingerprint', 'provider', 'success', 'missing', 'duplicate', 'status', 'gate'])('rejects mismatched %s readiness', fault => {
    const value = storedReadiness();
    if (fault === 'actor') value.actor_id = other;
    if (fault === 'snapshot') value.snapshot_id = other;
    if (fault === 'fingerprint') value.fingerprint = 'f'.repeat(64);
    if (fault === 'provider') value.provider_refs_verified = true;
    if (fault === 'success') value.evaluation.conclusion = 'SUCCESS';
    if (fault === 'missing') value.evaluation.issues = [];
    if (fault === 'duplicate') value.evaluation.issues.push(structuredClone(value.evaluation.issues[0]));
    if (fault === 'status') value.evaluation.issues[0].conclusion = 'FAILURE';
    if (fault === 'gate') value.evaluation.issues[0].gate = 'UNKNOWN' as never;
    expect(validStoredReadiness(value, product, proposal, metadata())).toBe(false);
  });
  const root = `/api/v1/proposals/${proposal}/review/snapshots/${snapshot}/readiness`;
  afterEach(() => { transport?.verify({ ignoreCancelled: true }); transport = null; });
  async function create() {
    const profile = signal<{ user_id: string } | null>({ user_id: product }), logoutEpoch = signal(0);
    await TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting(), { provide: SessionState, useValue: { profile, logoutEpoch } }] }).compileComponents();
    const fixture = TestBed.createComponent(ReviewReadinessView), view = fixture.componentInstance, http = TestBed.inject(HttpTestingController);
    transport = http;
    fixture.componentRef.setInput('actor', product); fixture.componentRef.setInput('proposal', proposal); fixture.componentRef.setInput('snapshot', metadata()); fixture.detectChanges();
    return { fixture, view, http, profile, logoutEpoch };
  }
  it('reads only on request, refreshes loaded reports and clears facts after a failed refresh', async () => {
    const { fixture, view, http } = await create(); view.refreshIfLoaded(); http.expectNone(root);
    view.refresh(); http.expectOne(root).flush(storedReadiness(product, true)); fixture.detectChanges();
    expect(fixture.nativeElement.textContent).toContain('שאלות מוצר חוסמות'); expect(fixture.nativeElement.textContent).toContain('אין כאן אישור למיזוג');
    view.refreshIfLoaded(); expect(view.view()).toBeNull(); http.expectOne(root).flush({}, { status: 503, statusText: 'Unavailable' });
    expect(view.view()).toBeNull(); expect(view.error()).toContain('לא ניתן'); http.expectNone(r => r.method === 'POST');
  });
  it('cancels old snapshot requests and blocks queued refreshes on account change', async () => {
    const { fixture, view, http, profile } = await create(); view.refresh(); const old = http.expectOne(root);
    const value = metadata(); value.snapshot_id = other; fixture.componentRef.setInput('snapshot', value); fixture.detectChanges();
    expect(old.cancelled).toBe(true); expect(view.view()).toBeNull();
    profile.set({ user_id: other }); view.refresh(); http.expectNone(r => r.url.endsWith('/readiness')); fixture.detectChanges();
  });
  it('purges stored facts and notifies its parent on scoped denial', async () => {
    const { view, http } = await create(); const denied = vi.fn(); view.accessDenied.subscribe(denied);
    view.refresh(); http.expectOne(root).flush({}, { status: 404, statusText: 'Denied' });
    expect(view.view()).toBeNull(); expect(denied).toHaveBeenCalledOnce();
  });
});
