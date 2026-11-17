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
});
