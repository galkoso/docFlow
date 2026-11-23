import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { SessionState } from '../../core/auth/session-state';
import { ReviewUnitsView } from './review-units-view';
import { REVIEW_SOURCE_DIGEST } from './review-source-state';
import { abcHash, sourceScope, sourceWindow, visitReceipt } from '../../../testing/review-source-fixtures';
import { product, proposal, snapshot, metadata, reviewUnits } from '../../../testing/review-fixtures';
import { reviewCoverage } from '../../../testing/review-coverage-fixtures';

describe('Required-source reading controls', () => {
  const root = `/api/v1/proposals/${proposal}/review/snapshots/${snapshot}`;
  async function settle() { for (let i = 0; i < 8; i++) await Promise.resolve(); }
  function create(eligible = true) {
    const profile = signal<{ user_id: string } | null>({ user_id: product }), logoutEpoch = signal(0);
    TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting(),
      { provide: SessionState, useValue: { profile, logoutEpoch } }, { provide: REVIEW_SOURCE_DIGEST, useValue: async () => abcHash }] });
    const fixture = TestBed.createComponent(ReviewUnitsView), view = fixture.componentInstance, http = TestBed.inject(HttpTestingController), denied = vi.fn();
    view.accessDenied.subscribe(denied); fixture.componentRef.setInput('actor', product); fixture.componentRef.setInput('proposal', proposal);
    fixture.componentRef.setInput('snapshot', metadata()); fixture.componentRef.setInput('domains', eligible ? ['PRODUCT'] : []); fixture.detectChanges();
    view.show(); const page = reviewUnits(product, 0, 1); page.units[0] = sourceScope().unit;
    http.expectOne(req => req.url === `${root}/units`).flush(page); fixture.detectChanges();
    return { fixture, view, http, denied, profile };
  }
  it('offers source recording only for eligible domains and renders a verified source without automatically acknowledging it', async () => {
    const { fixture, view, http } = create(); const unit = view.view()!.units[0];
    view.readSource({ ...unit }, 'after'); http.expectNone(req => req.url.endsWith('/source'));
    view.readSource(unit, 'after'); const read = http.expectOne(req => req.url.endsWith('/source'));
    expect(read.request.params.get('domain')).toBe('PRODUCT'); read.flush(sourceWindow()); await settle(); fixture.detectChanges();
    const pre = fixture.nativeElement.querySelector('.source-reader pre') as HTMLElement;
    expect(pre.textContent).toBe('abc'); expect(pre.getAttribute('tabindex')).toBe('0'); expect(pre.getAttribute('dir')).toBe('auto');
    expect(fixture.nativeElement.textContent).toContain('אינו אישור סעיף או הצעה'); http.expectNone(req => req.method === 'POST');
    const button = [...fixture.nativeElement.querySelectorAll('button')].find((b: any) => b.textContent.includes('קראתי את המקטע')) as HTMLButtonElement;
    button.click(); const ack = http.expectOne(`${root}/visits`); ack.flush(visitReceipt(ack.request.body)); fixture.detectChanges();
    expect(fixture.nativeElement.textContent).toContain('קריאת המקטע תועדה'); expect(view.hasPending()).toBe(false);
    fixture.destroy(); http.verify();
  });
  it('keeps reading links for members without offering recording controls or fetching source', () => {
    const { fixture, view, http } = create(false); view.readSource(view.view()!.units[0], 'after');
    expect(fixture.nativeElement.querySelectorAll('a').length).toBeGreaterThan(0);
    expect(fixture.nativeElement.querySelector('.source-reader')).toBeNull(); http.expectNone(req => req.url.endsWith('/source')); fixture.destroy(); http.verify();
  });
  it('keeps one uncertain acknowledgement captured while refreshing or paging the requirement list', async () => {
    const { fixture, view, http } = create(); view.readSource(view.view()!.units[0], 'after');
    http.expectOne(req => req.url.endsWith('/source')).flush(sourceWindow()); await settle(); fixture.detectChanges(); view.source.acknowledge();
    const post = http.expectOne(`${root}/visits`), body = post.request.body; post.error(new ProgressEvent('error'));
    view.refresh(); http.expectOne(req => req.url === `${root}/units`).flush(reviewUnits(product, 0, 1)); fixture.detectChanges();
    expect(view.source.pending()!.body).toBe(body); expect(view.source.scope()!.unit.id).toBe(sourceScope().unit.id);
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(false); view.closeSource(); expect(view.source.pending()!.body).toBe(body);
    view.chooseDomain('DEVELOPMENT'); expect(view.source.scope()!.domain).toBe('PRODUCT');
    view.source.retry(); const retry = http.expectOne(`${root}/visits`); expect(retry.request.body).toBe(body);
    retry.flush({ ...visitReceipt(body), replayed: true }); expect(confirm).toHaveBeenCalledOnce(); confirm.mockRestore();
    fixture.destroy(); http.verify();
  });
  it('purges a source pane and relays scoped denial to its parent', async () => {
    const { fixture, view, http, denied } = create(); view.readSource(view.view()!.units[0], 'after');
    http.expectOne(req => req.url.endsWith('/source')).flush({}, { status: 403, statusText: 'Forbidden' }); await settle(); fixture.detectChanges();
    expect(denied).toHaveBeenCalledOnce(); expect(view.source.scope()).toBeNull(); expect(fixture.nativeElement.querySelector('.source-reader')).toBeNull();
    fixture.destroy(); http.verify();
  });
  it('shows source markup as inert text and never creates remote image or script elements', async () => {
    const { fixture, view, http } = create(); const text = '<script>bad()</script><img src=x onerror=bad()>', unit = view.view()!.units[0];
    unit.after!.source.bytes = text.length + 1; unit.after!.range!.end_byte = text.length + 1;
    view.readSource(unit, 'after'); const scope = view.source.scope()!, w = { ...sourceWindow(scope), text };
    http.expectOne(req => req.url.endsWith('/source')).flush(w); await settle(); fixture.detectChanges();
    expect(fixture.nativeElement.querySelector('.source-reader pre').textContent).toBe(text);
    expect(fixture.nativeElement.querySelector('.source-reader img, .source-reader script')).toBeNull();
    fixture.destroy(); http.verify();
  });
  it('loads coverage explicitly and preserves an uncertain acknowledgement while refreshing server counts', async () => {
    const { fixture, view, http } = create(), page = view.view()!;
    http.expectNone(req => req.url.endsWith('/coverage'));
    const open = [...fixture.nativeElement.querySelectorAll('button')].find((b: any) => b.textContent.includes('בדיקת מצב הקריאה שלי')) as HTMLButtonElement;
    open.click(); fixture.detectChanges(); const partial = reviewCoverage(page); partial.units[0].after = 'partial';
    http.expectOne(`${root}/coverage?expected_fingerprint=${page.fingerprint}&domain=PRODUCT&offset=0&limit=20`).flush(partial);
    fixture.detectChanges(); expect(fixture.nativeElement.textContent).toContain('תועדה קריאה חלקית');
    expect(fixture.nativeElement.textContent).not.toContain('קריאת הפריט תועדה במלואה');
    view.readSource(page.units[0], 'after'); http.expectOne(req => req.url.endsWith('/source')).flush(sourceWindow()); await settle(); fixture.detectChanges();
    view.source.acknowledge(); const post = http.expectOne(`${root}/visits`), body = post.request.body;
    post.error(new ProgressEvent('error')); fixture.detectChanges(); http.expectNone(req => req.url.endsWith('/coverage'));
    view.coveragePanel()!.refresh(); http.expectOne(req => req.url.endsWith('/coverage')).flush(partial); fixture.detectChanges();
    expect(view.source.pending()!.body).toBe(body); expect(view.source.view()?.text).toBe('abc');
    view.source.retry(); const retry = http.expectOne(`${root}/visits`); expect(retry.request.body).toBe(body);
    retry.flush({ ...visitReceipt(body), replayed: true }); fixture.detectChanges();
    expect(fixture.nativeElement.textContent).not.toContain('קריאת הפריט תועדה במלואה');
    const complete = reviewCoverage(page); complete.units[0] = { ...complete.units[0], before: 'viewed', after: 'viewed', viewed: true };
    complete.viewed_units = 1; complete.unviewed_units = 0;
    http.expectOne(req => req.url.endsWith('/coverage')).flush(complete); fixture.detectChanges();
    expect(fixture.nativeElement.textContent).toContain('קריאת הפריט תועדה במלואה');
    view.closeSource(); fixture.detectChanges(); http.expectNone(req => req.url.endsWith('/coverage'));
    expect(view.coveragePanel()!.view()?.viewed_units).toBe(1); fixture.destroy(); http.verify();
  });
  it.each([401, 403, 404])('clears source and requirement evidence when coverage reports %s', async status => {
    const { fixture, view, http, denied } = create(); view.readSource(view.view()!.units[0], 'after');
    http.expectOne(req => req.url.endsWith('/source')).flush(sourceWindow()); await settle(); fixture.detectChanges();
    view.source.acknowledge(); http.expectOne(`${root}/visits`).error(new ProgressEvent('error')); expect(view.hasPending()).toBe(true);
    view.coverageOpened.set(true); fixture.detectChanges();
    http.expectOne(req => req.url.endsWith('/coverage')).flush({}, { status, statusText: 'Denied' }); fixture.detectChanges();
    expect(denied).toHaveBeenCalledOnce(); expect(view.source.scope()).toBeNull(); expect(view.view()).toBeNull();
    expect(view.hasPending()).toBe(false); view.refresh(); http.expectNone(req => req.url.endsWith('/units'));
    fixture.destroy(); http.verify();
  });
  it('keeps the captured domain during an uncertain write and selects an eligible domain after resolution', async () => {
    const { fixture, view, http } = create(); view.readSource(view.view()!.units[0], 'after');
    http.expectOne(req => req.url.endsWith('/source')).flush(sourceWindow()); await settle(); fixture.detectChanges();
    view.source.acknowledge(); const post = http.expectOne(`${root}/visits`), body = post.request.body; post.error(new ProgressEvent('error'));
    fixture.componentRef.setInput('domains', ['DEVELOPMENT']); fixture.detectChanges();
    expect(view.domain()).toBe('PRODUCT'); expect(view.source.pending()!.body).toBe(body);
    view.source.retry(); http.expectOne(`${root}/visits`).flush(visitReceipt(body)); fixture.detectChanges();
    expect(view.domain()).toBe('DEVELOPMENT'); expect(view.source.scope()).toBeNull(); http.expectNone(req => req.url.endsWith('/coverage'));
    fixture.destroy(); http.verify();
  });
});
