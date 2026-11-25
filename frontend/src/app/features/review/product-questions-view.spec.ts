import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { SessionState } from '../../core/auth/session-state';
import { actor, product, proposal, other } from '../../../testing/review-fixtures';
import { question, questionPage, questionHistory, questionReceipt, questionId } from '../../../testing/product-question-fixtures';
import { ProductQuestionsView } from './product-questions-view';
import type { ProductQuestionChange } from '../../core/api/product-question-api';

describe('Product question controls', () => {
  const root = `/api/v1/proposals/${proposal}/questions`, detail = `${root}/${questionId}`;
  afterEach(() => { TestBed.inject(HttpTestingController).verify({ ignoreCancelled: true }); vi.useRealTimers(); vi.restoreAllMocks(); });
  async function create() {
    const profile = signal<{ user_id: string } | null>({ user_id: product }), logoutEpoch = signal(0);
    await TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting(), { provide: SessionState, useValue: { profile, logoutEpoch } }] }).compileComponents();
    const fixture = TestBed.createComponent(ProductQuestionsView), view = fixture.componentInstance, http = TestBed.inject(HttpTestingController);
    fixture.componentRef.setInput('scope', { actor: product, proposal }); fixture.detectChanges();
    http.expectNone(root); view.open(); http.expectOne(root).flush(questionPage()); fixture.detectChanges();
    const select = () => { view.select(view.view()!.questions[0]); http.expectOne(detail).flush(questionHistory()); fixture.detectChanges(); };
    return { fixture, view, http, profile, logoutEpoch, select };
  }
  function decided(body: ProductQuestionChange) {
    const q = question(), first = structuredClone(q.current), receipt = questionReceipt(body);
    q.current = { ...q.current, id: receipt.id, version: 2, kind: 'DECIDE', state: 'DECIDED', actor: { id: product, name: 'Product' },
      answer: body.action.kind === 'decide' ? body.action.answer : 'Answer', recorded_at: receipt.recorded_at,
      decision: { actor: { id: product, name: 'Product' }, recorded_at: receipt.recorded_at } };
    q.permissions.can_propose = false;
    return { ...questionHistory(product, q), revisions: [first, structuredClone(q.current)] };
  }
  it('renders imported suggestions and history inertly and keeps Product decisions explicit', async () => {
    const { fixture, view, http, select } = await create(); select();
    expect(fixture.nativeElement.textContent).toContain('תשובה מיובאת היא הצעה בלבד');
    expect(fixture.nativeElement.textContent).toContain('<script>unsafe()</script>'); expect(fixture.nativeElement.querySelector('script, img')).toBeNull();
    view.begin('decide'); fixture.detectChanges(); await fixture.whenStable();
    expect(view.answer()).toBe(question().current.answer); expect(view.canSend()).toBe(true); http.expectNone(r => r.method === 'POST');
    view.send(); const post = http.expectOne(`${detail}/actions`), body = post.request.body;
    expect(body.action).toEqual({ kind: 'decide', answer: question().current.answer }); expect(body.expected_version).toBe(1);
    expect(Object.isFrozen(body.action)).toBe(true); expect(post.request.headers.get('X-Expected-Actor')).toBe(product);
    post.flush(questionReceipt(body)); http.expectOne(root).flush(questionPage(product, [decided(body).question])); http.expectOne(detail).flush(decided(body));
    fixture.detectChanges(); expect(view.pending()).toBeNull(); expect(view.view()?.unresolved_blockers).toBe(0);
    expect(fixture.nativeElement.textContent).toContain('הוכרעה במפורש במוצר'); expect(view.hasDraft()).toBe(false);
  });
  it('never enables a write from reader permission hints', async () => {
    const { view, http, fixture } = await create(); view.refresh();
    const page = questionPage(); page.can_create = false; page.questions[0].permissions = { can_propose: false, can_decide: false, can_reopen: false, can_toggle_blocking: false };
    http.expectOne(root).flush(page); view.select(view.view()!.questions[0]);
    http.expectOne(detail).flush(questionHistory(product, page.questions[0])); fixture.detectChanges();
    view.beginCreate(); view.begin('decide'); view.begin('reopen'); view.begin('set_blocking'); expect(view.draft()).toBeNull();
    expect([...fixture.nativeElement.querySelectorAll('button')].filter((b: unknown) => (b as HTMLButtonElement).textContent?.includes('הכרעת מוצר')).every((b: unknown) => (b as HTMLButtonElement).disabled)).toBe(true);
    http.expectNone(r => r.method === 'POST');
  });
  it('captures manual create data without source, actor or decision claims and counts Unicode scalars', async () => {
    const { view, http, fixture } = await create(); view.beginCreate(); view.question.set('🦀'.repeat(10000)); view.answer.set('Optional proposed answer\r\n');
    expect(view.questionLength()).toBe(10000); expect(view.canSend()).toBe(true); view.question.update(v => `${v}a`); expect(view.canSend()).toBe(false);
    view.question.set('Manual question'); view.send(); const post = http.expectOne(root), body = post.request.body;
    expect(body.expected_proposal_version).toBe(2); expect(body.proposed_answer).toBe('Optional proposed answer\r\n');
    expect(body).not.toHaveProperty('source'); expect(body).not.toHaveProperty('decided_by'); expect(body).not.toHaveProperty('actor_id');
    post.error(new ProgressEvent('error')); fixture.detectChanges(); await fixture.whenStable(); fixture.detectChanges(); expect(view.pending()).not.toBeNull();
    expect((fixture.nativeElement.querySelector('textarea[name="questionText"]') as HTMLTextAreaElement).disabled).toBe(true);
  });
  it('submits through the labelled native form only after valid input', async () => {
    const { fixture, view, http } = await create(); view.beginCreate(); fixture.detectChanges(); await fixture.whenStable();
    const field = fixture.nativeElement.querySelector('textarea[name="questionText"]') as HTMLTextAreaElement;
    field.value = 'שאלה שנכתבה בטופס'; field.dispatchEvent(new Event('input')); fixture.detectChanges(); await fixture.whenStable(); fixture.detectChanges();
    const submit = fixture.nativeElement.querySelector('button[type="submit"]') as HTMLButtonElement;
    expect(submit.disabled).toBe(false); expect(field.labels?.[0].textContent).toContain('השאלה'); submit.click();
    const post = http.expectOne(root); expect(post.request.body.text).toBe(field.value); expect(post.request.body.proposed_answer).toBeNull();
    post.error(new ProgressEvent('error'));
  });
  it('retains the exact nonce/body through uncertainty, list paging and a refreshed current decision', async () => {
    const { view, http, fixture, select } = await create(); select(); view.begin('decide'); view.answer.set('Captured answer'); view.send();
    const post = http.expectOne(`${detail}/actions`), body = post.request.body; post.error(new ProgressEvent('error'));
    view.answer.set('Programmatic later value'); view.refresh(); http.expectOne(root).flush(questionPage(product, [decided(body).question])); http.expectOne(detail).flush(decided(body));
    view.page(other); http.expectOne(req => req.url === root && req.params.get('after') === other).flush(questionPage(product, []));
    fixture.componentRef.setInput('scope', { actor: product, proposal }); fixture.detectChanges();
    expect(view.pending()!.intent.body).toBe(body); expect(view.hasDraft()).toBe(true); http.expectNone(r => r.method === 'POST');
    view.retry(); const retry = http.expectOne(`${detail}/actions`); expect(retry.request.body).toBe(body); expect(retry.request.body.action.answer).toBe('Captured answer');
    retry.error(new ProgressEvent('error'));
  });
  it('keeps unknown or malformed success receipts pending and emits no local approval', async () => {
    const { view, http, select } = await create(); select(); view.begin('decide'); const saved = vi.fn(); view.recorded.subscribe(saved); view.send();
    const post = http.expectOne(`${detail}/actions`), body = post.request.body; post.flush({ ...questionReceipt(body), actor_id: actor });
    expect(view.pending()).not.toBeNull(); expect(saved).not.toHaveBeenCalled(); expect(view.history()?.question.current.state).toBe('PROPOSED');
    http.expectNone(r => r.method === 'GET');
  });
  it('requires explicit fresh-version adoption after a definitive version conflict and preserves text', async () => {
    const { view, http, select } = await create(); select(); view.begin('decide'); view.answer.set('Draft retained'); view.send();
    const post = http.expectOne(`${detail}/actions`), body = post.request.body;
    post.flush({ error: { code: 'QUESTION_VERSION_MISMATCH' } }, { status: 409, statusText: 'Conflict' });
    expect(view.pending()).toBeNull(); expect(view.answer()).toBe('Draft retained'); expect(view.canSend()).toBe(false);
    view.refresh(); http.expectOne(root).flush(questionPage()); http.expectOne(detail).flush(decided(body));
    expect(view.canSend()).toBe(false); view.updateContext(); expect(view.canSend()).toBe(true); view.send();
    const fresh = http.expectOne(`${detail}/actions`); expect(fresh.request.body.expected_version).toBe(2); expect(fresh.request.body.request_id).not.toBe(body.request_id);
    expect(fresh.request.body.action.answer).toBe('Draft retained'); fresh.error(new ProgressEvent('error'));
  });
  it('honors retry guidance without a timer-generated POST and retains uncertain requests on Product authority loss', async () => {
    const { view, http, select } = await create(); select(); vi.useFakeTimers(); view.begin('decide'); view.send();
    const post = http.expectOne(`${detail}/actions`), body = post.request.body;
    post.flush({}, { status: 429, statusText: 'Quota', headers: { 'Retry-After': '2' } }); view.retry(); http.expectNone(r => r.method === 'POST');
    vi.advanceTimersByTime(2000); http.expectNone(r => r.method === 'POST'); view.retry();
    const retry = http.expectOne(`${detail}/actions`); expect(retry.request.body).toBe(body);
    retry.flush({ error: { code: 'QUESTION_PRODUCT_REQUIRED' } }, { status: 403, statusText: 'Forbidden' });
    expect(view.pending()?.intent.body).toBe(body); expect(view.hasDraft()).toBe(true);
  });
  it.each([401, 403, 404])('purges private questions and drafts on scoped read denial %s', async status => {
    const { view, http, fixture, select } = await create(); select(); view.begin('decide'); view.answer.set('Private answer'); const denied = vi.fn(); view.accessDenied.subscribe(denied);
    view.refresh(); http.expectOne(root).flush({}, { status, statusText: 'Denied' }); fixture.detectChanges();
    expect(view.question()).toBe(''); expect(view.answer()).toBe(''); expect(view.history()).toBeNull(); expect(view.draft()).toBeNull(); expect(denied).toHaveBeenCalledOnce();
    expect(fixture.nativeElement.textContent).not.toContain('Private answer');
  });
  it('blocks a queued retry synchronously on account change and then erases private intent', async () => {
    const { view, http, profile, fixture, select } = await create(); select(); view.begin('decide'); view.send();
    http.expectOne(`${detail}/actions`).error(new ProgressEvent('error')); profile.set({ user_id: other });
    expect(view.history()).toBeNull(); expect(view.pending()).toBeNull(); view.retry(); http.expectNone(r => r.method === 'POST');
    fixture.detectChanges(); expect(view.answer()).toBe(''); expect(view.draft()).toBeNull();
  });
  it('rejects mismatched history and preserves a draft across failed refreshes without stale eligibility', async () => {
    const { view, http, select } = await create(); select(); view.begin('decide'); view.answer.set('Retained'); view.refresh();
    http.expectOne(root).flush(questionPage()); http.expectOne(detail).flush({ ...questionHistory(), actor_id: other });
    expect(view.history()).toBeNull(); expect(view.canSend()).toBe(false); expect(view.answer()).toBe('Retained');
    view.refresh(); http.expectOne(root).flush({}, { status: 503, statusText: 'Unavailable' }); http.expectOne(detail).flush({}, { status: 503, statusText: 'Unavailable' });
    expect(view.draft()).not.toBeNull(); expect(view.answer()).toBe('Retained'); expect(view.history()).toBeNull();
  });
});
