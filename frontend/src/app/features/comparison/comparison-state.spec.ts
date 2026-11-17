import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting, TestRequest } from '@angular/common/http/testing';
import type { DiffResponse } from '../../core/api/comparison-api';
import { ComparisonState } from './comparison-state';

import { actor, proposal, snapshot, head, base, target, id, file, ready, pending, failed } from './comparison.testing';

describe('Comparison snapshot lifecycle', () => {
  let state: ComparisonState, http: HttpTestingController;
  beforeEach(() => {
    vi.useFakeTimers();
    TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting(), ComparisonState] });
    state = TestBed.inject(ComparisonState); http = TestBed.inject(HttpTestingController);
    expect(state.canRetry()).toBe(false); expect(state.canCheck()).toBe(false);
  });
  afterEach(() => { state.clear(); http.verify(); vi.useRealTimers(); });
  function request(): TestRequest { return http.expectOne(req => req.url.endsWith('/diff')); }
  function start(): TestRequest { expect(state.start(actor, proposal, { request_id: id(6) })).toBe(true); return request(); }
  function loaded(total = 1): void { start().flush(ready(total)); }

  it('prepares once and polls only the accepted immutable snapshot through real progress phases', () => {
    const first = start();
    expect(first.request.headers.get('X-Expected-Actor')).toBe(actor);
    expect(first.request.params.get('request_id')).toBe(id(6));
    first.flush(pending());
    expect(state.snapshot()).toBe(snapshot); expect(state.status()).toBe('queued');
    expect(state.openSnapshot(actor, proposal, snapshot)).toBe(false); // Our own URL promotion.
    http.expectNone(req => req.url.endsWith('/diff'));
    for (const phase of ['LOADING', 'PUBLISHING', 'RETRY_WAIT']) {
      vi.advanceTimersByTime(2000);
      const poll = request();
      expect(poll.request.params.keys().sort()).toEqual(['limit', 'offset', 'snapshot']);
      expect(poll.request.params.get('snapshot')).toBe(snapshot);
      expect(poll.request.headers.get('X-Expected-Actor')).toBe(actor);
      poll.flush(pending(phase));
      expect(state.status()).toBe(phase.toLowerCase());
    }
    vi.advanceTimersByTime(2000); request().flush(ready());
    expect(state.status()).toBe('ready'); expect(state.noChanges()).toBe(false);
    vi.advanceTimersByTime(60000); http.expectNone(req => req.url.endsWith('/diff'));
  });

  it('freezes the original request identity and selection across unknown acceptance and manual replay', () => {
    const input = { head, target, request_id: id(6) };
    expect(state.start(actor, proposal, input)).toBe(true);
    const first = request();
    input.head = '9'.repeat(40); input.target = '8'.repeat(40); input.request_id = id(99);
    first.error(new ProgressEvent('error'));
    expect(state.uncertain()).toBe(true); expect(state.snapshot()).toBeNull();
    vi.advanceTimersByTime(60000); http.expectNone(req => req.url.endsWith('/diff'));
    state.retry();
    const retry = request();
    expect(retry.request.params.toString()).toBe(first.request.params.toString());
    retry.flush(ready());
    expect(state.data()?.summary?.head_sha).toBe(head); expect(state.uncertain()).toBe(false);
  });

  it('preserves an unaccepted intent while authentication is unknown and never resubmits on same-actor refresh', () => {
    const first = start(); state.identity(null);
    expect(first.cancelled).toBe(true); expect(state.status()).toBe('suspended'); expect(state.uncertain()).toBe(true);
    expect(state.start(actor, proposal)).toBe(false);
    state.identity(actor);
    http.expectNone(req => req.url.endsWith('/diff'));
    expect(state.requestId()).toBe(id(6)); expect(state.canRetry()).toBe(true);
    state.retry(); const retry = request();
    expect(retry.request.params.get('request_id')).toBe(id(6)); retry.flush(ready());
  });

  it('observes accepted work after errors and resumes the same snapshot after a same-actor refresh', () => {
    start().flush(pending()); vi.advanceTimersByTime(2000);
    request().flush({ error: { code: 'COMPARISON_UNAVAILABLE' } }, { status: 503, statusText: 'Unavailable' });
    expect(state.status()).toBe('error');
    vi.advanceTimersByTime(60000); http.expectNone(req => req.url.endsWith('/diff'));
    state.retry(); const retry = request(); expect(retry.request.params.has('request_id')).toBe(false); retry.flush(ready());
    expect(state.select(file().id)).toBe(true);
    expect(Object.isFrozen(state.selected()?.after)).toBe(true);
    expect(state.canCheck()).toBe(true);
    state.identity(null);
    expect(state.canCheck()).toBe(false);
    expect(state.data()).toBeNull(); expect(state.selected()).toBeNull(); expect(state.snapshot()).toBe(snapshot);
    state.identity(actor); const resumed = request();
    expect(resumed.request.params.get('snapshot')).toBe(snapshot);
    const historical = ready(); historical.current_head_sha = '4'.repeat(40); historical.current_proposal_version = 8;
    historical.current_proposal_state = 'CLOSED'; historical.context_changed = true; historical.historical_head = true;
    resumed.flush(historical);
    expect(state.data()?.summary?.head_sha).toBe(head); expect(state.data()?.historical_head).toBe(true);
    expect(state.selected()).toBeNull();
  });

  it('purges private data and cancels pending work on identity change or completed logout', () => {
    loaded(); state.select(file().id); state.check(); const old = request();
    state.identity(id(8));
    expect(old.cancelled).toBe(true); expect(state.data()).toBeNull(); expect(state.selected()).toBeNull();
    expect(state.snapshot()).toBeNull(); expect(state.requestId()).toBeNull(); expect(state.owner()).toBeNull();
    expect(state.identityChanged()).toBe(true);
    vi.advanceTimersByTime(60000); http.expectNone(req => req.url.endsWith('/diff'));
    expect(state.openSnapshot(id(8), proposal, snapshot)).toBe(true); request().flush(ready());
    state.identity(null, true);
    expect(state.data()).toBeNull(); expect(state.snapshot()).toBeNull(); expect(state.owner()).toBeNull();
    state.identity(id(8)); http.expectNone(req => req.url.endsWith('/diff'));
  });

  it('also purges on a cross-actor start attempt without silently beginning new work', () => {
    loaded();
    expect(state.start(id(8), proposal)).toBe(false);
    expect(state.data()).toBeNull(); expect(state.identityChanged()).toBe(true);
    http.expectNone(req => req.url.endsWith('/diff'));
  });

  it('cancels stale navigation requests and keeps file pages and selected evidence pinned', () => {
    const old = start();
    expect(state.start(actor, id(8), { request_id: id(9) })).toBe(true); expect(old.cancelled).toBe(true);
    const next = ready(); next.proposal_id = id(8); next.summary!.proposal_id = id(8);
    request().flush(next); expect(state.data()?.proposal_id).toBe(id(8));
    state.clear(); loaded(120); state.select(file().id);
    state.files(50); const fifty = request();
    state.files(100); expect(fifty.cancelled).toBe(true);
    const hundred = request(); expect(hundred.request.params.get('snapshot')).toBe(snapshot);
    expect(hundred.request.params.get('offset')).toBe('100'); hundred.flush(ready(120, 100));
    expect(state.data()?.page?.files).toHaveLength(20);
    expect(state.selected()?.id).toBe(file().id); // A different inventory page cannot retarget the selected file.
    expect(state.select(file(100).id)).toBe(true); expect(state.select('unknown')).toBe(false);
  });

  it('rejects malformed or scope-mismatched preparation receipts without adopting their snapshot', () => {
    const corrupt: Array<(value: DiffResponse) => void> = [
      v => v.proposal_id = id(8), v => v.summary!.head_sha = 'bad', v => v.page!.snapshot_id = id(8),
      v => v.page!.content_id = '9'.repeat(64), v => v.page!.next_offset = 0, v => v.page!.offset = 50,
      v => v.page!.files[0].before!.path = 'docs/../secret.md', v => v.page!.files[0].content_changed = false,
      v => v.context_changed = true, v => v.summary!.target_outdated = true,
      v => { v.summary!.before_file_count = 0; v.summary!.after_file_count = 0; },
      v => { v.summary!.changes_total = 2; v.page!.total = 2; v.page!.files.push(structuredClone(v.page!.files[0])); },
    ];
    for (const mutate of corrupt) {
      state.clear(); const first = start(); const value = ready(); mutate(value); first.flush(value);
      expect(state.status()).toBe('error'); expect(state.errorCode()).toBe('COMPARISON_RESPONSE_MISMATCH');
      expect(state.snapshot()).toBeNull(); expect(state.data()).toBeNull();
      state.retry(); const retry = request(); expect(retry.request.params.get('request_id')).toBe(id(6)); retry.flush(ready());
    }
  });

  it('never mixes later pages with a changed summary, job or selected file descriptor', () => {
    const corrupt: Array<(value: DiffResponse) => void> = [
      v => v.summary!.repository_id = id(8), v => v.job_id = id(8),
      v => { v.page!.files[0].before!.path = 'docs/changed.md'; v.page!.files[0].after!.path = 'docs/changed.md'; },
    ];
    for (const mutate of corrupt) {
      state.clear(); loaded(); state.select(file().id); state.check();
      const value = ready(); mutate(value); request().flush(value);
      expect(state.errorCode()).toBe('COMPARISON_RESPONSE_MISMATCH');
      expect(state.data()).toBeNull(); expect(state.selected()).toBeNull(); expect(state.snapshot()).toBe(snapshot);
    }
  });

  it('keeps terminal failures explicit and cannot turn a terminal snapshot back into pending work', () => {
    start().flush(failed());
    expect(state.status()).toBe('failed'); expect(state.noChanges()).toBe(false);
    vi.advanceTimersByTime(60000); http.expectNone(req => req.url.endsWith('/diff'));
    state.check(); request().flush(pending());
    expect(state.errorCode()).toBe('COMPARISON_RESPONSE_MISMATCH');
    state.clear(); loaded(0);
    expect(state.noChanges()).toBe(true); expect(state.status()).toBe('ready');
    expect(state.select(file().id)).toBe(false);
  });

  it('clears visible data on revoked access and requires identity confirmation after authentication failure', () => {
    for (const status of [401, 403, 404, 409]) {
      state.clear(); loaded(); state.select(file().id); state.check();
      const code = status === 409 ? 'SESSION_IDENTITY_CHANGED' : status === 401 ? 'AUTH_REQUIRED' : 'NOT_FOUND';
      request().flush({ error: { code, message_he: 'יש לבדוק הרשאה' } }, { status, statusText: 'Denied' });
      expect(state.data()).toBeNull(); expect(state.selected()).toBeNull();
      if ([401, 409].includes(status)) {
        expect(state.authRequired()).toBe(true); expect(state.canRetry()).toBe(false);
        state.identity(actor); const poll = request(); expect(poll.request.params.get('snapshot')).toBe(snapshot); poll.flush(ready());
      }
    }
  });

  it('bounds automatic polling and leaves slow work available for explicit observation', () => {
    start().flush(pending());
    for (let n = 1; n < 450; n++) { vi.advanceTimersByTime(2000); request().flush(pending()); }
    expect(state.status()).toBe('waiting'); expect(state.canCheck()).toBe(true);
    vi.advanceTimersByTime(60000); http.expectNone(req => req.url.endsWith('/diff'));
    state.check(); request().flush(ready()); expect(state.status()).toBe('ready');
  });

  it('times out without replacing the preparation identity and cancels all work on destruction', () => {
    const first = start(); vi.advanceTimersByTime(20000);
    expect(first.cancelled).toBe(true); expect(state.uncertain()).toBe(true); expect(state.status()).toBe('error');
    state.retry(); const second = request(); expect(second.request.params.get('request_id')).toBe(id(6)); second.flush(pending());
    state.ngOnDestroy(); vi.advanceTimersByTime(60000); http.expectNone(req => req.url.endsWith('/diff'));
    expect(state.owner()).toBeNull(); expect(state.snapshot()).toBeNull();
  });

  it('rejects invalid navigation inputs and prevents reusing a request identity for a changed selection', () => {
    loaded(); expect(state.openSnapshot(actor, proposal, 'bad')).toBe(false);
    expect(state.data()).toBeNull(); expect(state.canRetry()).toBe(false);
    const first = start();
    expect(state.start(actor, proposal, { request_id: id(6), head: base })).toBe(false);
    expect(first.cancelled).toBe(true); expect(state.errorCode()).toBe('COMPARISON_RESPONSE_MISMATCH');
    state.retry(); const replay = request(); expect(replay.request.params.has('head')).toBe(false); replay.flush(ready());
  });

  it('keeps repository-context conflicts separate from login failures and checks only the existing snapshot', () => {
    loaded(); state.select(file().id); state.check();
    request().flush({ error: { code: 'COMPARISON_CONTEXT_CHANGED' } }, { status: 409, statusText: 'Conflict' });
    expect(state.data()).toBeNull(); expect(state.selected()).toBeNull(); expect(state.authRequired()).toBe(false);
    expect(state.canRetry()).toBe(true); expect(state.snapshot()).toBe(snapshot);
    state.retry(); const retry = request();
    expect(retry.request.params.get('snapshot')).toBe(snapshot); expect(retry.request.params.has('request_id')).toBe(false);
    retry.flush(ready());
  });


  it('honors explicit request-key and page navigation while cancelling obsolete requests', () => {
    const old = start();
    expect(state.start(actor, proposal, { request_id: id(9) })).toBe(true);
    expect(old.cancelled).toBe(true);
    const replacement = request(); expect(replacement.request.params.get('request_id')).toBe(id(9)); replacement.flush(ready(120));
    state.check(); request().flush({ error: { code: 'NOT_FOUND' } }, { status: 404, statusText: 'Denied' });
    expect(state.data()).toBeNull();
    state.openSnapshot(actor, proposal, snapshot, 50);
    const page = request(); expect(page.request.params.get('offset')).toBe('50');
    expect(page.request.params.get('snapshot')).toBe(snapshot); page.flush(ready(120, 50));
    expect(state.data()?.page?.offset).toBe(50);
  });

  it('locates a selected file on a later page without preparing or trusting an old list position', () => {
    state.openFile(actor, proposal, snapshot, file(75).id); const located = request();
    expect(located.request.params.keys().sort()).toEqual(['file', 'limit', 'snapshot']);
    expect(located.request.params.get('file')).toBe(file(75).id); located.flush(ready(120, 50));
    expect(state.selected()?.id).toBe(file(75).id); expect(state.data()?.page?.offset).toBe(50);
    state.openFile(actor, proposal, snapshot, file(75).id); state.openFile(actor, proposal, snapshot, file(76).id);
    expect(state.selected()?.id).toBe(file(76).id); http.expectNone(req => req.url.endsWith('/diff'));
    state.files(100); const obsolete = request(); state.openFile(actor, proposal, snapshot, file(75).id);
    expect(obsolete.cancelled).toBe(true); expect(state.selected()?.id).toBe(file(75).id);
    state.openSnapshot(actor, proposal, snapshot); const list = request();
    expect(list.request.params.has('file')).toBe(false); expect(list.request.params.get('offset')).toBe('0'); list.flush(ready(120));
  });
  it('pins a file locator through failures and same-actor refresh, and rejects a page without that identity', () => {
    state.openFile(actor, proposal, snapshot, file(75).id); const first = request(); first.error(new ProgressEvent('error'));
    state.retry(); const retry = request(); expect(retry.request.params.toString()).toBe(first.request.params.toString());
    retry.flush(ready(120)); expect(state.data()).toBeNull(); expect(state.errorCode()).toBe('COMPARISON_RESPONSE_MISMATCH');
    state.retry(); request().flush(ready(120, 50)); state.identity(null); expect(state.selected()).toBeNull();
    state.identity(actor); const restored = request(); expect(restored.request.params.get('file')).toBe(file(75).id);
    restored.flush(ready(120, 50)); expect(state.selected()?.id).toBe(file(75).id); expect(state.requestId()).toBeNull();
  });
  it('cancels obsolete file locations and never silently prepares after an unknown immutable ID', () => {
    state.openFile(actor, proposal, snapshot, file(75).id); const old = request();
    state.openFile(actor, proposal, snapshot, file(99).id); expect(old.cancelled).toBe(true);
    request().flush({ error: { code: 'NOT_FOUND' } }, { status: 404, statusText: 'Not found' });
    expect(state.selected()).toBeNull(); expect(state.snapshot()).toBe(snapshot);
    vi.advanceTimersByTime(60000); http.expectNone(req => req.url.endsWith('/diff'));
    state.openSnapshot(actor, proposal, snapshot); request().flush(ready()); expect(state.data()?.state).toBe('READY');
  });

});
