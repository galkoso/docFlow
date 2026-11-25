import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { actor, proposal, snapshot, other, configuration, detail } from '../../../testing/review-fixtures';
import { ReviewApi } from './review-api';
import { submissionContext } from '../../features/review/submission-evidence';

describe('Review API request scope', () => {
  it('reads stored readiness for the captured actor without supplying gate evidence or performing a write', () => {
    TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting()] });
    const api = TestBed.inject(ReviewApi), http = TestBed.inject(HttpTestingController);
    api.readiness(actor, 'p/other?x=1', 'h/other').subscribe();
    const read = http.expectOne('/api/v1/proposals/p%2Fother%3Fx%3D1/review/snapshots/h%2Fother/readiness');
    expect(read.request.method).toBe('GET'); expect(read.request.headers.get('X-Expected-Actor')).toBe(actor);
    expect(read.request.params.keys()).toEqual([]); expect(read.request.body).toBeNull();
    read.flush({}); http.expectNone(req => req.method === 'POST'); http.verify();
  });
  it('encodes every identifier and carries the captured actor on submission, context, jobs and shared reads', () => {
    TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting()] });
    const api = TestBed.inject(ReviewApi), http = TestBed.inject(HttpTestingController), context = submissionContext(configuration(), detail(), proposal)!;
    api.submit(actor, 'p/foreign?x=1', 'original-key', context.body).subscribe();
    const post = http.expectOne('/api/v1/proposals/p%2Fforeign%3Fx%3D1/submit');
    expect(post.request.headers.get('Idempotency-Key')).toBe('original-key'); expect(post.request.headers.get('X-Expected-Actor')).toBe(actor); post.flush({});
    api.detail(actor, proposal).subscribe(); const detailRequest = http.expectOne(`/api/v1/proposals/${proposal}`);
    expect(detailRequest.request.headers.get('X-Expected-Actor')).toBe(actor); detailRequest.flush({});
    api.job(actor, 'job/other').subscribe(); const job = http.expectOne('/api/v1/jobs/job%2Fother');
    expect(job.request.headers.get('X-Expected-Actor')).toBe(actor); job.flush({});
    api.history(actor, proposal, other).subscribe(); const history = http.expectOne(req => req.url.endsWith('/review/snapshots'));
    expect(history.request.params.get('after')).toBe(other); expect(history.request.params.get('limit')).toBe('20'); history.flush({});
    api.snapshot(actor, proposal, 'snap/other').subscribe(); http.expectOne(`/api/v1/proposals/${proposal}/review/snapshots/snap%2Fother`).flush({});
    api.documents(actor, 'p/other', 'snap/other', 20).subscribe();
    const documents = http.expectOne(req => req.url === '/api/v1/proposals/p%2Fother/review/snapshots/snap%2Fother/documents');
    expect(documents.request.method).toBe('GET'); expect(documents.request.headers.get('X-Expected-Actor')).toBe(actor);
    expect(documents.request.params.get('offset')).toBe('20'); expect(documents.request.params.get('limit')).toBe('20'); documents.flush({});
    api.units(actor, 'p/other', 'snap/other', 40).subscribe();
    const units = http.expectOne(req => req.url === '/api/v1/proposals/p%2Fother/review/snapshots/snap%2Fother/units');
    expect(units.request.method).toBe('GET'); expect(units.request.headers.get('X-Expected-Actor')).toBe(actor);
    expect(units.request.params.get('offset')).toBe('40'); expect(units.request.params.get('limit')).toBe('20'); units.flush({});
    api.files(actor, proposal, snapshot, 50).subscribe(); api.media(actor, proposal, snapshot, 100).subscribe();
    const pages = http.match(req => req.url.endsWith('/files') || req.url.endsWith('/media'));
    expect(pages.map(request => request.request.params.get('offset'))).toEqual(['50', '100']);
    for (const request of pages) { expect(request.request.headers.get('X-Expected-Actor')).toBe(actor); expect(request.request.params.get('limit')).toBe('50'); request.flush({}); }
    http.verify();
  });
  it('keeps source delivery reads separate from a frozen explicit acknowledgement intent', () => {
    TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting()] });
    const api = TestBed.inject(ReviewApi), http = TestBed.inject(HttpTestingController);
    const source = Object.freeze({ unit_id: 'unit/other', expected_fingerprint: 'a'.repeat(64), domain: 'PRODUCT' as const, side: 'before' as const, offset: 65536 });
    api.source(actor, 'proposal/other', 'snapshot/other', source).subscribe();
    const read = http.expectOne(req => req.url === '/api/v1/proposals/proposal%2Fother/review/snapshots/snapshot%2Fother/units/unit%2Fother/source');
    expect(read.request.method).toBe('GET'); expect(read.request.headers.get('X-Expected-Actor')).toBe(actor);
    expect(read.request.params.get('expected_fingerprint')).toBe(source.expected_fingerprint);
    expect(read.request.params.get('domain')).toBe('PRODUCT'); expect(read.request.params.get('side')).toBe('before');
    expect(read.request.params.get('offset')).toBe('65536'); read.flush({}); http.expectNone(req => req.method === 'POST');
    const intent = Object.freeze({ request_id: other, delivery_id: snapshot, expected_fingerprint: source.expected_fingerprint, domain: 'PRODUCT' as const });
    api.acknowledge(actor, 'proposal/other', 'snapshot/other', intent).subscribe();
    const ack = http.expectOne('/api/v1/proposals/proposal%2Fother/review/snapshots/snapshot%2Fother/visits');
    expect(ack.request.method).toBe('POST'); expect(ack.request.headers.get('X-Expected-Actor')).toBe(actor);
    expect(ack.request.body).toBe(intent); expect(ack.request.params.keys()).toEqual([]); ack.flush({}); http.verify();
  });

  it('reads caller/domain coverage with captured fingerprint and bounded page coordinates', () => {
    TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting()] });
    const api = TestBed.inject(ReviewApi), http = TestBed.inject(HttpTestingController);
    const intent = Object.freeze({ expected_fingerprint: 'a'.repeat(64), domain: 'PRODUCT' as const, offset: 20, limit: 20 });
    api.coverage(actor, 'p/other', 'h/other', intent).subscribe();
    const read = http.expectOne(req => req.url === '/api/v1/proposals/p%2Fother/review/snapshots/h%2Fother/coverage');
    expect(read.request.method).toBe('GET'); expect(read.request.headers.get('X-Expected-Actor')).toBe(actor);
    expect(read.request.params.get('expected_fingerprint')).toBe(intent.expected_fingerprint);
    expect(read.request.params.get('domain')).toBe('PRODUCT'); expect(read.request.params.get('offset')).toBe('20');
    expect(read.request.params.get('limit')).toBe('20'); read.flush({}); http.expectNone(req => req.method === 'POST'); http.verify();
  });
  it('keeps approval status reads separate from an immutable explicit item approval', () => {
    TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting()] });
    const api = TestBed.inject(ReviewApi), http = TestBed.inject(HttpTestingController);
    const scope = Object.freeze({ expected_fingerprint: 'a'.repeat(64), domain: 'PRODUCT' as const, offset: 20, limit: 20 });
    const url = '/api/v1/proposals/p%2Fother/review/snapshots/h%2Fother/unit-approvals';
    api.unitApprovals(actor, 'p/other', 'h/other', scope).subscribe(); const read = http.expectOne(req => req.url === url);
    expect(read.request.method).toBe('GET'); expect(read.request.headers.get('X-Expected-Actor')).toBe(actor);
    expect(read.request.params.get('expected_fingerprint')).toBe(scope.expected_fingerprint); expect(read.request.params.get('domain')).toBe('PRODUCT');
    expect(read.request.params.get('offset')).toBe('20'); expect(read.request.params.get('limit')).toBe('20'); read.flush({});
    http.expectNone(req => req.method === 'POST');
    const intent = Object.freeze({ request_id: other, expected_fingerprint: scope.expected_fingerprint, domain: 'PRODUCT' as const, unit_id: 'b'.repeat(64) });
    api.approveUnit(actor, 'p/other', 'h/other', intent).subscribe(); const write = http.expectOne(url);
    expect(write.request.method).toBe('POST'); expect(write.request.headers.get('X-Expected-Actor')).toBe(actor);
    expect(write.request.body).toBe(intent); expect(write.request.params.keys()).toEqual([]); write.flush({}); http.verify();
  });

});
