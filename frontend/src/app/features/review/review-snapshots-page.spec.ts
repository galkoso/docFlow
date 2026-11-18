import { Component, signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { provideRouter, Router } from '@angular/router';
import { RouterTestingHarness } from '@angular/router/testing';
import { SessionState } from '../../core/auth/session-state';
import { ReviewSnapshotsPage } from './review-snapshots-page';
import { reviewSnapshotExit } from './review-snapshot-exit';
import { actor, proposal, snapshot, other, configuration, detail, history, metadata, files, media,
  ticket, publication, job } from '../../../testing/review-fixtures';

@Component({ template: '<p>Elsewhere</p>' })
class Away {}
describe('Review snapshots page', () => {
  afterEach(() => vi.restoreAllMocks());
  const route = `/changes/${proposal}/review/snapshots`, root = `/api/v1/proposals/${proposal}`, snapshots = `${root}/review/snapshots`;
  async function create(query = `?snapshot=${snapshot}`) {
    const profile = signal<{ user_id: string } | null>(null), logoutEpoch = signal(0); let guard = () => true;
    const released = vi.fn();
    await TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting(),
      provideRouter([{ path: 'changes/:id/review/snapshots', component: ReviewSnapshotsPage, canDeactivate: [reviewSnapshotExit] }, { path: 'away', component: Away }]),
      { provide: SessionState, useValue: { profile, logoutEpoch, status: () => profile() ? 'authenticated' : 'anonymous', refresh: vi.fn(),
        guardLogout: (callback: () => boolean) => { guard = callback; return released; } } }],
    }).compileComponents();
    const harness = await RouterTestingHarness.create(), page = await harness.navigateByUrl(`${route}${query}`, ReviewSnapshotsPage);
    const http = TestBed.inject(HttpTestingController), router = TestBed.inject(Router);
    profile.set({ user_id: actor }); harness.detectChanges();
    const settle = async () => { await harness.fixture.whenStable(); harness.detectChanges(); };
    const load = (unsafe = false) => {
      const value = history(), body = metadata();
      if (unsafe) { value.title = '<script>bad()</script>'; value.snapshots[0].title = '<script>bad()</script>'; body.title = '<script>bad()</script>';
        body.metadata.context.description = '<img src=x onerror=bad()> private description'; }
      http.expectOne(req => req.url === snapshots).flush(value);
      http.expectOne(`${snapshots}/${snapshot}`).flush(body);
      http.expectOne(req => req.url === `${snapshots}/${snapshot}/files`).flush(files());
      http.expectOne(req => req.url === `${snapshots}/${snapshot}/media`).flush(media()); harness.detectChanges();
    };
    const prepare = () => { page.prepare(); http.expectOne(`${root}/review/configuration`).flush(configuration()); http.expectOne(root).flush(detail()); harness.detectChanges(); };
    return { harness, page, http, router, profile, logoutEpoch, released, guard: () => guard(), settle, load, prepare };
  }
  it('renders shared history read-only, keeps branch identity visible and sends only after explicit checked-version confirmation', async () => {
    const { harness, page, http, router, load, prepare, settle } = await create(); load(); await settle();
    const element = harness.routeNativeElement!;
    expect(element.textContent).toContain('change/25-review'); expect(element.textContent).toContain('main');
    expect(element.textContent).toContain('קבצים שהשתנו'); expect(element.textContent).toContain('docs/a.md');
    http.expectNone(root); http.expectNone(`${root}/review/configuration`); http.expectNone(req => req.method === 'POST');
    prepare(); await settle(); expect(element.textContent).toContain('שליחת הגרסה השמורה לסקירה');
    const send = [...element.querySelectorAll('button')].find(button => button.textContent?.trim() === 'שליחת הגרסה השמורה לסקירה')!;
    send.click(); harness.detectChanges(); const request = http.expectOne(`${root}/submit`);
    expect(request.request.headers.get('X-Expected-Actor')).toBe(actor); request.flush(ticket);
    http.expectOne(`/api/v1/jobs/${ticket.job_id}`).flush(job({ state: 'SUCCEEDED', phase: 'SNAPSHOT_READY', attempts: 1, review: publication() }));
    harness.detectChanges(); await settle();
    expect(page.submission.pending()).toBe(false); expect(router.url).toContain(`job=${ticket.job_id}`);
    expect(element.textContent).toContain('גרסה משותפת נשמרה לסקירה'); expect(element.textContent).toContain('פרסום באפיון המאושר דורש מיזוג');
    expect([...element.querySelectorAll('button')].some(button => button.textContent?.trim() === 'אישור')).toBe(false); http.verify();
  });
  it('restores a job URL through observation only, never reconstructing a submission', async () => {
    const { page, http, load, settle } = await create(`?snapshot=${snapshot}&job=${ticket.job_id}`); load();
    const observe = http.expectOne(`/api/v1/jobs/${ticket.job_id}`); expect(observe.request.headers.get('X-Expected-Actor')).toBe(actor);
    observe.flush(job({ state: 'SUCCEEDED', phase: 'SNAPSHOT_READY', attempts: 1, review: publication() })); await settle();
    expect(page.submission.result()?.snapshot_id).toBe(snapshot); expect(page.submission.submitted()).toBeNull();
    page.submission.retry(); http.expectNone(req => req.method === 'POST'); http.verify();
  });
  it('canonicalizes the selected snapshot without reloading its already verified pages', async () => {
    const { router, http, load, settle } = await create(''); load(); await settle();
    expect(router.url).toContain(`snapshot=${snapshot}`); http.verify();
  });
  it('guards route changes, logout and unload while an uncertain request remains', async () => {
    const { page, http, router, load, prepare, guard, settle, released } = await create(); load(); prepare(); page.submission.start();
    http.expectOne(`${root}/submit`).error(new ProgressEvent('error')); await settle();
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(false);
    expect(guard()).toBe(false); expect(await router.navigateByUrl('/away')).toBe(false);
    const unload = new Event('beforeunload', { cancelable: true }) as BeforeUnloadEvent; page.beforeUnload(unload); expect(unload.defaultPrevented).toBe(true);
    confirm.mockReturnValue(true); expect(await router.navigateByUrl('/away')).toBe(true); expect(released).toHaveBeenCalled();
    expect(page.submission.submitted()).toBeNull(); http.verify();
  });
  it('hides private context during refresh and purges it on account switch without adopting a new actor for the old intent', async () => {
    const { harness, page, http, profile, load, prepare, settle } = await create(); load(); prepare(); page.submission.start(); const sent = http.expectOne(`${root}/submit`);
    profile.set(null); harness.detectChanges(); await settle(); expect(sent.cancelled).toBe(true);
    expect(harness.routeNativeElement!.textContent).not.toContain('Synthetic review');
    profile.set({ user_id: actor }); harness.detectChanges(); load(); await settle();
    expect(page.submission.canRetry()).toBe(true); http.expectNone(`${root}/submit`);
    profile.set({ user_id: other }); harness.detectChanges(); await settle();
    expect(page.blocked()).toBe(true); expect(page.submission.submitted()).toBeNull(); expect(page.state.snapshot()).toBeNull();
    http.expectNone(req => req.method === 'POST'); page.reload(); const fresh = http.expectOne(req => req.url === snapshots);
    expect(fresh.request.headers.get('X-Expected-Actor')).toBe(other); fresh.flush({ ...history(), snapshots: [], active_snapshot_id: null });
    const denied = http.expectOne(`${snapshots}/${snapshot}`), pendingFiles = http.expectOne(req => req.url === `${snapshots}/${snapshot}/files`);
    const pendingMedia = http.expectOne(req => req.url === `${snapshots}/${snapshot}/media`);
    expect(denied.request.headers.get('X-Expected-Actor')).toBe(other);
    denied.flush({ error: { code: 'NOT_FOUND' } }, { status: 404, statusText: 'Not Found' });
    expect(pendingFiles.cancelled && pendingMedia.cancelled).toBe(true); await settle(); http.verify();
  });
  it('keeps server strings inert and exposes keyboard-operable controls with visible labels', async () => {
    const { harness, http, load, settle } = await create(); load(true); await settle(); const element = harness.routeNativeElement!;
    expect(element.querySelector('script, img')).toBeNull(); expect(element.textContent).toContain('<script>bad()</script>');
    expect(element.textContent).toContain('<img src=x onerror=bad()>');
    expect([...element.querySelectorAll('button')].every(button => !!button.textContent?.trim())).toBe(true);
    expect(element.querySelector('nav[aria-label]')).not.toBeNull(); expect(element.querySelector('bdi')).not.toBeNull(); http.verify();
  });
  it('rejects ambiguous locations before sending requests', async () => {
    const { harness, http, settle } = await create(`?snapshot=${snapshot}&snapshot=${other}`); await settle();
    expect(harness.routeNativeElement!.textContent).toContain('כתובת גרסת הסקירה אינה תקינה'); http.expectNone(req => req.url.startsWith(root)); http.verify();
  });
  it('purges displayed evidence when private job access is denied and reloads shared history without retrying that job', async () => {
    const { harness, page, http, router, load, settle } = await create(`?snapshot=${snapshot}&job=${ticket.job_id}`); load();
    http.expectOne(`/api/v1/jobs/${ticket.job_id}`).flush({ error: { code: 'NOT_FOUND' } }, { status: 404, statusText: 'Not Found' });
    harness.detectChanges(); await settle(); expect(page.blocked()).toBe(true); expect(page.state.snapshot()).toBeNull();
    expect(harness.routeNativeElement!.textContent).not.toContain('Synthetic review');
    page.reload(); load(); await settle(); expect(router.url).not.toContain('job='); expect(page.blocked()).toBe(false);
    expect(page.state.snapshot()?.snapshot_id).toBe(snapshot); http.expectNone(`/api/v1/jobs/${ticket.job_id}`); http.verify();
  });
  it('refuses a queued UI action immediately after the profile changes, before reactive cleanup runs', async () => {
    const { harness, page, http, profile, load, prepare, settle } = await create(); load(); prepare(); await settle();
    profile.set({ user_id: other }); page.send(); page.retry(); page.check();
    http.expectNone(req => req.method === 'POST'); expect(page.submission.pending()).toBe(false);
    harness.detectChanges(); await settle(); expect(page.blocked()).toBe(true); expect(page.submission.context()).toBeNull(); http.verify();
  });
  it('restores a shared raw file URL and changes modes without reloading immutable metadata; navigation cancels source reads', async () => {
    const file = files().files[0].id, query = `?snapshot=${snapshot}&file=${file}&mode=raw`;
    const { page, http, load, router, settle } = await create(query); load(); await settle();
    const path = `${snapshots}/${snapshot}/files/${file}`, raw = http.expectOne(req => req.url === `${path}/raw`);
    expect(raw.request.headers.get('X-Expected-Actor')).toBe(actor); expect(page.fileScope()?.snapshot).toBe(snapshot);
    raw.flush({}, { status: 503, statusText: 'Unavailable' }); await settle(); expect(page.state.snapshot()).not.toBeNull();
    page.setMode('readable'); await settle(); const readable = http.expectOne(req => req.url === `${path}/readable`);
    expect(router.url).toContain('mode=readable'); http.expectNone(req => req.url === snapshots);
    await router.navigateByUrl(`${route}${query}`); await settle(); expect(readable.cancelled).toBe(true);
    const restored = http.expectOne(req => req.url === `${path}/raw`);
    await router.navigateByUrl('/away'); await settle(); expect(restored.cancelled).toBe(true); http.verify();
  });
  it('purges the snapshot and body on denied source access and recreates authority only after explicit reload', async () => {
    const file = files().files[0].id;
    const { page, harness, http, load, settle } = await create(`?snapshot=${snapshot}&file=${file}&mode=raw`); load(); await settle();
    const path = `${snapshots}/${snapshot}/files/${file}/raw`;
    http.expectOne(req => req.url === path).flush({}, { status: 403, statusText: 'Denied' }); await settle();
    expect(page.blocked()).toBe(true); expect(page.state.snapshot()).toBeNull(); expect(page.fileScope()).toBeNull();
    expect(harness.routeNativeElement!.querySelector('app-shared-review-body')).toBeNull(); http.expectNone(() => true);
    page.reload(); load(); await settle(); expect(page.blocked()).toBe(false);
    http.expectOne(req => req.url === path).flush({}, { status: 503, statusText: 'Unavailable' }); await settle(); http.verify();
  });
  it('does not fetch source for an unknown file or a queued file click after an account switch', async () => {
    const { page, http, profile, load, settle } = await create(`?snapshot=${snapshot}&file=${'9'.repeat(64)}`); load(); await settle();
    expect(page.fileScope()).toBeNull(); http.expectNone(req => req.url.includes('/readable') || req.url.includes('/raw'));
    profile.set({ user_id: other }); page.selectFile(files().files[0].id); await settle();
    expect(page.blocked()).toBe(true); http.expectNone(() => true); http.verify();
  });

});
