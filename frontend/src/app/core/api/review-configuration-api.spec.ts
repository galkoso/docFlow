import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { ReviewConfigurationApi, type ReviewConfigurationRequest } from './review-configuration-api';

describe('Review configuration request identity', () => {
  it('retries the captured request and actor after a lost response without adopting a later session', () => {
    TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting()] });
    const api = TestBed.inject(ReviewConfigurationApi);
    const http = TestBed.inject(HttpTestingController);
    const body: ReviewConfigurationRequest = {
      request_id: '00000000-0000-4000-8000-000000000003', expected_head_sha: '1'.repeat(40),
      expected_proposal_version: 7, expected_configuration_version: 3,
      settings: { ui_impact: 'no', ui_impact_reason: 'No UI change', assignments: [
        { reviewer_id: '00000000-0000-4000-8000-000000000004', domain: 'PRODUCT', required: true },
      ] },
    };
    const url = '/api/v1/proposals/proposal/review/configuration';
    api.update('original', 'proposal', body).subscribe({ error: () => {} });
    const initial = http.expectOne(url);
    expect(initial.request.method).toBe('PUT');
    expect(initial.request.headers.get('X-Expected-Actor')).toBe('original');
    expect(initial.request.body).toEqual(body);
    initial.error(new ProgressEvent('error'));
    api.read('another', 'proposal').subscribe();
    const other = http.expectOne(url);
    expect(other.request.method).toBe('GET');
    expect(other.request.headers.get('X-Expected-Actor')).toBe('another');
    other.flush({});
    api.update('original', 'proposal', body).subscribe({ error: () => {} });
    const retry = http.expectOne(url);
    expect(retry.request.headers.get('X-Expected-Actor')).toBe('original');
    expect(retry.request.body).toEqual(body);
    retry.flush({ error: { code: 'SESSION_IDENTITY_CHANGED' } }, { status: 409, statusText: 'Conflict' });
    http.verify();
  });

  it('encodes proposal scope as one path component', () => {
    TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting()] });
    const api = TestBed.inject(ReviewConfigurationApi);
    const http = TestBed.inject(HttpTestingController);
    api.read('actor', 'foreign/proposal?latest=true').subscribe();
    const read = http.expectOne('/api/v1/proposals/foreign%2Fproposal%3Flatest%3Dtrue/review/configuration');
    expect(read.request.params.keys()).toEqual([]);
    read.flush({});
    http.verify();
  });
});
