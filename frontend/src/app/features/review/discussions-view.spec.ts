import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { SessionState } from '../../core/auth/session-state';
import { DiscussionsView, type DiscussionScope } from './discussions-view';
import { bookDiscussionThreads, discussionComments, discussionThreads, thread, comment } from '../../../testing/discussion-fixtures';
import { actor, other, product, proposal, metadata, files } from '../../../testing/review-fixtures';

describe('Discussion UI', () => {
  const root = `/api/v1/proposals/${proposal}/threads`;
  afterEach(() => { vi.useRealTimers(); vi.restoreAllMocks(); TestBed.inject(HttpTestingController).verify({ ignoreCancelled: true }); });
  async function create(viewer = product) {
    const profile = signal<{ user_id: string } | null>({ user_id: viewer }), logoutEpoch = signal(0);
    await TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting(),
      { provide: SessionState, useValue: { profile, logoutEpoch } }],
    }).compileComponents();
    const fixture = TestBed.createComponent(DiscussionsView), view = fixture.componentInstance, http = TestBed.inject(HttpTestingController);
    const scope: DiscussionScope = { actor: viewer, proposal, snapshot: metadata(), file: files().files[0] };
    fixture.componentRef.setInput('scope', scope); fixture.detectChanges();
    const initial = http.expectOne(root); expect(initial.request.method).toBe('GET');
    expect(initial.request.headers.get('X-Expected-Actor')).toBe(viewer); initial.flush(discussionThreads(viewer)); fixture.detectChanges();
    const select = () => { view.select(view.view()!.threads[0]); http.expectOne(req => req.url === `${root}/${thread}`).flush(discussionComments(viewer)); fixture.detectChanges(); };
    return { fixture, view, http, profile, logoutEpoch, scope, select };
  }
  it('renders reader history and message text inertly with no author controls', async () => {
    const { fixture, view, http, select } = await create(actor); select(); view.beginOpen();
    expect(view.draft()).toBeNull(); expect(fixture.nativeElement.textContent).toContain('<script>bad()</script>');
    expect(fixture.nativeElement.querySelector('script, img, textarea')).toBeNull();
    view.act('resolve'); view.edit(view.comments()!.comments[0]); http.expectNone(req => req.method === 'POST');
    expect([...fixture.nativeElement.querySelectorAll('button')].every((b: any) => !!b.textContent?.trim())).toBe(true);
  });
  it('renders sealed book anchors as original source text without treating them as a diff side', async () => {
    const { fixture, view, http } = await create(actor); view.refresh(); const page = bookDiscussionThreads(actor);
    http.expectOne(root).flush(page); view.select(page.threads[0]);
    http.expectOne(req => req.url === `${root}/${thread}`).flush({ ...discussionComments(actor), thread: page.threads[0] }); fixture.detectChanges();
    expect(fixture.nativeElement.textContent).toContain('docs/unchanged.md');
    expect(fixture.nativeElement.textContent).toContain('מסמך בגרסת הסקירה המקורית');
    expect(fixture.nativeElement.textContent).toContain('<img src=x>'); expect(fixture.nativeElement.querySelector('img, script')).toBeNull();
    http.expectNone(req => req.method === 'POST');
  });
  it('keeps exact Unicode text and captures file-side selection without trusting client source metadata', async () => {
    const { view, http } = await create(); view.beginOpen(); view.mode.set('lines'); view.startLine.set(2); view.endLine.set(3);
    view.body.set('  שלום 🌍\r\n'); view.send();
    const req = http.expectOne(root); expect(req.request.method).toBe('POST');
    expect(req.request.body).toEqual({ request_id: expect.any(String), body: '  שלום 🌍\r\n', blocking: false,
      origin: { kind: 'snapshot', snapshot_id: metadata().snapshot_id, expected_fingerprint: metadata().fingerprint,
        anchor: { kind: 'file', file_id: files().files[0].id, side: 'after', selection: { kind: 'lines', start_line: 2, end_line: 3 } } } });
    req.error(new ProgressEvent('error')); expect(view.hasDraft()).toBe(true);
  });
  it('bounds scalar length, blank text, NUL and line range before posting', async () => {
    const { view, http } = await create(); view.beginOpen();
    view.body.set('🌍'.repeat(10000)); expect(view.canSend()).toBe(true);
    for (const value of ['🌍'.repeat(10001), ' \n ', 'text\0']) { view.body.set(value); view.send(); expect(view.canSend()).toBe(false); }
    view.body.set('valid'); view.mode.set('lines'); view.endLine.set(501); view.send(); expect(view.canSend()).toBe(false);
    view.endLine.set(500); expect(view.canSend()).toBe(true); http.expectNone(req => req.method === 'POST');
  });
  it('retains one frozen intent across network uncertainty and GET refresh, retrying only explicitly', async () => {
    const { view, http } = await create(); view.beginOpen(); view.body.set('original'); view.send();
    const first = http.expectOne(root), body = first.request.body;
    expect(Object.isFrozen(body)).toBe(true); expect(Object.isFrozen(body.origin)).toBe(true);
    first.error(new ProgressEvent('error')); view.body.set('different local text'); view.refresh();
    const get = http.expectOne(root); expect(get.request.method).toBe('GET'); get.flush(discussionThreads(product));
    http.expectNone(req => req.method === 'POST'); view.retry(); const retry = http.expectOne(root);
    expect(retry.request.body).toBe(body); expect(retry.request.body.body).toBe('original');
    retry.flush({}); expect(view.pending()).not.toBeNull(); expect(view.body()).toBe('different local text');
  });
  it('honors Retry-After without automatically replaying a write', async () => {
    const { view, http } = await create(); vi.useFakeTimers(); view.beginOpen(); view.body.set('waiting'); view.send();
    const first = http.expectOne(root), body = first.request.body;
    first.flush({ error: { code: 'DISCUSSION_LIMIT' } }, { status: 429, statusText: 'Limited', headers: { 'Retry-After': '2' } });
    expect(view.retryWait()).toBe(2); view.retry(); http.expectNone(root);
    vi.advanceTimersByTime(2000); expect(view.retryWait()).toBe(0); http.expectNone(root);
    view.retry(); const retry = http.expectOne(root); expect(retry.request.body).toBe(body); retry.error(new ProgressEvent('error'));
  });
  it('keeps stale draft text until the user explicitly adopts refreshed context', async () => {
    const { view, http } = await create(); view.beginOpen(); view.mode.set('proposal'); view.body.set('keep this'); view.send();
    const first = http.expectOne(root), original = first.request.body;
    first.flush({ error: { code: 'DISCUSSION_CONTEXT_CHANGED' } }, { status: 409, statusText: 'Conflict' });
    expect(view.pending()).toBeNull(); expect(view.body()).toBe('keep this'); view.refresh();
    http.expectOne(root).flush({ ...discussionThreads(product), proposal_version: 6 }); expect(view.canSend()).toBe(false);
    view.send(); http.expectNone(root); view.updateContext(); view.send(); const next = http.expectOne(root);
    expect(next.request.body.request_id).not.toBe(original.request_id);
    expect(next.request.body.origin.expected_proposal_version).toBe(6); next.error(new ProgressEvent('error'));
  });
  it('binds replies and edits to current message and thread revisions and ignores detached messages', async () => {
    const { view, http, select } = await create(); select(); const message = view.comments()!.comments[0];
    view.edit({ ...message }); expect(view.draft()).toBeNull(); view.reply({ ...message }); expect(view.draft()).toBeNull();
    view.edit(message); view.body.set('updated'); view.send(); const edit = http.expectOne(`${root}/${thread}/actions`);
    expect(edit.request.body).toEqual({ request_id: expect.any(String), expected_thread_version: 1,
      mutation: { kind: 'edit', comment_id: comment, expected_comment_version: 1, body: 'updated' } });
    edit.flush({ error: { code: 'INVALID_DISCUSSION' } }, { status: 400, statusText: 'Bad Request' });
    vi.spyOn(window, 'confirm').mockReturnValue(true); view.cancel(); view.reply(message); view.body.set('reply'); view.send();
    const reply = http.expectOne(`${root}/${thread}/actions`);
    expect(reply.request.body.mutation).toEqual({ kind: 'reply', reply_to_id: comment, body: 'reply' }); reply.error(new ProgressEvent('error'));
  });
  it('requires explicit deletion confirmation and keeps tombstoning separate from resolution', async () => {
    const { view, http, select } = await create(); select(); const message = view.comments()!.comments[0];
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(false); view.act('tombstone', message); http.expectNone(req => req.method === 'POST');
    confirm.mockReturnValue(true); view.act('tombstone', message); const req = http.expectOne(`${root}/${thread}/actions`);
    expect(req.request.body.mutation).toEqual({ kind: 'tombstone', comment_id: comment, expected_comment_version: 1 });
    req.error(new ProgressEvent('error'));
  });
  it('uses current resolution and blocking capabilities and preserves an uncertain denied action', async () => {
    const { view, http, select } = await create(); select(); view.act('reopen'); http.expectNone(req => req.method === 'POST');
    view.act('resolve'); const req = http.expectOne(`${root}/${thread}/actions`), body = req.request.body;
    expect(body.expected_thread_version).toBe(1); expect(body.mutation).toEqual({ kind: 'resolve' });
    req.flush({ error: { code: 'INDEPENDENT_REVIEWER_REQUIRED' } }, { status: 403, statusText: 'Forbidden' });
    expect(view.pending()?.intent.body).toBe(body); expect(view.comments()).not.toBeNull();
    view.refresh(); const latest = discussionThreads(product); latest.can_comment = false; latest.can_open_blocking_thread = false;
    latest.threads[0].permissions = { can_comment: false, can_resolve: false, can_reopen: false, can_toggle_blocking: false };
    http.expectOne(root).flush(latest); http.expectOne(`${root}/${thread}`).flush(discussionComments(actor));
    view.retry(); http.expectNone(req => req.method === 'POST'); expect(view.pending()).not.toBeNull();
  });
  it('prevents a new blocker during the merge barrier while permitting an ordinary discussion', async () => {
    const { view, http } = await create(); view.beginOpen(); view.body.set('discussion'); view.blocking.set(true); view.refresh();
    http.expectOne(root).flush({ ...discussionThreads(product), can_open_blocking_thread: false });
    expect(view.canSend()).toBe(false); view.blocking.set(false); expect(view.canSend()).toBe(true);
  });
  it('requires confirmation to abandon uncertain requests and never resubmits on cancellation', async () => {
    const { view, http } = await create(); view.beginOpen(); view.body.set('uncertain'); view.send();
    http.expectOne(root).error(new ProgressEvent('error')); const confirm = vi.spyOn(window, 'confirm').mockReturnValue(false);
    view.cancel(); expect(view.pending()).not.toBeNull(); expect(view.body()).toBe('uncertain');
    confirm.mockReturnValue(true); view.cancel(); expect(view.pending()).toBeNull(); expect(view.body()).toBe('');
    http.expectNone(req => req.method === 'POST');
  });
  it('clears a verified write and reloads history through GET only', async () => {
    const { view, http } = await create(); view.beginOpen(); view.body.set('saved'); view.send(); const req = http.expectOne(root);
    req.flush({ id: other, request_id: req.request.body.request_id, actor_id: product, proposal_id: proposal, thread_id: thread,
      thread_version: 1, comment_id: comment, comment_version: 1, recorded_at: '2026-11-19T00:00:00Z', replayed: false });
    expect(view.hasDraft()).toBe(false); expect(view.body()).toBe('');
    http.expectOne(root).flush(discussionThreads(product)); http.expectOne(`${root}/${thread}`).flush(discussionComments(product));
    expect(view.comments()?.thread.id).toBe(thread); http.expectNone(req => req.method === 'POST');
  });
  it('cancels pending writes and purges text on account, epoch and selected-context changes', async () => {
    const { view, http, fixture, scope, logoutEpoch, profile } = await create();
    view.beginOpen(); view.body.set('private'); view.send(); const req = http.expectOne(root);
    logoutEpoch.update(v => v + 1); fixture.detectChanges(); expect(req.cancelled).toBe(true);
    expect(view.body()).toBe(''); expect(view.pending()).toBeNull(); http.expectOne(root).flush(discussionThreads(product));
    view.beginOpen(); view.body.set('old snapshot'); fixture.componentRef.setInput('scope', { ...scope, snapshot: null }); fixture.detectChanges();
    expect(view.body()).toBe(''); http.expectOne(root).flush(discussionThreads(product));
    profile.set({ user_id: other }); expect(view.view()).toBeNull(); fixture.detectChanges(); expect(view.hasDraft()).toBe(false);
    http.expectNone(req => req.method === 'POST');
  });
  it('rejects foreign details and propagates scoped access loss while erasing drafts', async () => {
    const { view, http, fixture } = await create(); const denied = vi.fn(); view.accessDenied.subscribe(denied);
    view.select(view.view()!.threads[0]); http.expectOne(req => req.url === `${root}/${thread}`).flush(discussionComments(actor));
    expect(view.comments()).toBeNull(); view.beginOpen(); view.body.set('private'); view.refresh();
    http.expectOne(root).flush({ error: { code: 'NOT_FOUND' } }, { status: 404, statusText: 'Not Found' }); fixture.detectChanges();
    expect(denied).toHaveBeenCalledOnce(); expect(view.view()).toBeNull(); expect(view.body()).toBe('');
  });
});
