import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { SessionState } from '../../core/auth/session-state';
import type { UnitApprovalRequest } from '../../core/api/review-api';
import { UnitApprovalState, type ApprovalReadScope } from './unit-approval-state';
import { metadata, product, proposal, snapshot, other, reviewUnits } from '../../../testing/review-fixtures';
import { unitApprovalPage, unitApprovalReceipt } from '../../../testing/unit-approval-fixtures';
import { reviewCoverage } from '../../../testing/review-coverage-fixtures';

describe('Captured individual approval state', () => {
  const url = `/api/v1/proposals/${proposal}/review/snapshots/${snapshot}/unit-approvals`;
  const selection = (): ApprovalReadScope => ({ actor: product, proposal, snapshot: metadata(), page: reviewUnits(product), domain: 'PRODUCT' });
  function complete(scope = selection()) {
    const coverage = reviewCoverage(scope.page, scope.domain);
    for (const unit of coverage.units) { if (unit.before !== 'absent') unit.before = 'viewed'; if (unit.after !== 'absent') unit.after = 'viewed'; unit.viewed = true; }
    coverage.viewed_units = coverage.total_units; coverage.unviewed_units = 0; return coverage;
  }
  function create(scope = selection()) {
    const profile = signal<{ user_id: string } | null>({ user_id: product }), logoutEpoch = signal(0), live = signal(true);
    TestBed.configureTestingModule({ providers: [UnitApprovalState, provideHttpClient(), provideHttpClientTesting(),
      { provide: SessionState, useValue: { profile, logoutEpoch } }] });
    const state = TestBed.inject(UnitApprovalState), http = TestBed.inject(HttpTestingController);
    state.configure(scope, () => live()); return { state, http, profile, logoutEpoch, live, scope };
  }
  function loaded(scope = selection()) {
    const c = create(scope); c.http.expectOne(req => req.method === 'GET' && req.url === url).flush(unitApprovalPage(scope.page, scope.domain)); return c;
  }
  function approved(body: Readonly<UnitApprovalRequest>, page = reviewUnits(product)) {
    const value = unitApprovalPage(page), receipt = unitApprovalReceipt(body), row = value.units.find(u => u.unit_id === receipt.unit_id);
    if (row) { row.approval_id = receipt.id; row.approved_at = receipt.recorded_at; }
    value.approved_units = 1; value.unapproved_units = value.total_units - 1; return value;
  }
  afterEach(() => { vi.useRealTimers(); vi.restoreAllMocks(); });
  it('reads exact status without posting, then freezes one explicitly selected unit and validates its receipt before refreshing counts', () => {
    const { state, http, scope } = loaded(); const coverage = complete(scope), id = scope.page.units[0].id;
    expect(state.canApprove(id, coverage)).toBe(true); http.expectNone(req => req.method === 'POST');
    state.approve('0'.repeat(64), coverage); http.expectNone(req => req.method === 'POST'); state.approve(id, coverage); state.approve(id, coverage);
    const post = http.expectOne(url), body = post.request.body;
    expect(post.request.headers.get('X-Expected-Actor')).toBe(product); expect(Object.isFrozen(body)).toBe(true);
    expect(Object.isFrozen(state.pending()!.capture.scope.snapshot.metadata.assignments)).toBe(true);
    scope.page.units[0].after!.source.path = 'docs/changed.md'; expect(state.pending()!.unit.after!.source.path).toBe('docs/a.md');
    post.flush(unitApprovalReceipt(body)); expect(state.pending()).toBeNull(); expect(state.view()).toBeNull(); expect(state.result()?.receipt.unit_id).toBe(id);
    const summary = unitApprovalPage(); summary.units[0] = { unit_id: id, approval_id: state.result()!.receipt.id, approved_at: state.result()!.receipt.recorded_at };
    summary.approved_units = 1; summary.unapproved_units = 1; http.expectOne(req => req.method === 'GET').flush(summary);
    expect(state.view()?.approved_units).toBe(1); expect(state.canApprove(id, coverage)).toBe(false); state.clear(); http.verify();
  });
  it('requires matching complete current coverage and current unapproved status', () => {
    const { state, http, scope } = loaded(), id = scope.page.units[0].id;
    for (const value of [null, reviewCoverage(), { ...complete(), current_for_metadata: false }, { ...complete(), domain: 'DESIGN' as const }]) {
      state.approve(id, value); expect(state.canApprove(id, value)).toBe(false);
    }
    http.expectNone(req => req.method === 'POST'); state.refresh();
    http.expectOne(req => req.method === 'GET').flush({ ...unitApprovalPage(), current_for_metadata: false });
    expect(state.canApprove(id, complete())).toBe(false); state.clear(); http.verify();
  });
  it('keeps unavailable legacy and non-current snapshot evidence ineligible', () => {
    const scope = selection(); scope.snapshot.current_metadata = false;
    const { state, http } = loaded(scope); expect(state.canApprove(scope.page.units[0].id, complete(scope))).toBe(false);
    const legacy = selection(); legacy.page = { ...legacy.page, available: false, total: 0, next_offset: null, units: [] };
    state.configure(legacy, () => true); http.expectOne(req => req.method === 'GET').flush(unitApprovalPage(legacy.page));
    expect(state.view()?.available).toBe(false); state.approve(scope.page.units[0].id, complete()); http.expectNone(req => req.method === 'POST'); state.clear(); http.verify();
  });
  it('preserves one uncertain nonce through list removal, paging and status refresh and retries the captured off-page unit', () => {
    const { state, http, scope } = loaded(); state.approve(scope.page.units[0].id, complete(scope));
    const post = http.expectOne(url), body = post.request.body; post.error(new ProgressEvent('error'));
    state.configure(null, () => true); expect(state.hasPending()).toBe(true); expect(state.view()).toBeNull();
    const next = selection(); next.page = reviewUnits(product, 20, 22); state.configure(next, () => true);
    http.expectOne(req => req.method === 'GET').flush(unitApprovalPage(next.page)); state.refresh();
    http.expectOne(req => req.method === 'GET').flush({}, { status: 503, statusText: 'Unavailable' });
    expect(state.pending()!.body).toBe(body); state.approve(next.page.units[0].id, complete(next)); http.expectNone(req => req.method === 'POST');
    state.retry(); const retry = http.expectOne(url); expect(retry.request.body).toBe(body);
    retry.flush({ ...unitApprovalReceipt(body), replayed: true });
    const read = http.expectOne(req => req.method === 'GET'); expect(read.request.params.get('offset')).toBe('20'); read.flush(approved(body, next.page));
    expect(state.result()?.unit.id).toBe(body.unit_id); state.clear(); http.verify();
  });
  it('rejects domain retargeting while an approval is uncertain', () => {
    const { state, http, scope } = loaded(); state.approve(scope.page.units[0].id, complete());
    const post = http.expectOne(url), body = post.request.body; post.error(new ProgressEvent('error'));
    const next = selection(); next.domain = 'DESIGN'; next.snapshot.metadata.assignments.push({ reviewer_id: product, domain: 'DESIGN', required: true });
    state.configure(next, () => true); http.expectNone(req => req.method === 'GET'); expect(state.pending()!.body).toBe(body);
    state.retry(); const retry = http.expectOne(url); expect(retry.request.body.domain).toBe('PRODUCT'); retry.flush(unitApprovalReceipt(body));
    http.expectNone(req => req.method === 'GET'); state.clear(); http.verify();
  });
  it('retains malformed receipts and unknown conflicts for explicit same-intent retry', () => {
    const { state, http, scope } = loaded(); state.approve(scope.page.units[0].id, complete());
    const post = http.expectOne(url), body = post.request.body; post.flush({ ...unitApprovalReceipt(body), actor_id: other });
    expect(state.pending()!.body).toBe(body); expect(state.result()).toBeNull();
    state.retry(); const retry = http.expectOne(url); retry.flush({ error: { code: 'UNKNOWN_CONFLICT' } }, { status: 409, statusText: 'Unknown' });
    expect(state.pending()!.body).toBe(body); state.retry(); const exact = http.expectOne(url); expect(exact.request.body).toBe(body);
    exact.flush(unitApprovalReceipt(body)); http.expectOne(req => req.method === 'GET').flush(approved(body)); state.clear(); http.verify();
  });
  it('honors Retry-After without automatic writes and keeps the same nonce after the wait', () => {
    vi.useFakeTimers(); const { state, http, scope } = loaded(); state.approve(scope.page.units[0].id, complete());
    const post = http.expectOne(url), body = post.request.body;
    post.flush({ error: { code: 'UNIT_APPROVAL_LIMIT' } }, { status: 429, statusText: 'Wait', headers: { 'Retry-After': '7' } });
    expect(state.retryWait()).toBe(7); state.retry(); http.expectNone(req => req.method === 'POST');
    vi.advanceTimersByTime(7000); http.expectNone(req => req.method === 'POST'); state.retry();
    const retry = http.expectOne(url); expect(retry.request.body).toBe(body); retry.flush(unitApprovalReceipt(body));
    http.expectOne(req => req.method === 'GET').flush(approved(body)); state.clear(); http.verify();
  });
  it('does not invent a wait when the clock advances while processing a transport failure', () => {
    const { state, http, scope } = loaded(); state.approve(scope.page.units[0].id, complete()); const post = http.expectOne(url), body = post.request.body;
    let clock = Date.now(); vi.spyOn(Date, 'now').mockImplementation(() => clock++);
    post.error(new ProgressEvent('error')); expect(state.retryWait()).toBe(0); state.retry(); const retry = http.expectOne(url);
    expect(retry.request.body).toBe(body); state.clear(); http.verify();
  });
  it.each(['INVALID_UNIT_APPROVAL', 'REVIEW_CONTEXT_CHANGED', 'REVIEW_UNIT_UNVIEWED', 'REVIEW_UNIT_EVIDENCE_UNAVAILABLE', 'UNIT_APPROVAL_CONFLICT'])('treats %s as a definitive rejection and requires fresh coverage before another approval', code => {
    const { state, http, scope } = loaded(), coverage = complete(), id = scope.page.units[0].id;
    state.approve(id, coverage); http.expectOne(url).flush({ error: { code } }, { status: code === 'INVALID_UNIT_APPROVAL' ? 400 : 409, statusText: 'Rejected' });
    expect(state.hasPending()).toBe(false); expect(state.canApprove(id, coverage)).toBe(false);
    state.refresh(); http.expectOne(req => req.method === 'GET').flush(unitApprovalPage()); expect(state.canApprove(id, coverage)).toBe(false);
    expect(state.canApprove(id, complete())).toBe(true); state.clear(); http.verify();
  });
  it.each([401, 403, 404, 409])('purges private state on %s scoped denial or session-identity mismatch', status => {
    const { state, http, scope } = loaded(); state.approve(scope.page.units[0].id, complete());
    http.expectOne(url).flush({ error: { code: status === 409 ? 'SESSION_IDENTITY_CHANGED' : 'DENIED' } }, { status, statusText: 'Denied' });
    expect(state.denied()).toBe(true); expect(state.hasPending()).toBe(false); expect(state.pending()).toBeNull(); expect(state.view()).toBeNull();
    state.configure(selection(), () => true); state.retry(); http.expectNone(url); state.clear(); http.verify();
  });
  it.each(['actor', 'logout', 'scope'])('ignores queued approval success immediately after %s changes before effects run', fault => {
    const { state, http, scope, profile, logoutEpoch, live } = loaded(); state.approve(scope.page.units[0].id, complete()); const post = http.expectOne(url);
    if (fault === 'actor') profile.set({ user_id: other }); if (fault === 'logout') logoutEpoch.update(n => n + 1); if (fault === 'scope') live.set(false);
    post.flush(unitApprovalReceipt(post.request.body)); expect(state.result()).toBeNull(); expect(state.pending()).toBeNull(); expect(state.view()).toBeNull();
    http.expectNone(req => req.method === 'GET'); TestBed.tick(); expect(state.hasPending()).toBe(false); http.verify();
  });
  it('rejects malformed reads, cancels obsolete pages and never supplies approval authority from failed reads', () => {
    const { state, http } = create(), first = http.expectOne(req => req.method === 'GET');
    const next = selection(); next.page = reviewUnits(product, 20, 22); state.configure(next, () => true); expect(first.cancelled).toBe(true);
    http.expectOne(req => req.method === 'GET').flush({ ...unitApprovalPage(next.page), approved_units: 22 }); expect(state.view()).toBeNull();
    expect(state.readError()).toContain('אינם תואמים'); state.approve(next.page.units[0].id, complete(next)); http.expectNone(req => req.method === 'POST'); state.clear(); http.verify();
  });
  it('rejects a refreshed status that contradicts a confirmed immutable receipt instead of inventing counts', () => {
    const { state, http, scope } = loaded(); state.approve(scope.page.units[0].id, complete()); const post = http.expectOne(url), body = post.request.body;
    post.flush(unitApprovalReceipt(body)); http.expectOne(req => req.method === 'GET').flush(unitApprovalPage());
    expect(state.view()).toBeNull(); expect(state.result()?.receipt.unit_id).toBe(body.unit_id); expect(state.readError()).toContain('אינם תואמים');
    expect(state.canApprove(body.unit_id, complete())).toBe(false); state.clear(); http.verify();
  });
  it('bounds stalled writes and retains the original intent without retrying automatically', () => {
    vi.useFakeTimers(); const { state, http, scope } = loaded(); state.approve(scope.page.units[0].id, complete()); const post = http.expectOne(url), body = post.request.body;
    vi.advanceTimersByTime(20001); expect(post.cancelled).toBe(true); expect(state.sending()).toBe(false); expect(state.pending()!.body).toBe(body);
    http.expectNone(url); state.retry(); const retry = http.expectOne(url); expect(retry.request.body).toBe(body); state.clear(); expect(retry.cancelled).toBe(true); http.verify();
  });
  it('bounds stalled reads and waits for an explicit retry', () => {
    vi.useFakeTimers(); const { state, http } = create(), read = http.expectOne(req => req.method === 'GET');
    vi.advanceTimersByTime(20001); expect(read.cancelled).toBe(true); expect(state.busy()).toBe(false); expect(state.view()).toBeNull(); http.expectNone(url);
    state.refresh(); http.expectOne(req => req.method === 'GET').flush(unitApprovalPage()); state.clear(); http.verify();
  });
});
