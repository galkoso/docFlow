import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { actor, other } from '../../../testing/review-fixtures';
import { ProductQuestionApi } from './product-question-api';

describe('Product question API scope', () => {
  it('encodes resource identities, carries the captured actor and preserves frozen create/change intents', () => {
    TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting()] });
    const api = TestBed.inject(ProductQuestionApi), http = TestBed.inject(HttpTestingController);
    const root = '/api/v1/proposals/p%2Fother%3Fx%3D1/questions';
    api.list(actor, 'p/other?x=1', other).subscribe();
    const page = http.expectOne(req => req.url === root);
    expect(page.request.method).toBe('GET'); expect(page.request.headers.get('X-Expected-Actor')).toBe(actor);
    expect(page.request.params.get('after')).toBe(other); page.flush({});
    api.history(actor, 'p/other?x=1', 'q/other', 20).subscribe();
    const history = http.expectOne(req => req.url === `${root}/q%2Fother`);
    expect(history.request.params.get('after')).toBe('20'); expect(history.request.headers.get('X-Expected-Actor')).toBe(actor); history.flush({});
    const create = Object.freeze({ request_id: other, expected_proposal_version: 2, text: 'שאלה\r\n🦀', blocking: true, proposed_answer: null });
    api.create(actor, 'p/other?x=1', create).subscribe();
    const open = http.expectOne(root); expect(open.request.method).toBe('POST'); expect(open.request.body).toBe(create);
    expect(open.request.headers.get('X-Expected-Actor')).toBe(actor); expect(open.request.params.keys()).toEqual([]); open.flush({});
    const change = Object.freeze({ request_id: other, expected_version: 1, action: Object.freeze({ kind: 'decide' as const, answer: 'Explicit Product answer' }) });
    api.change(actor, 'p/other?x=1', 'q/other', change).subscribe();
    const decide = http.expectOne(`${root}/q%2Fother/actions`);
    expect(decide.request.method).toBe('POST'); expect(decide.request.body).toBe(change);
    expect(decide.request.headers.get('X-Expected-Actor')).toBe(actor); expect(decide.request.params.keys()).toEqual([]); decide.flush({}); http.verify();
  });
});
