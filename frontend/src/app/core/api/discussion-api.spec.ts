import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { DiscussionApi } from './discussion-api';

describe('Discussion API', () => {
  beforeEach(() => TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting()] }));
  afterEach(() => TestBed.inject(HttpTestingController).verify());
  it('encodes resource identities and binds reads to the captured actor, cursor and version', () => {
    const api = TestBed.inject(DiscussionApi), http = TestBed.inject(HttpTestingController);
    api.threads('captured', 'p/a', 'cursor').subscribe();
    const list = http.expectOne(req => req.url === '/api/v1/proposals/p%2Fa/threads');
    expect(list.request.method).toBe('GET'); expect(list.request.params.get('after')).toBe('cursor');
    expect(list.request.headers.get('X-Expected-Actor')).toBe('captured'); list.flush({});
    api.comments('captured', 'p/a', 't/b', 7, 'message').subscribe();
    const detail = http.expectOne(req => req.url === '/api/v1/proposals/p%2Fa/threads/t%2Fb');
    expect(detail.request.method).toBe('GET'); expect(detail.request.params.get('expected_thread_version')).toBe('7');
    expect(detail.request.params.get('after')).toBe('message'); expect(detail.request.headers.get('X-Expected-Actor')).toBe('captured'); detail.flush({});
  });
  it('posts the original action body to the encoded thread without reconstruction', () => {
    const api = TestBed.inject(DiscussionApi), http = TestBed.inject(HttpTestingController);
    const body = { request_id: 'nonce', expected_thread_version: 7, mutation: { kind: 'resolve' as const } };
    api.write('captured', 'p/a', { kind: 'action', thread: 't/b', body }).subscribe();
    const req = http.expectOne('/api/v1/proposals/p%2Fa/threads/t%2Fb/actions'); expect(req.request.method).toBe('POST');
    expect(req.request.body).toBe(body); expect(req.request.headers.get('X-Expected-Actor')).toBe('captured'); req.flush({});
  });
});
