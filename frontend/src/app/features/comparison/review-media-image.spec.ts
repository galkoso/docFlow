import { signal } from '@angular/core';
import { By } from '@angular/platform-browser';
import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { SessionState } from '../../core/auth/session-state';
import { DocumentRenderer } from '../specification/document-renderer';
import type { RenderEvent } from '../specification/render-model';
import { IMAGE_DIGEST } from './image-state';
import { ReviewMediaImage } from './review-media-image';
import { mediaContext, mediaScope, mediaResponse, mediaId, displayHash, beforeMediaContext, beforeMediaResponse } from './review-media.testing';

describe('Lazy shared inline image rendering', () => {
  afterEach(() => { vi.restoreAllMocks(); vi.unstubAllGlobals(); });
  async function setup(path = mediaScope().path, context = mediaContext()) {
    const profile = signal<{ user_id: string } | null>({ user_id: mediaContext().actor });
    vi.spyOn(URL, 'createObjectURL').mockReturnValue('blob:inline-sealed');
    const revoke = vi.spyOn(URL, 'revokeObjectURL').mockImplementation(() => {});
    await TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting(),
      { provide: SessionState, useValue: { profile } },
      { provide: IMAGE_DIGEST, useValue: async (blob: Blob) => blob.type === 'image/png' ? displayHash : mediaId }],
    }).compileComponents();
    const fixture = TestBed.createComponent(DocumentRenderer), http = TestBed.inject(HttpTestingController);
    const events: RenderEvent[] = [{ kind: 'image', source_start_byte: 0, source_end_byte: 100, destination: path, text: '<script>alt</script>', title: 'Synthetic' }];
    fixture.componentRef.setInput('events', events); fixture.componentRef.setInput('reviewMedia', context); fixture.detectChanges();
    const settle = async () => { await fixture.whenStable(); fixture.detectChanges(); };
    await settle();
    const child = fixture.debugElement.query(By.directive(ReviewMediaImage)).componentInstance as ReviewMediaImage;
    return { fixture, http, profile, revoke, child, settle };
  }
  async function flush(http: HttpTestingController) {
    const response = mediaResponse(), pending = http.expectOne(req => req.url.includes('/media/') && req.url.endsWith('/image'));
    expect(pending.request.headers.get('X-Expected-Actor')).toBe(mediaContext().actor);
    pending.flush(response.body, { headers: response.headers });
  }
  it('offers an accessible explicit load without viewport support and shows only decoded verified pixels with inert alt text', async () => {
    vi.stubGlobal('IntersectionObserver', undefined);
    const { fixture, http, child, settle } = await setup(); http.expectNone(() => true);
    const button = fixture.nativeElement.querySelector('button') as HTMLButtonElement;
    expect(button.textContent).toContain('טעינת תמונה'); button.click(); fixture.detectChanges(); await flush(http); await settle();
    const image = fixture.nativeElement.querySelector('img') as HTMLImageElement;
    expect(image.src).toBe('blob:inline-sealed'); expect(image.hidden).toBe(true); expect(getComputedStyle(image).display).toBe('none'); expect(image.alt).toBe('<script>alt</script>');
    expect(fixture.nativeElement.querySelector('script')).toBeNull();
    Object.defineProperty(image, 'naturalWidth', { value: 1 }); Object.defineProperty(image, 'naturalHeight', { value: 1 });
    image.dispatchEvent(new Event('load')); fixture.detectChanges(); expect(image.hidden).toBe(false); expect(child.state.loaded()).toBe(true); http.verify();
  });
  it('loads once near the viewport and leaves provider failures for explicit retry', async () => {
    const observers: Array<{ callback: IntersectionObserverCallback; observed: Element[]; disconnect: ReturnType<typeof vi.fn> }> = [];
    vi.stubGlobal('IntersectionObserver', class {
      observed: Element[] = []; disconnect = vi.fn();
      constructor(readonly callback: IntersectionObserverCallback) { observers.push(this); }
      observe(element: Element) { this.observed.push(element); }
    });
    const { fixture, http, settle } = await setup();
    const observer = observers.find(observer => observer.observed.some(element => element.tagName === 'APP-REVIEW-MEDIA-IMAGE'))!;
    expect(observer).toBeDefined(); http.expectNone(() => true);
    const enter = (isIntersecting: boolean) => observer.callback([{ isIntersecting } as IntersectionObserverEntry], observer as unknown as IntersectionObserver);
    enter(false); http.expectNone(() => true); enter(true);
    http.expectOne(req => req.url.includes('/media/')).flush(new Blob(), { status: 503, statusText: 'Backoff' }); await settle();
    enter(true); http.expectNone(() => true); expect(observer.disconnect).toHaveBeenCalled();
    (fixture.nativeElement.querySelector('button') as HTMLButtonElement).click(); fixture.detectChanges();
    const retry = http.expectOne(req => req.url.includes('/media/')); fixture.destroy(); expect(retry.cancelled).toBe(true); http.verify();
  });
  it('clears pixels on snapshot selection change and never falls back to a book asset route', async () => {
    const { fixture, http, child, revoke, settle } = await setup(); child.load(); await flush(http); await settle();
    const next = { ...mediaContext(), snapshot: '00000000-0000-4000-8000-000000000099' };
    fixture.componentRef.setInput('reviewMedia', next); fixture.detectChanges(); await settle();
    expect(revoke).toHaveBeenCalledWith('blob:inline-sealed'); expect(fixture.nativeElement.querySelector('img')).toBeNull();
    child.load(); const request = http.expectOne(req => req.url.includes(`/snapshots/${next.snapshot}/media/`));
    expect(request.request.params.keys()).toEqual([]); fixture.destroy(); expect(request.cancelled).toBe(true);
    http.expectNone(req => req.url.includes('/assets/')); http.verify();
  });
  it('cancels in-flight pixels and blocks queued loads during session refresh or account switch', async () => {
    const { fixture, http, child, profile, settle } = await setup(); child.load(); const request = http.expectOne(req => req.url.includes('/media/'));
    profile.set(null); child.load(); fixture.detectChanges(); await settle(); expect(request.cancelled).toBe(true);
    expect(child.current()).toBeNull(); expect(fixture.nativeElement.querySelector('img')).toBeNull();
    profile.set({ user_id: '00000000-0000-4000-8000-000000000099' }); fixture.detectChanges(); child.load(); http.expectNone(() => true); http.verify();
  });
  it.each(['https://tracker.example/pixel', 'assets/../private.png'])('keeps unsupported destinations inert: %s', async path => {
    const { fixture, http, child } = await setup(path); child.load();
    expect(fixture.nativeElement.querySelector('img')).toBeNull(); expect(fixture.nativeElement.textContent).toContain('ההפניה אינה זמינה'); http.verify();
  });
  it('renders before pixels only through the document-bound endpoint and revokes them when document proof changes', async () => {
    vi.stubGlobal('IntersectionObserver', undefined);
    const context = beforeMediaContext(), { fixture, child, http, revoke, settle } = await setup(mediaScope().path, context);
    child.load(); const request = http.expectOne(req => req.url.includes(`/files/${context.beforeDocument!.file}/before-media/`));
    expect(request.request.params.keys()).toEqual([]); const response = beforeMediaResponse(); request.flush(response.body, { headers: response.headers }); await settle();
    const image = fixture.nativeElement.querySelector('img') as HTMLImageElement;
    expect(image.src).toBe('blob:inline-sealed'); expect(getComputedStyle(image).display).toBe('none');
    fixture.componentRef.setInput('reviewMedia', { ...context, beforeDocument: { ...context.beforeDocument!, sha256: 'f'.repeat(64) } });
    fixture.detectChanges(); await settle(); expect(revoke).toHaveBeenCalledWith('blob:inline-sealed'); expect(fixture.nativeElement.querySelector('img')).toBeNull();
    http.expectNone(() => true); http.verify();
  });

});
