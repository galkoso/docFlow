import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { SharedReviewBodyApi } from './shared-review-body-api';

describe('Shared immutable file transport', () => {
  function setup() {
    TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting()] });
    return { api: TestBed.inject(SharedReviewBodyApi), http: TestBed.inject(HttpTestingController) };
  }
  it('encodes all path identities, captures actors and never sends a replaceable snapshot query or private diff request', () => {
    const { api, http } = setup(), args = ['captured-actor', 'p/foreign', 's?other', 'f#other'] as const;
    api.raw(...args, { chunk: 1, offset: 7 }, 1024).subscribe();
    api.readable(...args, 50, 20).subscribe(); api.readableRow(...args, 'row').subscribe();
    api.readableUnit(...args, 'after', 3, { event: 1, offset: 2 }, 2048).subscribe(); api.image(...args, 'after').subscribe();
    const requests = http.match(() => true);
    expect(requests).toHaveLength(5);
    for (const request of requests) {
      expect(request.request.method).toBe('GET'); expect(request.request.headers.get('X-Expected-Actor')).toBe(args[0]);
      expect(request.request.url).toMatch(/^\/api\/v1\/proposals\/p%2Fforeign\/review\/snapshots\/s%3Fother\/files\/f%23other\//);
      expect(request.request.params.has('snapshot')).toBe(false);
    }
    expect(requests[0].request.params.get('chunk')).toBe('1'); expect(requests[0].request.params.get('offset')).toBe('7');
    expect(requests[1].request.params.get('offset')).toBe('50'); expect(requests[2].request.params.get('row')).toBe('row');
    expect(requests[2].request.params.has('offset')).toBe(false); expect(requests[3].request.url).toContain('/units/3');
    expect(requests[3].request.params.get('event')).toBe('1'); expect(requests[4].request.responseType).toBe('blob');
    for (const request of requests) request.flush(request.request.responseType === 'blob' ? new Blob() : {});
    http.verify();
  });
  it.each([401, 403, 404])('latches access denial on HTTP %i without retrying', status => {
    const { api, http } = setup(); api.raw('a', 'p', 's', 'f').subscribe({ error: () => {} });
    http.expectOne(req => req.url.endsWith('/raw')).flush({}, { status, statusText: 'Denied' });
    expect(api.denied()).toBe(true); http.verify();
  });
  it('distinguishes temporary source failures from a captured-actor mismatch, including blob errors', async () => {
    const { api, http } = setup(); api.raw('a', 'p', 's', 'f').subscribe({ error: () => {} });
    http.expectOne(req => req.url.endsWith('/raw')).flush({}, { status: 503, statusText: 'Unavailable' });
    expect(api.denied()).toBe(false);
    api.image('a', 'p', 's', 'f', 'after').subscribe({ error: () => {} });
    http.expectOne(req => req.url.endsWith('/image')).flush(new Blob([JSON.stringify({ error: { code: 'SESSION_IDENTITY_CHANGED' } })]),
      { status: 409, statusText: 'Conflict' });
    await Promise.resolve(); await Promise.resolve(); expect(api.denied()).toBe(true); http.verify();
  });
  it('encodes sealed media locators and carries the captured actor without any ref query', () => {
    const { api, http } = setup(); api.mediaImage('actor', 'p/other', 's?other', 'f#other.png').subscribe();
    const request = http.expectOne('/api/v1/proposals/p%2Fother/review/snapshots/s%3Fother/media/f%23other.png/image');
    expect(request.request.headers.get('X-Expected-Actor')).toBe('actor'); expect(request.request.params.keys()).toEqual([]);
    expect(request.request.responseType).toBe('blob'); request.flush(new Blob()); http.verify();
  });

});
