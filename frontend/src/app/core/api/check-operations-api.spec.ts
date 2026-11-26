import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { describe, it, expect } from 'vitest';
import { CheckOperationsApi } from './check-operations-api';

describe('check-operation transport', () => {
  function setup() {
    TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting()] });
    return { api: TestBed.inject(CheckOperationsApi), http: TestBed.inject(HttpTestingController) };
  }
  it('binds bounded discovery to the actor and encodes the proposal and cursor as parameters', () => {
    const { api, http } = setup(); api.page('actor', 'proposal/with space', 'cursor').subscribe();
    const request = http.expectOne(v => v.url === '/api/v1/proposals/proposal%2Fwith%20space/review/checks');
    expect(request.request.method).toBe('GET'); expect(request.request.headers.get('X-Expected-Actor')).toBe('actor');
    expect(request.request.params.keys().sort()).toEqual(['after', 'limit']); expect(request.request.params.get('limit')).toBe('20');
    expect(request.request.params.get('after')).toBe('cursor'); request.flush({}); http.verify();
  });
  it('reads an exact encoded operation without producing another POST', () => {
    const { api, http } = setup(); api.read('actor', 'proposal', 'operation/one').subscribe();
    const request = http.expectOne('/api/v1/proposals/proposal/review/checks/operation%2Fone');
    expect(request.request.headers.get('X-Expected-Actor')).toBe('actor'); expect(request.request.method).toBe('GET');
    request.flush({}); http.expectNone(v => v.method === 'POST'); http.verify();
  });
  it('sends only the captured immutable recovery request with its expected actor', () => {
    const { api, http } = setup(); const body = Object.freeze({ request_id: 'request', expected_request_hash: 'a'.repeat(64) });
    api.recover('actor', 'proposal', 'operation', body).subscribe();
    const request = http.expectOne('/api/v1/proposals/proposal/review/checks/operation/recover');
    expect(request.request.method).toBe('POST'); expect(request.request.body).toBe(body);
    expect(request.request.headers.get('X-Expected-Actor')).toBe('actor'); expect(request.request.params.keys()).toEqual([]);
    request.flush({}); http.verify();
  });
});
