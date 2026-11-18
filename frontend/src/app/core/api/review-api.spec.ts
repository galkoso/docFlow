import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { actor, proposal, snapshot, other, configuration, detail } from '../../../testing/review-fixtures';
import { ReviewApi } from './review-api';
import { submissionContext } from '../../features/review/submission-evidence';

describe('Review API request scope', () => {
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
    api.files(actor, proposal, snapshot, 50).subscribe(); api.media(actor, proposal, snapshot, 100).subscribe();
    const pages = http.match(req => req.url.endsWith('/files') || req.url.endsWith('/media'));
    expect(pages.map(request => request.request.params.get('offset'))).toEqual(['50', '100']);
    for (const request of pages) { expect(request.request.headers.get('X-Expected-Actor')).toBe(actor); expect(request.request.params.get('limit')).toBe('50'); request.flush({}); }
    http.verify();
  });
});
