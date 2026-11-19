import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { SessionState } from '../../core/auth/session-state';
import { actor, proposal, snapshot, other, head, base, metadata, reviewUnits } from '../../../testing/review-fixtures';
import { ReviewUnitsView } from './review-units-view';

describe('Mandatory review requirements view', () => {
  const url = `/api/v1/proposals/${proposal}/review/snapshots/${snapshot}/units`;
  function create(offset = 0) {
    const profile = signal<{ user_id: string } | null>({ user_id: actor }), logoutEpoch = signal(0);
    TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting(),
      { provide: SessionState, useValue: { profile, logoutEpoch } }] });
    const fixture = TestBed.createComponent(ReviewUnitsView), view = fixture.componentInstance, http = TestBed.inject(HttpTestingController);
    const denied = vi.fn(), selected = vi.fn(); view.accessDenied.subscribe(denied); view.offsetSelected.subscribe(selected);
    fixture.componentRef.setInput('actor', actor); fixture.componentRef.setInput('proposal', proposal);
    fixture.componentRef.setInput('snapshot', metadata()); fixture.componentRef.setInput('offset', offset); fixture.detectChanges();
    const show = () => { view.show(); fixture.detectChanges(); return http.expectOne(req => req.url === url); };
    return { fixture, view, http, profile, logoutEpoch, denied, selected, show };
  }
  afterEach(() => vi.useRealTimers());
  it('loads only on demand and renders exact source ranges with pinned before/after links', () => {
    const { fixture, view, http, show } = create(); http.expectNone(url);
    const request = show(); expect(request.request.headers.get('X-Expected-Actor')).toBe(actor);
    request.flush(reviewUnits()); fixture.detectChanges();
    expect(fixture.nativeElement.textContent).toContain('שורות 1–3');
    expect(fixture.nativeElement.textContent).toContain('DOC-A');
    const links = [...fixture.nativeElement.querySelectorAll('a')] as HTMLAnchorElement[];
    const before = new URL(links[0].href), after = new URL(links[1].href);
    expect(before.pathname).toBe('/specification'); expect(before.searchParams.get('commit')).toBe(base);
    expect(before.searchParams.get('repo')).toBe(metadata().metadata.summary.repository_id);
    expect(after.pathname).toBe(`/changes/${proposal}/book`); expect(after.searchParams.get('commit')).toBe(head);
    for (const link of links) { expect(link.target).toBe('_blank'); expect(link.rel).toBe('noopener noreferrer'); }
    expect(after.searchParams.get('section')).toBe('SEC-0'); expect(after.searchParams.get('doc')).toBe('DOC-A');
    expect(view.bookHref({ ...view.view()!.units[0] }, 'after')).toBeNull();
    http.expectNone(req => req.method !== 'GET'); fixture.destroy(); http.verify();
  });
  it('restores a URL page, emits bounded paging intent and cancels reads on the next page', () => {
    const { fixture, view, http, selected } = create(20), first = http.expectOne(req => req.url === url);
    expect(first.request.params.get('offset')).toBe('20'); first.flush(reviewUnits(actor, 20, 42)); fixture.detectChanges();
    view.move(40); view.move(999); expect(selected.mock.calls).toEqual([[40]]);
    fixture.componentRef.setInput('offset', 40); fixture.detectChanges();
    expect(view.view()).toBeNull(); const pending = http.expectOne(req => req.url === url);
    fixture.componentRef.setInput('offset', 0); fixture.detectChanges(); expect(pending.cancelled).toBe(true);
    expect(view.opened()).toBe(true); expect(view.view()).toBeNull();
    const start = http.expectOne(req => req.url === url); expect(start.request.params.get('offset')).toBe('0');
    start.flush(reviewUnits(actor, 0, 42)); expect(view.view()?.offset).toBe(0); fixture.destroy(); http.verify();
  });
  it.each(['actor', 'logout', 'snapshot', 'fingerprint', 'offset'])('rejects a late response immediately after %s changes', fault => {
    const { fixture, view, http, show, profile, logoutEpoch } = create(), pending = show();
    if (fault === 'actor') profile.set({ user_id: other });
    if (fault === 'logout') logoutEpoch.update(n => n + 1);
    if (fault === 'snapshot') fixture.componentRef.setInput('snapshot', { ...metadata(), snapshot_id: other });
    if (fault === 'fingerprint') fixture.componentRef.setInput('snapshot', { ...metadata(), fingerprint: 'a'.repeat(64) });
    if (fault === 'offset') fixture.componentRef.setInput('offset', 20);
    pending.flush(reviewUnits()); expect(view.view()).toBeNull(); fixture.destroy(); http.verify();
  });
  it.each([401, 403, 404])('purges and reports denied access for %s without automatically retrying', status => {
    const { fixture, view, http, denied, show } = create(); show().flush({}, { status, statusText: 'Denied' }); fixture.detectChanges();
    expect(view.view()).toBeNull(); expect(denied).toHaveBeenCalledOnce(); view.refresh(); http.expectNone(url); fixture.destroy(); http.verify();
  });
  it('retains only retryable local errors and rejects malformed evidence', () => {
    const { fixture, view, http, denied, show } = create(); show().flush({}, { status: 503, statusText: 'Unavailable' });
    expect(view.error()).toContain('לא ניתן'); view.refresh(); http.expectOne(req => req.url === url).flush({ ...reviewUnits(), actor_id: other });
    expect(view.error()).toContain('אינם תואמים'); expect(view.view()).toBeNull(); expect(denied).not.toHaveBeenCalled();
    view.refresh(); http.expectOne(req => req.url === url).flush(reviewUnits()); expect(view.error()).toBe('');
    fixture.destroy(); http.verify();
  });
  it('shows unavailable legacy evidence and missing sides without inventing coverage or links', () => {
    const { fixture, view, http, show } = create(); show().flush({ ...reviewUnits(), available: false, total: 0, units: [], next_offset: null });
    fixture.detectChanges(); expect(fixture.nativeElement.textContent).toContain('גרסה היסטורית');
    expect(fixture.nativeElement.querySelector('ol')).toBeNull();
    view.refresh(); const value = reviewUnits(); value.units[0].after = null;
    http.expectOne(req => req.url === url).flush(value); fixture.detectChanges();
    expect(fixture.nativeElement.textContent).toContain('הפריט אינו קיים בצד זה');
    expect(view.bookHref(view.view()!.units[0], 'after')).toBeNull(); fixture.destroy(); http.verify();
  });
  it('renders file-level work and untrusted path text inertly without claiming section completion', () => {
    const { fixture, view, http, show } = create(), value = reviewUnits();
    value.units[0].requirement = { kind: 'file', reason: 'layout' };
    value.units[0].after!.source.path = 'docs/<img src=x onerror=bad()>.md';
    value.units[1] = { id: value.units[1].id, file_id: value.units[1].file_id,
      requirement: { kind: 'file', reason: 'non_document' }, before: null,
      after: { source: { path: `assets/${'3'.repeat(64)}.png`, bytes: 100, blob_oid: head, executable: false }, range: null } };
    show().flush(value); fixture.detectChanges();
    expect(fixture.nativeElement.textContent).toContain('סדר הסעיפים');
    expect(fixture.nativeElement.textContent).toContain('<img src=x onerror=bad()>.md');
    expect(fixture.nativeElement.querySelector('img')).toBeNull();
    expect(view.bookHref(view.view()!.units[1], 'after')).toBeNull();
    expect(fixture.nativeElement.textContent).toContain('יש לבדוק את הקובץ בהשוואת הקבצים');
    expect(fixture.nativeElement.querySelectorAll('input').length).toBe(0); http.expectNone(req => req.method !== 'GET'); fixture.destroy(); http.verify();
  });
  it('bounds a stalled read and waits for explicit retry without recording any action', () => {
    vi.useFakeTimers(); const { fixture, view, http, denied, show } = create(), pending = show();
    vi.advanceTimersByTime(20_001); expect(pending.cancelled).toBe(true); expect(view.busy()).toBe(false);
    expect(view.error()).toContain('לא ניתן'); expect(denied).not.toHaveBeenCalled(); http.expectNone(url);
    view.refresh(); http.expectOne(req => req.url === url).flush(reviewUnits()); expect(view.view()).not.toBeNull();
    fixture.destroy(); http.verify();
  });
  it('hides an already rendered page synchronously on identity loss and cancels pending refreshes', () => {
    const { fixture, view, http, show, profile } = create(); show().flush(reviewUnits()); expect(view.view()).not.toBeNull();
    view.refresh(); const pending = http.expectOne(req => req.url === url); profile.set(null); expect(view.view()).toBeNull();
    fixture.detectChanges(); expect(pending.cancelled).toBe(true); fixture.destroy(); http.verify();
  });
});
