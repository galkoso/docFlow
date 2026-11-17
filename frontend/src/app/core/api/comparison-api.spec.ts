import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { ComparisonApi } from './comparison-api';

describe('Pinned comparison requests', () => {
  it('replays the original selection and actor after an uncertain result and another actor polling', () => {
    TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting()] });
    const api = TestBed.inject(ComparisonApi);
    const http = TestBed.inject(HttpTestingController);
    const selection = { request_id: 'original-request', head: '1'.repeat(40), target: '2'.repeat(40) };
    const deferred = api.prepare('original', 'proposal', selection);
    selection.head = '3'.repeat(40);
    deferred.subscribe({ error: () => {} });
    const first = http.expectOne(request => request.url === '/api/v1/proposals/proposal/diff');
    expect(first.request.params.get('head')).toBe('1'.repeat(40));
    expect(first.request.params.get('target')).toBe('2'.repeat(40));
    expect(first.request.headers.get('X-Expected-Actor')).toBe('original');
    first.error(new ProgressEvent('error'));
    http.expectNone(request => request.url.includes('/diff'));
    api.job('another', 'job').subscribe();
    const poll = http.expectOne('/api/v1/jobs/job');
    expect(poll.request.headers.get('X-Expected-Actor')).toBe('another');
    poll.flush({});
    deferred.subscribe({ error: () => {} });
    const retry = http.expectOne(request => request.url === '/api/v1/proposals/proposal/diff');
    expect(retry.request.params.toString()).toBe(first.request.params.toString());
    expect(retry.request.headers.get('X-Expected-Actor')).toBe('original');
    retry.flush({}, { status: 409, statusText: 'Conflict' });
    http.verify();
  });

  it('polls and pages only the accepted snapshot without preparing or reselecting refs', () => {
    TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting()] });
    const api = TestBed.inject(ComparisonApi);
    const http = TestBed.inject(HttpTestingController);
    for (const offset of [0, 50]) {
      api.page('captured', 'proposal/with slash', 'saved-snapshot', offset, 50).subscribe();
      const page = http.expectOne(request => request.url === '/api/v1/proposals/proposal%2Fwith%20slash/diff');
      expect(page.request.method).toBe('GET');
      expect(page.request.headers.get('X-Expected-Actor')).toBe('captured');
      expect(page.request.params.keys().sort()).toEqual(['limit', 'offset', 'snapshot']);
      expect(page.request.params.get('snapshot')).toBe('saved-snapshot');
      expect(page.request.params.get('offset')).toBe(String(offset));
      page.flush({});
    }
    http.verify();
  });

  it('freezes a raw window cursor and snapshot across deferred subscriptions and identity changes', () => {
    TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting()] });
    const api = TestBed.inject(ComparisonApi);
    const http = TestBed.inject(HttpTestingController);
    const cursor = { chunk: 2, offset: 1024 };
    const window = api.raw('captured', 'proposal', 'snapshot', 'file', cursor, 4096);
    cursor.offset = 9999;
    window.subscribe({ error: () => {} });
    const first = http.expectOne(request => request.url === '/api/v1/proposals/proposal/diff/files/file');
    expect(first.request.headers.get('X-Expected-Actor')).toBe('captured');
    expect(first.request.params.keys().sort()).toEqual(['chunk', 'max_bytes', 'offset', 'snapshot']);
    expect(first.request.params.get('snapshot')).toBe('snapshot');
    expect(first.request.params.get('offset')).toBe('1024');
    expect(first.request.params.get('max_bytes')).toBe('4096');
    first.error(new ProgressEvent('error'));
    http.expectNone(request => request.url.includes('/diff'));
    api.job('another', 'job').subscribe();
    const other = http.expectOne('/api/v1/jobs/job');
    expect(other.request.headers.get('X-Expected-Actor')).toBe('another');
    other.flush({});
    window.subscribe();
    const retry = http.expectOne(request => request.url === '/api/v1/proposals/proposal/diff/files/file');
    expect(retry.request.params.toString()).toBe(first.request.params.toString());
    expect(retry.request.headers.get('X-Expected-Actor')).toBe('captured');
    retry.flush({});
    http.verify();
  });


  it('pages readable rows using only the accepted snapshot and captured actor', () => {
    TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting()] });
    const api = TestBed.inject(ComparisonApi);
    const http = TestBed.inject(HttpTestingController);
    api.readable('captured', 'proposal/one', 'snapshot', 'file', 25, 25).subscribe();
    const page = http.expectOne(request => request.url === '/api/v1/proposals/proposal%2Fone/diff/files/file/readable');
    expect(page.request.method).toBe('GET');
    expect(page.request.headers.get('X-Expected-Actor')).toBe('captured');
    expect(page.request.params.keys().sort()).toEqual(['limit', 'offset', 'snapshot']);
    expect(page.request.params.get('snapshot')).toBe('snapshot');
    expect(page.request.params.get('offset')).toBe('25');
    page.flush({});
    http.verify();
  });

  it('retains readable side and decoded event coordinates across subscription and exact manual replay', () => {
    TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting()] });
    const api = TestBed.inject(ComparisonApi);
    const http = TestBed.inject(HttpTestingController);
    const cursor = { event: 5, offset: 1024 };
    const intent = api.readableUnit('original', 'proposal', 'snapshot', 'file', 'before', 7, cursor, 4096);
    cursor.event = 100;
    cursor.offset = 9000;
    intent.subscribe({ error: () => {} });
    const first = http.expectOne(request => request.url === '/api/v1/proposals/proposal/diff/files/file/readable/units/7');
    expect(first.request.headers.get('X-Expected-Actor')).toBe('original');
    expect(first.request.params.keys().sort()).toEqual(['event', 'max_bytes', 'offset', 'side', 'snapshot']);
    expect(first.request.params.get('event')).toBe('5');
    expect(first.request.params.get('offset')).toBe('1024');
    expect(first.request.params.get('side')).toBe('before');
    expect(first.request.params.get('snapshot')).toBe('snapshot');
    first.error(new ProgressEvent('error'));
    http.expectNone(request => request.url.includes('/readable'));
    api.job('another', 'job').subscribe();
    const other = http.expectOne('/api/v1/jobs/job');
    expect(other.request.headers.get('X-Expected-Actor')).toBe('another');
    other.flush({});
    intent.subscribe();
    const retry = http.expectOne(request => request.url === '/api/v1/proposals/proposal/diff/files/file/readable/units/7');
    expect(retry.request.headers.get('X-Expected-Actor')).toBe('original');
    expect(retry.request.params.toString()).toBe(first.request.params.toString());
    retry.flush({ state: 'RAW_REQUIRED', window: null });
    http.verify();
  });

  it('loads a comparison image as a captured-actor blob response with only immutable side and snapshot selection', () => {
    TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting()] });
    const api = TestBed.inject(ComparisonApi), http = TestBed.inject(HttpTestingController);
    const request = api.image('captured', 'proposal/one', 'snapshot', 'file', 'before'); request.subscribe({ error: () => {} });
    const first = http.expectOne(req => req.url.endsWith('/file/image'));
    expect(first.request.url).toContain('/proposal%2Fone/'); expect(first.request.headers.get('X-Expected-Actor')).toBe('captured');
    expect(first.request.responseType).toBe('blob'); expect(first.request.params.keys().sort()).toEqual(['side', 'snapshot']);
    expect(first.request.params.get('side')).toBe('before'); first.error(new ProgressEvent('error'));
    request.subscribe(); const retry = http.expectOne(req => req.url.endsWith('/file/image'));
    expect(retry.request.params.toString()).toBe(first.request.params.toString()); retry.flush(new Blob([], { type: 'image/png' })); http.verify();
  });

});
