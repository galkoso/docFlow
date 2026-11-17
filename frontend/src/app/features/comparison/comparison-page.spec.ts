import { TestBed } from '@angular/core/testing';
import { signal } from '@angular/core';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { ActivatedRoute, convertToParamMap, provideRouter, Router } from '@angular/router';
import { BehaviorSubject } from 'rxjs';
import { SessionState } from '../../core/auth/session-state';
import { ComparisonPage } from './comparison-page';
import { actor, proposal, snapshot, head, id, file, ready, pending } from './comparison.testing';
import { rawResponse } from './raw.testing';

describe('Routed comparison page', () => {
  async function create(initial: Record<string, string | string[]> = { snapshot }) {
    const profile = signal<{ user_id: string } | null>({ user_id: actor }), logoutEpoch = signal(0);
    const params = new BehaviorSubject(convertToParamMap({ id: proposal })), query = new BehaviorSubject(convertToParamMap(initial));
    await TestBed.configureTestingModule({ imports: [ComparisonPage], providers: [provideHttpClient(), provideHttpClientTesting(), provideRouter([]),
      { provide: SessionState, useValue: { profile, logoutEpoch, status: () => profile() ? 'authenticated' : 'anonymous', refresh: vi.fn() } },
      { provide: ActivatedRoute, useValue: { paramMap: params, queryParamMap: query,
        snapshot: { paramMap: params.value, queryParamMap: query.value } } }],
    }).compileComponents();
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigate').mockImplementation((_commands, extras) => {
      const values: Record<string, string | string[]> = {};
      for (const key of query.value.keys) values[key] = query.value.getAll(key);
      for (const [key, value] of Object.entries(extras?.queryParams ?? {})) {
        if (value == null) delete values[key]; else values[key] = String(value);
      }
      query.next(convertToParamMap(values)); return Promise.resolve(true);
    });
    const fixture = TestBed.createComponent(ComparisonPage), page = fixture.componentInstance, http = TestBed.inject(HttpTestingController);
    fixture.detectChanges();
    const request = () => http.expectOne(req => req.url.endsWith('/diff'));
    const body = () => http.expectOne(req => req.url.includes('/diff/files/'));
    const settle = () => { fixture.detectChanges(); TestBed.tick(); fixture.detectChanges(); };
    function destroy() { fixture.destroy(); http.verify(); }
    return { fixture, page, http, profile, logoutEpoch, params, query, navigate, request, body, settle, destroy };
  }

  it('restores a snapshot without preparing and resumes only that snapshot after session refresh', async () => {
    const c = await create(); const first = c.request(); expect(first.request.params.has('request_id')).toBe(false);
    expect(first.request.params.get('snapshot')).toBe(snapshot); first.flush(ready()); c.settle();
    expect(c.fixture.nativeElement.textContent).toContain(head);
    c.profile.set(null); c.settle(); expect(c.page.state.data()).toBeNull();
    expect(c.fixture.nativeElement.textContent).not.toContain(head);
    c.profile.set({ user_id: actor }); c.settle(); const resumed = c.request();
    expect(resumed.request.params.get('snapshot')).toBe(snapshot); resumed.flush(ready()); c.settle(); c.destroy();
  });

  it('persists the original intent and promotes an accepted snapshot without duplicate preparation', async () => {
    const c = await create({ head }); const first = c.request(); const key = first.request.params.get('request_id');
    c.settle(); expect(c.query.value.get('request_id')).toBe(key); expect(c.query.value.get('head')).toBe(head);
    c.http.expectNone(req => req.url.endsWith('/diff')); first.flush(pending()); c.settle();
    expect(c.query.value.get('snapshot')).toBe(snapshot); expect(c.query.value.has('request_id')).toBe(false);
    expect(c.query.value.has('head')).toBe(false); c.http.expectNone(req => req.url.endsWith('/diff')); c.destroy();
  });

  it('prepares latest with a fresh nonce and removes the old snapshot and page from the URL', async () => {
    const c = await create({ snapshot, offset: '50' }); c.request().flush(ready(120, 50)); c.settle();
    c.page.fresh(); c.settle(); const latest = c.request(); const key = latest.request.params.get('request_id');
    expect(key).toBeTruthy(); expect(c.query.value.has('snapshot')).toBe(false); expect(c.query.value.has('offset')).toBe(false);
    expect(c.query.value.get('request_id')).toBe(key); expect(latest.request.params.has('head')).toBe(false);
    const receipt = ready(); receipt.snapshot_id = id(9); receipt.page!.snapshot_id = id(9); latest.flush(receipt); c.settle();
    expect(c.query.value.get('snapshot')).toBe(id(9)); expect(c.page.routeError()).toBe('');
    c.http.expectNone(req => req.url.endsWith('/diff')); c.destroy();
  });

  it('selects pinned raw source, keeps HTML inert and provides textual removal markers', async () => {
    const html = '<img src=x onerror=alert(1)>', data = ready(); data.page!.files[0].before!.bytes = html.length;
    const c = await create(); c.request().flush(data); c.settle(); c.page.select(file().id); c.settle();
    const body = c.body(); expect(body.request.headers.get('X-Expected-Actor')).toBe(actor);
    expect(body.request.params.get('snapshot')).toBe(snapshot);
    const response = rawResponse(); response.file = data.page!.files[0]; response.raw.old_bytes = html.length;
    response.raw.segments[0].old_text = html; response.raw.segments[0].old_end_byte = html.length;
    response.raw.returned_text_bytes = html.length; body.flush(response); c.settle();
    expect(c.fixture.nativeElement.querySelector('pre').textContent).toBe(html);
    expect(c.fixture.nativeElement.querySelector('img')).toBeNull(); expect(c.fixture.nativeElement.querySelector('[onerror]')).toBeNull();
    expect(c.fixture.nativeElement.textContent).toContain('− הוסר'); c.destroy();
  });

  it('purges private body on actor change and completed logout without silently preparing for another identity', async () => {
    const c = await create(); c.request().flush(ready()); c.settle(); c.page.select(file().id); c.settle(); c.body().flush(rawResponse()); c.settle();
    c.profile.set({ user_id: id(8) }); c.settle(); expect(c.page.raw.data()).toBeNull(); expect(c.page.state.data()).toBeNull();
    expect(c.fixture.nativeElement.textContent).not.toContain('old'); c.http.expectNone(req => req.url.endsWith('/diff'));
    c.page.fresh(); c.settle(); const fresh = c.request(); expect(fresh.request.headers.get('X-Expected-Actor')).toBe(id(8));
    fresh.flush(ready()); c.settle(); c.logoutEpoch.update(v => v + 1); c.settle();
    expect(c.page.state.owner()).toBeNull(); c.http.expectNone(req => req.url.endsWith('/diff')); c.destroy();
  });

  it('rejects duplicate and mixed query parameters without sending private requests', async () => {
    const invalid: Array<Record<string, string | string[]>> = [{ snapshot: [snapshot, id(9)] }, { snapshot, request_id: id(9) }, { snapshot: '' }];
    for (const query of invalid) {
      const c = await create(query); expect(c.page.routeError()).not.toBe('');
      c.http.expectNone(req => req.url.endsWith('/diff')); c.destroy(); TestBed.resetTestingModule();
    }
  });

  it('navigates inventory pages without retargeting the snapshot or repeating requests', async () => {
    const c = await create(); c.request().flush(ready(120)); c.settle(); c.page.files(50); c.settle();
    const next = c.request(); expect(next.request.params.get('snapshot')).toBe(snapshot);
    expect(next.request.params.get('offset')).toBe('50'); next.flush(ready(120, 50)); c.settle();
    expect(c.query.value.get('offset')).toBe('50'); c.page.previousFiles(50); c.settle();
    c.request().flush(ready(120)); c.settle(); c.http.expectNone(req => req.url.endsWith('/diff')); c.destroy();
  });
});
