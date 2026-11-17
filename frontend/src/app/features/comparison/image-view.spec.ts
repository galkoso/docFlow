import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { SessionState } from '../../core/auth/session-state';
import { ComparisonImage } from './image-view';
import { IMAGE_DIGEST } from './image-state';
import { displayHash, imageResponse, imageScope } from './image.testing';

describe('Before and after image display', () => {
  let revoke: ReturnType<typeof vi.fn<(url: string) => void>>;
  beforeEach(() => {
    revoke = vi.fn<(url: string) => void>(); vi.stubGlobal('URL', class extends URL {
      static override createObjectURL(_blob: Blob | MediaSource): string { return 'blob:http://localhost/verified'; }
      static override revokeObjectURL(url: string): void { revoke(url); }
    });
  });
  afterEach(() => { vi.unstubAllGlobals(); });
  async function create() {
    const refresh = vi.fn();
    await TestBed.configureTestingModule({ imports: [ComparisonImage], providers: [provideHttpClient(), provideHttpClientTesting(),
      { provide: SessionState, useValue: { refresh } }, { provide: IMAGE_DIGEST, useValue: async () => displayHash }],
    }).compileComponents();
    const fixture = TestBed.createComponent(ComparisonImage), http = TestBed.inject(HttpTestingController);
    fixture.componentRef.setInput('scope', imageScope().file); fixture.detectChanges();
    const request = () => http.expectOne(req => req.url.endsWith('/image'));
    async function opened() { const response = imageResponse(); request().flush(response.body, { headers: response.headers }); await Promise.resolve(); fixture.detectChanges(); await fixture.whenStable(); fixture.detectChanges(); }
    const destroy = () => { fixture.destroy(); http.verify(); };
    return { fixture, http, request, opened, destroy, refresh };
  }
  it('labels the missing side and shows only a verified blob derivative after dimensions match', async () => {
    const c = await create(); await c.opened(); const element: HTMLElement = c.fixture.nativeElement;
    expect(element.textContent).toContain('לפני — לקריאה בלבד'); expect(element.textContent).toContain('התמונה אינה קיימת בצד זה');
    const image = element.querySelector('img')!; expect(image.src).toBe('blob:http://localhost/verified'); expect(image.hidden).toBe(true);
    Object.defineProperty(image, 'naturalWidth', { value: 1 }); Object.defineProperty(image, 'naturalHeight', { value: 1 });
    image.dispatchEvent(new Event('load')); await c.fixture.whenStable(); expect(image.hidden).toBe(false);
    expect(image.alt).toBe('תמונה אחרי השינוי'); expect(element.querySelector('pre')).toBeNull(); c.destroy();
    expect(revoke).toHaveBeenCalledWith('blob:http://localhost/verified');
  });
  it('revokes and reports decode failures while manual retry remains on the same snapshot', async () => {
    const c = await create(); await c.opened(); c.fixture.nativeElement.querySelector('img').dispatchEvent(new Event('error'));
    await c.fixture.whenStable(); expect(c.fixture.nativeElement.querySelector('img')).toBeNull();
    expect(revoke).toHaveBeenCalledWith('blob:http://localhost/verified'); c.fixture.nativeElement.querySelector('button').click();
    const retry = c.request(); expect(retry.request.params.get('snapshot')).toBe(imageScope().file.snapshot); c.destroy(); expect(retry.cancelled).toBe(true);
  });
  it('clears private previews and cancels pending reads when its parent removes the scope', async () => {
    const c = await create(); const pending = c.request(); c.fixture.componentRef.setInput('scope', null); await c.fixture.whenStable();
    expect(pending.cancelled).toBe(true); expect(c.fixture.nativeElement.querySelector('img')).toBeNull(); c.destroy();
  });
});
