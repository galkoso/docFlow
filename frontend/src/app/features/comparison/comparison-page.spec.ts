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
import { readableResponse, readableScope, unitResponse, unitScope } from './readable.testing';
import { ReadableView } from './readable-view';
import { By } from '@angular/platform-browser';
import { IMAGE_DIGEST } from './image-state';
import { displayHash, imageResponse, imageScope } from './image.testing';

describe('Routed comparison page', () => {
  async function create(initial: Record<string, string | string[]> = { snapshot }, mode: 'raw' | 'readable' = 'raw') {
    const profile = signal<{ user_id: string } | null>({ user_id: actor }), logoutEpoch = signal(0);
    const params = new BehaviorSubject(convertToParamMap({ id: proposal })), query = new BehaviorSubject(convertToParamMap({ view: mode, ...initial }));
    await TestBed.configureTestingModule({ imports: [ComparisonPage], providers: [provideHttpClient(), provideHttpClientTesting(), provideRouter([]),
      { provide: IMAGE_DIGEST, useValue: async () => displayHash },
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
    page.mode.set(mode);
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
  it('loads readable content by default and cancels its private request when switching to raw or losing identity', async () => {
    const c = await create({ snapshot }, 'readable'), response = ready(); response.page!.files[0] = readableScope().file;
    c.request().flush(response); c.settle(); c.page.select(response.page!.files[0].id); c.settle();
    c.http.expectOne(req => req.url.endsWith('/readable')).flush(readableResponse()); c.settle();
    const view = c.fixture.debugElement.query(By.directive(ReadableView)).componentInstance as ReadableView;
    const row = view.state.data()!.page.rows.find(row => row.before?.kind === 'paragraph')!;
    view.state.select(row.change.id); c.settle();
    for (const side of ['before', 'after'] as const) {
      const scope = unitScope('paragraph', side);
      c.http.expectOne(req => req.url.endsWith(`/units/${scope.unit.index}`) && req.params.get('side') === side).flush(unitResponse(scope));
    }
    c.settle(); expect(c.fixture.nativeElement.querySelectorAll('app-document-renderer')).toHaveLength(2);
    view.state.select(view.state.data()!.page.rows.find(row => row.before?.kind === 'code')!.change.id); c.settle();
    const requests = c.http.match(req => req.url.includes('/units/')); expect(requests).toHaveLength(2);
    c.page.mode.set('raw'); c.settle(); expect(requests.every(request => request.cancelled)).toBe(true);
    const raw = c.body(); expect(raw.request.url.endsWith(response.page!.files[0].id)).toBe(true);
    c.profile.set(null); c.settle(); expect(raw.cancelled).toBe(true); expect(c.page.raw.data()).toBeNull();
    expect(c.fixture.nativeElement.querySelector('app-readable-view')).toBeNull(); c.destroy();
  });

  it('selects the dedicated image viewer and destroys the private object URL on identity loss', async () => {
    const revoke = vi.fn<(url: string) => void>();
    vi.stubGlobal('URL', class extends URL {
      static override createObjectURL(_blob: Blob | MediaSource): string { return 'blob:http://localhost/image'; }
      static override revokeObjectURL(url: string): void { revoke(url); }
    });
    try {
      const c = await create(), readyImage = ready(); readyImage.page!.files[0] = imageScope().file.file;
      c.request().flush(readyImage); c.settle(); c.page.select(readyImage.page!.files[0].id); c.settle();
      const image = c.http.expectOne(req => req.url.endsWith('/image')); expect(image.request.params.get('side')).toBe('after');
      const response = imageResponse(); image.flush(response.body, { headers: response.headers }); await Promise.resolve(); c.settle();
      expect(c.fixture.nativeElement.querySelector('app-comparison-image img')).not.toBeNull();
      expect(c.fixture.nativeElement.querySelector('app-readable-view')).toBeNull();
      c.http.expectNone(req => req.url.endsWith('/readable')); c.profile.set(null); c.settle();
      expect(c.fixture.nativeElement.querySelector('img')).toBeNull(); expect(revoke).toHaveBeenCalledWith('blob:http://localhost/image'); c.destroy();
    } finally { vi.unstubAllGlobals(); }
  });

  it('restores a file and paired unit directly from a frozen link without preparing or scanning list pages', async () => {
    const metadata = readableResponse(), target = metadata.page.rows.find(row => row.after?.kind === 'heading')!.change.id;
    const c = await create({ snapshot, file: readableScope().file.id, unit: target, view: 'readable' }, 'readable');
    const root = c.request(); expect(root.request.params.get('file')).toBe(readableScope().file.id);
    expect(root.request.params.has('offset')).toBe(false); expect(root.request.params.has('request_id')).toBe(false);
    const response = ready(); response.page!.files[0] = readableScope().file; root.flush(response); c.settle();
    const units = c.http.expectOne(req => req.url.endsWith('/readable'));
    expect(units.request.params.get('row')).toBe(target); expect(units.request.params.has('offset')).toBe(false);
    units.flush(metadata); c.settle();
    for (const side of ['before', 'after'] as const) {
      const scope = unitScope('heading', side);
      c.http.expectOne(req => req.url.endsWith(`/units/${scope.unit.index}`) && req.params.get('side') === side).flush(unitResponse(scope));
    }
    c.settle(); expect(c.fixture.nativeElement.querySelector('app-document-renderer h2').textContent).toBe('כותרת');
    const href = c.page.selectionHref()!; expect(href).toContain(`snapshot=${snapshot}`); expect(href).toContain(`unit=${target}`);
    expect(href).not.toContain('actor'); c.http.expectNone(req => req.url.endsWith('/diff'));
    c.page.fresh(); c.settle(); const fresh = c.request(); expect(fresh.request.params.has('file')).toBe(false);
    expect(c.query.value.has('file')).toBe(false); expect(c.query.value.has('unit')).toBe(false);
    fresh.flush(pending()); c.settle(); c.destroy();
  });

  it('restores browser back selection and writes view changes without new preparation or redundant inventory reads', async () => {
    const c = await create(); c.request().flush(ready(2)); c.settle();
    c.page.select(file().id); c.settle(); c.body().flush(rawResponse()); c.settle(); expect(c.query.value.get('file')).toBe(file().id);
    c.page.select(file(1).id); c.settle(); const next = rawResponse(); next.file = file(1); c.body().flush(next); c.settle();
    c.query.next(convertToParamMap({ snapshot, file: file().id, view: 'raw' })); c.settle(); c.body().flush(rawResponse()); c.settle();
    expect(c.page.state.selected()?.id).toBe(file().id); c.http.expectNone(req => req.url.endsWith('/diff'));
    c.page.setMode('readable'); c.settle(); expect(c.query.value.get('view')).toBe('readable');
    const metadata = c.http.expectOne(req => req.url.endsWith('/readable'));
    c.page.setMode('raw'); c.settle(); expect(metadata.cancelled).toBe(true); c.body().flush(rawResponse()); c.settle();
    expect(c.query.value.get('view')).toBe('raw'); c.destroy();
  });

  it('returns from an unknown file locator to the existing snapshot list without silently replacing the snapshot', async () => {
    const c = await create({ snapshot, file: '9'.repeat(64) });
    c.request().flush({ error: { code: 'NOT_FOUND' } }, { status: 404, statusText: 'Not found' }); c.settle();
    expect(c.page.state.snapshot()).toBe(snapshot); expect(c.page.state.selected()).toBeNull();
    c.page.clearFile(); c.settle(); const list = c.request(); expect(list.request.params.has('file')).toBe(false);
    expect(list.request.params.get('snapshot')).toBe(snapshot); list.flush(ready()); c.settle(); c.destroy();
  });

  it('does not silently apply a readable-unit locator to an image file', async () => {
    const source = imageScope(), c = await create({ snapshot, file: source.file.file.id, unit: 'd'.repeat(64), view: 'readable' }, 'readable');
    const response = ready(); response.page!.files[0] = source.file.file; c.request().flush(response); c.settle();
    expect(c.page.invalidUnitKind()).toBe(true); expect(c.fixture.nativeElement.querySelector('[role=alert]').textContent).toContain('אין יחידות טקסט');
    c.http.expectNone(req => req.url.endsWith('/image') || req.url.endsWith('/readable'));
    c.page.selectUnit(null); c.settle(); const image = c.http.expectOne(req => req.url.endsWith('/image'));
    expect(image.request.params.get('snapshot')).toBe(snapshot); c.destroy(); expect(image.cancelled).toBe(true);
  });

});
