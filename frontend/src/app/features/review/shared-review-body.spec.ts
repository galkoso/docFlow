import { signal } from '@angular/core';
import { By } from '@angular/platform-browser';
import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { SessionState } from '../../core/auth/session-state';
import { SharedReviewBody } from './shared-review-body';
import { rawScope, rawResponse } from '../comparison/raw.testing';
import { readableScope, readableResponse, unitResponse } from '../comparison/readable.testing';
import { ReadableView } from '../comparison/readable-view';
import { imageScope, imageEvidence, proofHeader, displayHash } from '../comparison/image.testing';
import { SharedReviewBodyApi } from '../../core/api/shared-review-body-api';
import { ReadableSide } from '../comparison/readable-side';
import { DocumentRenderer } from '../specification/document-renderer';
import { IMAGE_DIGEST } from '../comparison/image-state';

describe('Shared snapshot body rendering', () => {
  afterEach(() => vi.restoreAllMocks());
  async function setup(scope = rawScope(), mode: 'raw' | 'readable' = 'raw') {
    const profile = signal<{ user_id: string } | null>({ user_id: scope.actor });
    await TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting(),
      { provide: SessionState, useValue: { profile, refresh: vi.fn() } }, { provide: IMAGE_DIGEST, useValue: async () => displayHash }],
    }).compileComponents();
    const fixture = TestBed.createComponent(SharedReviewBody), body = fixture.componentInstance, http = TestBed.inject(HttpTestingController);
    fixture.componentRef.setInput('scope', scope); fixture.componentRef.setInput('mode', mode); fixture.detectChanges();
    const root = `/api/v1/proposals/${scope.proposal}/review/snapshots/${scope.snapshot}/files/${scope.file.id}`;
    const settle = async () => { await fixture.whenStable(); fixture.detectChanges(); };
    return { fixture, body, http, profile, root, settle };
  }
  it('renders only validated raw source with explicit side/change labels and cancels the next window on identity loss', async () => {
    const { fixture, body, http, profile, root, settle } = await setup();
    const request = http.expectOne(req => req.url === `${root}/raw`); expect(request.request.headers.get('X-Expected-Actor')).toBe(rawScope().actor);
    request.flush(rawResponse()); await settle();
    expect(fixture.nativeElement.textContent).toContain('− הוסר'); expect(fixture.nativeElement.textContent).toContain('לפני — לקריאה בלבד');
    expect(fixture.nativeElement.querySelector('pre').textContent).toBe('old');
    body.move('next'); const next = http.expectOne(req => req.url === `${root}/raw`);
    expect(next.request.params.get('chunk')).toBe('1'); profile.set(null); fixture.detectChanges(); await settle();
    expect(next.cancelled).toBe(true); expect(body.raw.data()).toBeNull(); expect(fixture.nativeElement.querySelector('pre')).toBeNull();
    body.move('retry'); http.expectNone(() => true); http.verify();
  });
  it('rejects a response from another snapshot and keeps raw HTML inert', async () => {
    const { fixture, body, http, root, settle } = await setup();
    const forged = rawResponse(); forged.snapshot_id = '00000000-0000-4000-8000-000000000099';
    http.expectOne(req => req.url === `${root}/raw`).flush(forged); await settle(); expect(body.raw.data()).toBeNull();
    expect(fixture.nativeElement.querySelector('pre')).toBeNull();
    body.move('retry'); const safe = rawResponse(); safe.raw.segments[0].old_text = '<b>'; // Three exact bytes, rendered as text.
    http.expectOne(req => req.url === `${root}/raw`).flush(safe); await settle();
    expect(fixture.nativeElement.querySelector('pre').textContent).toBe('<b>'); expect(fixture.nativeElement.querySelector('pre b')).toBeNull(); http.verify();
  });
  it('uses shared endpoints for readable metadata and both bounded event sides, with URL selection output', async () => {
    const scope = readableScope(), { fixture, http, root, settle } = await setup(scope, 'readable');
    fixture.componentRef.setInput('mediaValidationVersion', 1); fixture.detectChanges();
    http.expectOne(req => req.url === `${root}/readable`).flush(readableResponse()); await settle();
    const view = fixture.debugElement.query(By.directive(ReadableView)).componentInstance as ReadableView;
    const row = view.state.data()!.page.rows.find(row => row.after?.kind === 'paragraph' && row.before?.kind === 'paragraph')!;
    const selected = vi.fn(); fixture.componentInstance.rowSelected.subscribe(selected); view.choose(row.change.id); fixture.detectChanges();
    for (const side of ['before', 'after'] as const) {
      const unit = row[side]!;
      const request = http.expectOne(req => req.url === `${root}/readable/units/${unit.index}` && req.params.get('side') === side);
      expect(request.request.headers.get('X-Expected-Actor')).toBe(scope.actor); expect(request.request.params.has('snapshot')).toBe(false);
      request.flush(unitResponse({ file: scope, side, unit }));
    }
    await settle(); expect(selected).toHaveBeenCalledWith(row.change.id);
    const sides = fixture.debugElement.queryAll(By.directive(ReadableSide));
    const before = sides.find(side => side.componentInstance.scope()?.side === 'before')!, after = sides.find(side => side.componentInstance.scope()?.side === 'after')!;
    expect(before.componentInstance.reviewMedia()).toBeNull(); expect(after.componentInstance.reviewMedia()).toEqual(fixture.componentInstance.mediaContext());
    expect(after.query(By.directive(DocumentRenderer)).componentInstance.reviewMedia()).toEqual(fixture.componentInstance.mediaContext());
    expect(fixture.nativeElement.textContent).toContain('אחרי'); http.expectNone(req => req.url.includes('/diff')); http.verify();
  });
  it('drops all visible source and reports denial without an automatic read retry', async () => {
    const { fixture, body, http, root, settle } = await setup(), denied = vi.fn(); body.accessDenied.subscribe(denied);
    http.expectOne(req => req.url === `${root}/raw`).flush({}, { status: 404, statusText: 'Denied' }); await settle();
    expect(denied).toHaveBeenCalledOnce(); expect(body.active()).toBeNull(); expect(fixture.nativeElement.querySelector('pre')).toBeNull();
    body.move('retry'); http.expectNone(() => true); http.verify();
  });
  it('verifies image proof and body before creating a URL and revokes it on scope loss', async () => {
    const image = imageScope(), created = vi.spyOn(URL, 'createObjectURL').mockReturnValue('blob:shared'), revoked = vi.spyOn(URL, 'revokeObjectURL').mockImplementation(() => {});
    const { fixture, http, profile, root, settle } = await setup(image.file);
    const request = http.expectOne(req => req.url === `${root}/image`);
    expect(request.request.params.get('side')).toBe('after');
    request.flush(new Blob(['PNG'], { type: 'image/png' }), { headers: { 'Content-Type': 'image/png', 'X-Comparison-Image': proofHeader(imageEvidence(image)) } });
    await settle(); expect(created).toHaveBeenCalledOnce(); expect(fixture.nativeElement.querySelector('img').src).toBe('blob:shared');
    profile.set(null); fixture.detectChanges(); await settle(); expect(revoked).toHaveBeenCalledWith('blob:shared'); http.verify();
  });
  it('cancels a selected file read on destruction', async () => {
    const { fixture, http, root } = await setup(); const request = http.expectOne(req => req.url === `${root}/raw`);
    fixture.destroy(); expect(request.cancelled).toBe(true); http.verify();
  });
  it('hides the prior raw result immediately when its input file identity changes', async () => {
    const { fixture, body, http, root, settle } = await setup();
    http.expectOne(req => req.url === `${root}/raw`).flush(rawResponse()); await settle(); expect(body.rawView()).not.toBeNull();
    const next = rawScope(); next.file.id = '9'.repeat(64); fixture.componentRef.setInput('scope', next);
    expect(body.rawView()).toBeNull(); fixture.detectChanges();
    const request = http.expectOne(req => req.url.includes(`/files/${next.file.id}/raw`)); fixture.destroy(); expect(request.cancelled).toBe(true); http.verify();
  });

  it('purges existing raw content when an inline media request is denied', async () => {
    const { fixture, body, http, root, settle } = await setup(), denied = vi.fn(); body.accessDenied.subscribe(denied);
    http.expectOne(req => req.url === `${root}/raw`).flush(rawResponse()); await settle(); expect(body.rawView()).not.toBeNull();
    const api = fixture.debugElement.injector.get(SharedReviewBodyApi), scope = rawScope();
    api.mediaImage(scope.actor, scope.proposal, scope.snapshot, 'a'.repeat(64) + '.png').subscribe({ error: () => {} });
    http.expectOne(req => req.url.includes('/media/')).flush(new Blob(), { status: 403, statusText: 'Denied' }); await settle();
    expect(denied).toHaveBeenCalledOnce(); expect(body.rawView()).toBeNull(); expect(body.active()).toBeNull(); http.verify();
  });

});
