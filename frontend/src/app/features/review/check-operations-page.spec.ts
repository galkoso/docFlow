import { Component, signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { provideRouter, Router } from '@angular/router';
import { RouterTestingHarness } from '@angular/router/testing';
import { afterEach, describe, it, expect, vi } from 'vitest';
import { SessionState } from '../../core/auth/session-state';
import type { CheckOperation, CheckOperationPage } from '../../core/api/check-operations-api';
import { CheckOperationsPage } from './check-operations-page';
import { checkOperationsExit } from './check-operations-exit';

@Component({ template: '<p>Elsewhere</p>' }) class Away {}
const id = (n: number) => `00000000-0000-4000-8000-${n.toString(16).padStart(12, '0')}`;
const actor = id(1), org = id(2), proposal = id(3), operation = id(4);
const route = `/changes/${proposal}/review/checks`, root = `/api/v1/proposals/${proposal}/review/checks`;
const profileValue = (user = actor, organization = org) => ({ user_id: user, memberships: [{ organization_id: organization, role: 'ADMIN', review_domains: [] }] });
function operationValue(): CheckOperation {
  return { viewer_id: actor, organization_id: org, proposal_id: proposal, operation_id: operation, snapshot_id: id(5), request_hash: 'a'.repeat(64),
    purpose: 'EVALUATION', desired_result: 'SUCCESS', job_state: 'FAILED', operation_state: 'UNKNOWN', attempts: 8, max_attempts: 8,
    write_phase: 'CREATE_STARTED', known_run: false, published: false, error_code: 'CHECK_RESULT_UNKNOWN', provider_verified: false };
}
function pageValue(item = operationValue()): CheckOperationPage {
  return { viewer_id: actor, organization_id: org, proposal_id: proposal, after: null, limit: 20, items: [item], next_cursor: null };
}
describe('administrator check operations page', () => {
  afterEach(() => { vi.restoreAllMocks(); vi.useRealTimers(); });
  async function create(query = `?operation=${operation}`, initialProfile = profileValue()) {
    const profile = signal<ReturnType<typeof profileValue> | null>(null), logoutEpoch = signal(0); let guard = () => true;
    const released = vi.fn();
    await TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting(),
      provideRouter([{ path: 'changes/:id/review/checks', component: CheckOperationsPage, canDeactivate: [checkOperationsExit],
        runGuardsAndResolvers: 'paramsOrQueryParamsChange' }, { path: 'away', component: Away }]),
      { provide: SessionState, useValue: { profile, logoutEpoch, guardLogout: (callback: () => boolean) => { guard = callback; return released; } } },
    ] }).compileComponents();
    const harness = await RouterTestingHarness.create(), page = await harness.navigateByUrl(`${route}${query}`, CheckOperationsPage);
    const http = TestBed.inject(HttpTestingController), router = TestBed.inject(Router);
    profile.set(initialProfile); harness.detectChanges();
    const flush = (value = operationValue(), history = pageValue(value)) => {
      http.expectOne(req => req.url === root).flush(history);
      if (query.includes('operation=')) http.expectOne(`${root}/${operation}`).flush(value);
      harness.detectChanges();
    };
    const destroy = async () => { vi.spyOn(window, 'confirm').mockReturnValue(true); await router.navigateByUrl('/away'); http.verify(); };
    return { harness, page, http, router, profile, logoutEpoch, released, guard: () => guard(), flush, destroy };
  }
  it('renders escaped, scoped journal metadata and never posts on load or refresh', async () => {
    const f = await create(); f.flush();
    expect(f.page.view()?.operation_id).toBe(operation);
    const element = f.harness.routeNativeElement!;
    expect(element.textContent).toContain('בדיקת התוצאה נעצרה'); expect(element.textContent).toContain('מצב הספק');
    expect(element.querySelector('bdi')?.textContent).toBe(operation);
    expect(element.querySelector('[innerHTML]')).toBeNull(); f.http.expectNone(req => req.method === 'POST');
    f.page.reload(); f.flush(); f.http.expectNone(req => req.method === 'POST'); await f.destroy(); expect(f.released).toHaveBeenCalled();
  });
  it.each(['RETRY_UNSTARTED', 'OBSERVE_STARTED'] as const)('sends one explicit %s recovery and observes completion separately', async mode => {
    const f = await create(), original = operationValue();
    if (mode === 'RETRY_UNSTARTED') Object.assign(original, { write_phase: 'NOT_STARTED', operation_state: 'FAILED' });
    f.flush(original); f.page.recover(); f.page.recover();
    const request = f.http.expectOne(`${root}/${operation}/recover`), body = request.request.body;
    expect(Object.keys(body).sort()).toEqual(['expected_request_hash', 'request_id']); expect(body.expected_request_hash).toBe(original.request_hash);
    expect(request.request.headers.get('X-Expected-Actor')).toBe(actor);
    request.flush({ proposal_id: proposal, operation_id: operation, request_id: body.request_id, mode, max_attempts: 16, replayed: false });
    expect(f.page.recoveryPending()).toBe(false); expect(f.page.view()).toBeNull();
    f.flush({ ...original, job_state: 'QUEUED', operation_state: mode === 'RETRY_UNSTARTED' ? 'PENDING' : 'UNKNOWN', max_attempts: 16 });
    expect(f.harness.routeNativeElement!.textContent).toContain('תוצאת העבודה נבדקת בנפרד'); await f.destroy();
  });
  it('keeps the same immutable request after a lost response even when refreshed metadata has a newer budget', async () => {
    const f = await create(); f.flush(); f.page.recover();
    const first = f.http.expectOne(`${root}/${operation}/recover`), body = first.request.body;
    expect(Object.isFrozen(body)).toBe(true); first.error(new ProgressEvent('error')); expect(f.page.recoveryPending()).toBe(true);
    f.page.reload(); f.flush({ ...operationValue(), max_attempts: 16 }); f.http.expectNone(req => req.method === 'POST');
    f.page.recover(); const retry = f.http.expectOne(`${root}/${operation}/recover`); expect(retry.request.body).toBe(body);
    retry.flush({ proposal_id: proposal, operation_id: operation, request_id: body.request_id, mode: 'OBSERVE_STARTED', max_attempts: 16, replayed: true });
    f.flush({ ...operationValue(), max_attempts: 16 }); expect(f.page.recoveryPending()).toBe(false); await f.destroy();
  });
  it.each(['proposal_id', 'operation_id', 'request_id', 'mode', 'max_attempts'] as const)('retains uncertainty on a substituted %s receipt', async key => {
    const f = await create(); f.flush(); f.page.recover(); const request = f.http.expectOne(`${root}/${operation}/recover`);
    const receipt = { proposal_id: proposal, operation_id: operation, request_id: request.request.body.request_id,
      mode: 'OBSERVE_STARTED', max_attempts: 16 };
    const value: Record<string, unknown> = { ...receipt, replayed: false }; value[key] = key === 'max_attempts' ? 17 : key === 'mode' ? 'RETRY_UNSTARTED' : id(99);
    request.flush(value); expect(f.page.recoveryPending()).toBe(true); expect(f.page.view()).toBeNull();
    f.http.expectNone(req => req.method === 'GET'); await f.destroy();
  });
  it.each(['viewer_id', 'organization_id', 'operation_id', 'provider_verified'] as const)('rejects unbound %s observations without installing partial page data', async key => {
    const f = await create(), value = operationValue();
    Object.assign(value, { [key]: key === 'provider_verified' ? true : id(99) });
    f.flush(value, pageValue()); expect(f.page.view()).toBeNull(); expect(f.page.page()).toBeNull(); expect(f.page.error()).toBeTruthy(); await f.destroy();
  });
  it('guards route, query, logout and unload while the accepted result is unknown', async () => {
    const f = await create(); f.flush(); f.page.recover();
    f.http.expectOne(`${root}/${operation}/recover`).error(new ProgressEvent('error'));
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(false);
    expect(await f.router.navigateByUrl(`${route}?operation=${id(99)}`)).toBe(false);
    expect(await f.router.navigateByUrl('/away')).toBe(false); expect(f.guard()).toBe(false);
    const event = new Event('beforeunload', { cancelable: true }) as BeforeUnloadEvent; f.page.beforeUnload(event); expect(event.defaultPrevented).toBe(true);
    confirm.mockReturnValue(true); await f.router.navigateByUrl('/away'); expect(f.page.recoveryPending()).toBe(false); f.http.verify();
  });
  it('purges scope on access denial and synchronously prevents a post after a role or account change', async () => {
    const f = await create(); f.flush();
    f.profile.set(profileValue(actor, id(99))); f.page.recover(); f.http.expectNone(req => req.method === 'POST'); expect(f.page.view()).toBeNull();
    f.harness.detectChanges(); expect(f.page.denied()).toBe(true); expect(f.page.page()).toBeNull();
    f.profile.set(profileValue()); f.harness.detectChanges(); f.flush(); f.page.recover();
    f.http.expectOne(`${root}/${operation}/recover`).flush({}, { status: 403, statusText: 'Forbidden' });
    expect(f.page.recoveryPending()).toBe(false); expect(f.page.view()).toBeNull(); expect(f.page.denied()).toBe(true);
    await f.destroy();
  });
  it('never posts from a cooldown timer and permits only an explicit exact retry after expiry', async () => {
    const f = await create(); f.flush(); vi.useFakeTimers(); f.page.recover();
    const first = f.http.expectOne(`${root}/${operation}/recover`), body = first.request.body;
    first.flush({ error: { code: 'CHECK_PROVIDER_COOLDOWN' } }, { status: 429, statusText: 'Too Many Requests', headers: { 'Retry-After': '7' } });
    f.page.recover(); f.http.expectNone(req => req.method === 'POST'); expect(f.page.retryAt()).toBeGreaterThan(Date.now());
    vi.advanceTimersByTime(7000); expect(f.page.retryAt()).toBe(0); f.http.expectNone(req => req.method === 'POST');
    f.page.recover(); const retry = f.http.expectOne(`${root}/${operation}/recover`); expect(retry.request.body).toBe(body);
    retry.error(new ProgressEvent('error')); vi.useRealTimers(); await f.destroy();
  });
  it('rejects malformed URL parameters without starting requests', async () => {
    const f = await create('?operation=invalid&after=forged'); expect(f.page.error()).toBeTruthy(); f.http.expectNone(req => req.url.startsWith(root)); await f.destroy();
  });
  it('keeps bounded cursor navigation in the URL and does not automatically select or recover a row', async () => {
    const f = await create(''); f.flush(); expect(f.page.view()).toBeNull(); f.http.expectNone(req => req.method === 'POST');
    await f.router.navigateByUrl(`${route}?after=${id(40)}`); f.harness.detectChanges();
    const request = f.http.expectOne(req => req.url === root); expect(request.request.params.get('after')).toBe(id(40));
    request.flush({ ...pageValue(), after: id(40) }); f.harness.detectChanges(); expect(f.page.page()?.after).toBe(id(40)); await f.destroy();
  });
  it('does not load private metadata for a Reader', async () => {
    const reader = profileValue(); reader.memberships[0].role = 'READER';
    const f = await create('', reader); expect(f.page.denied()).toBe(true);
    expect(f.harness.routeNativeElement!.textContent).toContain('אין גישה'); f.page.reload(); expect(f.page.error()).toBeTruthy();
    f.http.expectNone(req => req.url.startsWith(root)); await f.destroy();
  });
  it('cancels old requests on account switches without replaying the previous recovery', async () => {
    const f = await create(); f.flush(); f.page.recover(); const old = f.http.expectOne(`${root}/${operation}/recover`);
    f.profile.set(profileValue(id(99))); f.page.recover(); expect(f.page.view()).toBeNull();
    f.harness.detectChanges(); expect(old.cancelled).toBe(true); f.http.expectNone(req => req.method === 'POST');
    const value = { ...operationValue(), viewer_id: id(99) };
    const history = { ...pageValue(value), viewer_id: id(99) };
    const page = f.http.expectOne(req => req.url === root), selected = f.http.expectOne(`${root}/${operation}`);
    expect(page.request.headers.get('X-Expected-Actor')).toBe(id(99)); page.flush(history); selected.flush(value);
    expect(f.page.recoveryPending()).toBe(false); await f.destroy();
  });
  it('purges private state and cancels recovery when the session is lost', async () => {
    const f = await create(); f.flush(); f.page.recover(); const old = f.http.expectOne(`${root}/${operation}/recover`);
    f.profile.set(null); f.logoutEpoch.update(v => v + 1); f.page.recover(); f.harness.detectChanges();
    expect(old.cancelled).toBe(true); expect(f.page.page()).toBeNull(); expect(f.page.view()).toBeNull();
    expect(f.page.recoveryPending()).toBe(false); f.http.expectNone(req => req.method === 'POST'); await f.destroy();
  });
  it('preserves an uncertain request after timeout without posting again', async () => {
    const f = await create(); f.flush(); vi.useFakeTimers(); f.page.recover();
    const old = f.http.expectOne(`${root}/${operation}/recover`); vi.advanceTimersByTime(20000);
    expect(old.cancelled).toBe(true); expect(f.page.recovering()).toBe(false); expect(f.page.recoveryPending()).toBe(true);
    f.http.expectNone(req => req.method === 'POST'); vi.useRealTimers(); await f.destroy();
  });

  it('accepts a concurrent operator budget/phase advance without rearming an old write', async () => {
    const f = await create(), original = { ...operationValue(), write_phase: 'NOT_STARTED', operation_state: 'FAILED' };
    f.flush(original); f.page.recover(); const request = f.http.expectOne(`${root}/${operation}/recover`);
    request.flush({ proposal_id: proposal, operation_id: operation, request_id: request.request.body.request_id,
      mode: 'OBSERVE_STARTED', max_attempts: 24, replayed: false });
    f.flush({ ...operationValue(), max_attempts: 24 }); expect(f.page.recoveryPending()).toBe(false);
    f.http.expectNone(req => req.method === 'POST'); await f.destroy();
  });

});
