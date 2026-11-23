import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { SessionState } from '../../core/auth/session-state';
import { ReviewUnitsView } from './review-units-view';
import { REVIEW_SOURCE_DIGEST } from './review-source-state';
import { abcHash, sourceScope, sourceWindow, visitReceipt } from '../../../testing/review-source-fixtures';
import { product, proposal, snapshot, metadata, reviewUnits, other } from '../../../testing/review-fixtures';
import { reviewCoverage } from '../../../testing/review-coverage-fixtures';
import { unitApprovalPage, unitApprovalReceipt } from '../../../testing/unit-approval-fixtures';
import type { ReviewUnits, UnitApprovalRequest } from '../../core/api/review-api';

describe('Explicit individual approval controls', () => {
  const root = `/api/v1/proposals/${proposal}/review/snapshots/${snapshot}`, endpoint = `${root}/unit-approvals`;
  async function settle() { for (let i = 0; i < 8; i++) await Promise.resolve(); }
  function create(dual = false, selected?: ReviewUnits) {
    const profile = signal<{ user_id: string } | null>({ user_id: product }), logoutEpoch = signal(0), s = metadata();
    if (dual) s.metadata.assignments.push({ reviewer_id: product, domain: 'DESIGN', required: true });
    TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting(),
      { provide: SessionState, useValue: { profile, logoutEpoch } }, { provide: REVIEW_SOURCE_DIGEST, useValue: async () => abcHash }] });
    const fixture = TestBed.createComponent(ReviewUnitsView), view = fixture.componentInstance, http = TestBed.inject(HttpTestingController), denied = vi.fn();
    view.accessDenied.subscribe(denied); fixture.componentRef.setInput('actor', product); fixture.componentRef.setInput('proposal', proposal);
    fixture.componentRef.setInput('snapshot', s); fixture.componentRef.setInput('domains', dual ? ['PRODUCT', 'DESIGN'] : ['PRODUCT']); fixture.detectChanges();
    view.show(); const page = selected ?? reviewUnits(product, 0, 1); if (!selected) page.units[0] = sourceScope().unit;
    http.expectOne(req => req.url === `${root}/units`).flush(page); fixture.detectChanges();
    return { fixture, view, http, denied, profile, logoutEpoch };
  }
  function coverage(page: ReviewUnits, full = true) {
    const value = reviewCoverage(page);
    if (full) {
      for (const row of value.units) { if (row.before !== 'absent') row.before = 'viewed'; if (row.after !== 'absent') row.after = 'viewed'; row.viewed = true; }
      value.viewed_units = page.total; value.unviewed_units = 0;
    }
    return value;
  }
  function confirmed(page: ReviewUnits, body: Readonly<UnitApprovalRequest>) {
    const value = unitApprovalPage(page), receipt = unitApprovalReceipt(body), row = value.units.find(u => u.unit_id === body.unit_id);
    if (row) { row.approval_id = receipt.id; row.approved_at = receipt.recorded_at; }
    value.approved_units = 1; value.unapproved_units = page.total - 1; return value;
  }
  function open(c: ReturnType<typeof create>, full = true) {
    const button = [...c.fixture.nativeElement.querySelectorAll('button')].find((b: any) => b.textContent.includes('בדיקת האישורים שלי ואישור פריטים')) as HTMLButtonElement;
    button.click(); c.fixture.detectChanges(); const page = c.view.view()!;
    c.http.expectOne(req => req.url === `${root}/coverage`).flush(coverage(page, full));
    c.http.expectOne(req => req.method === 'GET' && req.url === endpoint).flush(unitApprovalPage(page)); c.fixture.detectChanges();
  }
  afterEach(() => vi.restoreAllMocks());
  it('records neither approval nor visit on opening, requires complete server reading and posts only from the explicit approval button', async () => {
    const c = create(), { fixture, view, http } = c; http.expectNone(req => req.url === endpoint); open(c, false);
    let button = [...fixture.nativeElement.querySelectorAll('button')].find((b: any) => b.textContent.includes('אישור הסעיף בגרסה זו')) as HTMLButtonElement;
    expect(button.disabled).toBe(true); button.click(); http.expectNone(req => req.method === 'POST');
    view.readSource(view.view()!.units[0], 'after'); http.expectOne(req => req.url.endsWith('/source')).flush(sourceWindow()); await settle(); fixture.detectChanges();
    view.acknowledgeSource(); const visit = http.expectOne(`${root}/visits`); visit.flush(visitReceipt(visit.request.body)); fixture.detectChanges();
    http.expectNone(req => req.method === 'POST'); http.expectOne(req => req.url === `${root}/coverage`).flush(coverage(view.view()!)); fixture.detectChanges();
    button = [...fixture.nativeElement.querySelectorAll('button')].find((b: any) => b.textContent.includes('אישור הסעיף בגרסה זו')) as HTMLButtonElement;
    expect(button.disabled).toBe(false); button.click(); const post = http.expectOne(endpoint), body = post.request.body;
    expect(Object.isFrozen(body)).toBe(true); expect(body.unit_id).toBe(view.view()!.units[0].id); expect(view.hasPending()).toBe(true);
    expect(fixture.nativeElement.textContent).not.toContain('נשמר אישור אישי לפריט זה'); post.flush(unitApprovalReceipt(body)); fixture.detectChanges();
    expect(fixture.nativeElement.textContent).not.toContain('נשמר אישור אישי לפריט זה');
    http.expectOne(req => req.method === 'GET' && req.url === endpoint).flush(confirmed(view.view()!, body)); fixture.detectChanges();
    expect(fixture.nativeElement.textContent).toContain('נשמר אישור אישי לפריט זה'); expect(fixture.nativeElement.textContent).toContain('אינו מפרסם אותה');
    expect(view.source.view()?.text).toBe('abc'); expect(view.hasPending()).toBe(false); fixture.destroy(); http.verify();
  });
  it('preserves an uncertain off-page approval and captured source while refreshing, paging and blocking domain or write retargeting', async () => {
    const c = create(true), { fixture, view, http } = c; open(c);
    const original = view.view()!.units[0]; view.readSource(original, 'after'); http.expectOne(req => req.url.endsWith('/source')).flush(sourceWindow()); await settle(); fixture.detectChanges();
    view.approveUnit(original); const post = http.expectOne(endpoint), body = post.request.body; post.error(new ProgressEvent('error')); fixture.detectChanges();
    expect(view.hasPending()).toBe(true); await settle(); fixture.detectChanges();
    expect((fixture.nativeElement.querySelector('select') as HTMLSelectElement).disabled).toBe(true);
    view.chooseDomain('DESIGN'); expect(view.domain()).toBe('PRODUCT'); view.acknowledgeSource(); http.expectNone(req => req.method === 'POST');
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(false); view.closeApprovals(); expect(view.approvals.pending()!.body).toBe(body);
    view.refresh(); fixture.detectChanges(); expect(view.approvals.pending()!.body).toBe(body); expect(view.source.view()?.text).toBe('abc');
    const page = reviewUnits(product, 0, 1); page.units[0] = sourceScope().unit;
    http.expectOne(req => req.url === `${root}/units`).flush(page); fixture.detectChanges();
    http.expectOne(req => req.url === `${root}/coverage`).flush(coverage(page)); http.expectOne(req => req.method === 'GET' && req.url === endpoint).flush(unitApprovalPage(page));
    fixture.componentRef.setInput('offset', 20); fixture.detectChanges(); const later = reviewUnits(product, 20, 22);
    http.expectOne(req => req.url === `${root}/units`).flush(later); fixture.detectChanges();
    http.expectOne(req => req.url === `${root}/coverage`).flush(coverage(later)); http.expectOne(req => req.method === 'GET' && req.url === endpoint).flush(unitApprovalPage(later));
    expect(view.approvals.pending()!.body).toBe(body); expect(view.source.scope()!.unit.id).toBe(original.id);
    view.approvals.retry(); const retry = http.expectOne(endpoint); expect(retry.request.body).toBe(body);
    retry.flush({ ...unitApprovalReceipt(body), replayed: true }); fixture.detectChanges();
    const read = http.expectOne(req => req.method === 'GET' && req.url === endpoint); expect(read.request.params.get('offset')).toBe('20'); read.flush(confirmed(later, body)); fixture.detectChanges();
    expect(view.approvals.result()?.unit.id).toBe(original.id); expect(view.source.view()?.text).toBe('abc'); expect(confirm).toHaveBeenCalledOnce(); fixture.destroy(); http.verify();
  });
  it.each([401, 403, 404, 409])('purges sibling source and requirements on approval %s denial or identity mismatch', async status => {
    const c = create(), { fixture, view, http, denied } = c; open(c); const unit = view.view()!.units[0];
    view.readSource(unit, 'after'); http.expectOne(req => req.url.endsWith('/source')).flush(sourceWindow()); await settle(); fixture.detectChanges();
    view.approveUnit(unit); http.expectOne(endpoint).flush({ error: { code: status === 409 ? 'SESSION_IDENTITY_CHANGED' : 'DENIED' } }, { status, statusText: 'Denied' });
    expect(view.sourceVisible()).toBe(false); expect(view.view()).toBeNull(); view.acknowledgeSource(); http.expectNone(req => req.method === 'POST'); fixture.detectChanges();
    expect(denied).toHaveBeenCalledOnce(); expect(view.hasPending()).toBe(false); expect(view.source.scope()).toBeNull(); fixture.destroy(); http.verify();
  });
  it('purges an uncertain approval immediately when a source read reports a session-identity mismatch', async () => {
    const c = create(), { fixture, view, http, denied } = c; open(c); const unit = view.view()!.units[0];
    view.readSource(unit, 'after'); http.expectOne(req => req.url.endsWith('/source')).flush(sourceWindow()); await settle(); fixture.detectChanges();
    view.approveUnit(unit); http.expectOne(endpoint).error(new ProgressEvent('error')); view.source.refresh();
    http.expectOne(req => req.url.endsWith('/source')).flush({ error: { code: 'SESSION_IDENTITY_CHANGED' } }, { status: 409, statusText: 'Changed' });
    expect(view.approvals.pending()).toBeNull(); view.approvals.retry(); http.expectNone(req => req.method === 'POST'); fixture.detectChanges();
    expect(denied).toHaveBeenCalledOnce(); expect(view.view()).toBeNull(); expect(view.hasPending()).toBe(false); fixture.destroy(); http.verify();
  });
  it('keeps the pending domain when eligibility hints change and selects the eligible domain only after receipt validation', () => {
    const c = create(true), { fixture, view, http } = c; open(c); view.approveUnit(view.view()!.units[0]);
    const post = http.expectOne(endpoint), body = post.request.body; post.error(new ProgressEvent('error'));
    fixture.componentRef.setInput('domains', ['DESIGN']); fixture.detectChanges(); expect(view.domain()).toBe('PRODUCT'); expect(view.approvals.pending()!.body).toBe(body);
    view.approvals.retry(); http.expectOne(endpoint).flush(unitApprovalReceipt(body)); fixture.detectChanges(); expect(view.domain()).toBe('DESIGN');
    http.expectOne(req => req.url === `${root}/coverage` && req.params.get('domain') === 'DESIGN').flush(reviewCoverage(view.view()!, 'DESIGN'));
    http.expectOne(req => req.method === 'GET' && req.url === endpoint && req.params.get('domain') === 'DESIGN').flush(unitApprovalPage(view.view()!, 'DESIGN'));
    expect(view.hasPending()).toBe(false); fixture.destroy(); http.verify();
  });
  it('cancels and hides old private approval state after the account changes', () => {
    const c = create(), { fixture, view, http, profile } = c; open(c); view.approveUnit(view.view()!.units[0]); const post = http.expectOne(endpoint);
    profile.set({ user_id: other }); post.flush(unitApprovalReceipt(post.request.body)); expect(view.approvals.result()).toBeNull(); expect(view.approvals.pending()).toBeNull();
    fixture.detectChanges(); expect(view.view()).toBeNull(); expect(view.hasPending()).toBe(false); http.expectNone(req => req.method === 'GET'); fixture.destroy(); http.verify();
  });
  it('abandons only after explicit confirmation and reopens with fresh server reads', () => {
    const c = create(), { fixture, view, http } = c; open(c); view.approveUnit(view.view()!.units[0]); http.expectOne(endpoint).error(new ProgressEvent('error'));
    vi.spyOn(window, 'confirm').mockReturnValue(true); view.closeApprovals(); fixture.detectChanges(); expect(view.hasPending()).toBe(false); expect(view.approvalsOpened()).toBe(false);
    view.showApprovals(); fixture.detectChanges(); http.expectOne(req => req.method === 'GET' && req.url === endpoint).flush(unitApprovalPage(view.view()!));
    http.expectNone(req => req.method === 'POST'); fixture.destroy(); http.verify();
  });
  it('offers a distinct explicit file-level approval and displays only confirmed server status', () => {
    const page = reviewUnits(product, 0, 1); page.units[0].requirement = { kind: 'file', reason: 'non_section' };
    const c = create(false, page), { fixture, view, http } = c; open(c);
    const button = [...fixture.nativeElement.querySelectorAll('button')].find((b: any) => b.textContent.includes('אישור הפריט בגרסה זו')) as HTMLButtonElement;
    expect(button.disabled).toBe(false); button.click(); const post = http.expectOne(endpoint), body = post.request.body;
    post.flush(unitApprovalReceipt(body)); http.expectOne(req => req.method === 'GET' && req.url === endpoint).flush(confirmed(page, body)); fixture.detectChanges();
    expect(fixture.nativeElement.textContent).toContain('נשמר אישור אישי לפריט זה'); fixture.destroy(); http.verify();
  });
  it('never enables unsupported media or historical approval evidence', () => {
    const page = reviewUnits(product, 0, 1); page.units[0].requirement = { kind: 'file', reason: 'non_document' }; page.units[0].before = null;
    page.units[0].after = { source: { path: `assets/${'3'.repeat(64)}.png`, blob_oid: 'a'.repeat(40), bytes: 10, executable: false }, range: null };
    const c = create(false, page), { fixture, view, http } = c; open(c, false);
    const button = [...fixture.nativeElement.querySelectorAll('button')].find((b: any) => b.textContent.includes('אישור הפריט בגרסה זו')) as HTMLButtonElement;
    expect(button.disabled).toBe(true); button.click(); view.approveUnit(view.view()!.units[0]); http.expectNone(req => req.method === 'POST');
    view.approvals.refresh(); http.expectOne(req => req.method === 'GET' && req.url === endpoint).flush({ ...unitApprovalPage(page), current_for_metadata: false }); fixture.detectChanges();
    expect(fixture.nativeElement.textContent).toContain('נדרשת סקירה מחדש בגרסה העדכנית'); expect(view.canApproveUnit(view.view()!.units[0])).toBe(false);
    fixture.destroy(); http.verify();
  });
  it('rechecks eligibility hints synchronously before a queued approval action', () => {
    const c = create(), { fixture, view, http } = c; open(c); const unit = view.view()!.units[0];
    fixture.componentRef.setInput('domains', []); view.approveUnit(unit); http.expectNone(req => req.method === 'POST');
    fixture.detectChanges(); expect(view.approvals.view()).toBeNull(); fixture.destroy(); http.verify();
  });
  it('does not mistake an approved status read for confirmation of the original uncertain nonce', () => {
    const c = create(), { fixture, view, http } = c; open(c); const page = view.view()!; view.approveUnit(page.units[0]);
    const post = http.expectOne(endpoint), body = post.request.body; post.error(new ProgressEvent('error'));
    view.approvals.refresh(); http.expectOne(req => req.method === 'GET' && req.url === endpoint).flush(confirmed(page, body)); fixture.detectChanges();
    expect(view.approvals.view()?.approved_units).toBe(1); expect(view.approvals.pending()!.body).toBe(body); expect(view.hasPending()).toBe(true);
    http.expectNone(req => req.method === 'POST'); view.approvals.retry(); const retry = http.expectOne(endpoint); expect(retry.request.body).toBe(body);
    retry.flush({ ...unitApprovalReceipt(body), replayed: true }); http.expectOne(req => req.method === 'GET' && req.url === endpoint).flush(confirmed(page, body));
    expect(view.hasPending()).toBe(false); fixture.destroy(); http.verify();
  });
});
