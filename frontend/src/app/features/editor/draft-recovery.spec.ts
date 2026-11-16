import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting, TestRequest } from '@angular/common/http/testing';
import { DraftBuffer, DraftSaved, DraftSnapshot } from '../../core/api/editor-api';
import { DraftRecovery, RecoveryScope } from './draft-recovery';

const scope: RecoveryScope = { actor: 'actor-a', proposal: 'proposal-a', document: 'DOC-ONE', base: 'a'.repeat(40) };
const context: DraftSnapshot['context'] = { proposal_id: scope.proposal, proposal_version: 1, proposal_state: 'DRAFT',
  current_head_sha: scope.base, can_edit: true, can_store_recovery: true, retention_days: 30 };
function buffer(content = 'server recovery', version = 7): DraftBuffer {
  return { document_id: scope.document, base_head_sha: scope.base, version, updated_at: '2026-11-16T00:00:00Z',
    expires_at: '2026-12-16T00:00:00Z', content_utf8: content, base_is_current: true };
}

describe('Private draft recovery autosave', () => {
  let recovery: DraftRecovery;
  let http: HttpTestingController;
  beforeEach(() => {
    vi.useFakeTimers();
    TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting(), DraftRecovery] });
    recovery = TestBed.inject(DraftRecovery); http = TestBed.inject(HttpTestingController);
  });
  afterEach(() => { recovery.clear(); http.verify(); vi.useRealTimers(); });
  function read() { return http.expectOne(request => request.method === 'GET' && request.url.endsWith('/draft')); }
  function write() { return http.expectOne(request => request.method === 'PUT' && request.url.endsWith('/draft')); }
  function open(buffers: DraftBuffer[] = [], override = {}) {
    recovery.open(scope, 'original Git source');
    const request = read(); expect(request.request.headers.get('X-Expected-Actor')).toBe(scope.actor);
    expect(request.request.params.get('document_id')).toBe(scope.document);
    request.flush({ context: { ...context, ...override }, buffers });
  }
  function acknowledge(request: TestRequest, version = 8, override: Partial<DraftSaved> = {}) {
    request.flush({ context, buffer: { ...buffer(request.request.body.content_utf8, version), content_utf8: null }, replayed: false, ...override });
  }

  it('debounces for two idle seconds and serializes newer edits after the exact acknowledgement', () => {
    open(); recovery.update('first'); vi.advanceTimersByTime(1999); http.expectNone(request => request.method === 'PUT');
    recovery.update('second'); vi.advanceTimersByTime(1999); http.expectNone(request => request.method === 'PUT');
    vi.advanceTimersByTime(1); const first = write();
    expect(first.request.body).toMatchObject({ document_id: scope.document, base_head_sha: scope.base,
      expected_draft_version: 0, content_utf8: 'second' });
    recovery.update('third'); vi.advanceTimersByTime(5000); recovery.flush(); http.expectNone(request => request.method === 'PUT');
    acknowledge(first, 8); expect(recovery.status()).toBe('waiting');
    vi.advanceTimersByTime(2000); const next = write();
    expect(next.request.body.expected_draft_version).toBe(8); expect(next.request.body.content_utf8).toBe('third');
    expect(next.request.body.save_id).not.toBe(first.request.body.save_id);
    acknowledge(next, 9); expect(recovery.status()).toBe('saved'); expect(recovery.dirty()).toBe(true);
  });

  it('freezes a lost response across new typing and keeps uncertainty after a later conflict', () => {
    open(); recovery.update('first intent'); recovery.flush(); const first = write(); const body = first.request.body;
    first.error(new ProgressEvent('error')); expect(recovery.status()).toBe('uncertain');
    recovery.update('newer local input'); vi.advanceTimersByTime(5000); recovery.flush(); http.expectNone(request => request.method === 'PUT');
    recovery.retry(); const retry = write(); expect(retry.request.body).toEqual(body);
    retry.flush({ error: { code: 'DRAFT_VERSION_CONFLICT' } }, { status: 409, statusText: 'Conflict' });
    expect(recovery.uncertain()).toBe(true); expect(recovery.content()).toBe('newer local input');
    recovery.retry(); const final = write(); expect(final.request.body).toEqual(body);
    acknowledge(final, 9, { replayed: true }); vi.advanceTimersByTime(2000);
    const latest = write(); expect(latest.request.body.expected_draft_version).toBe(9);
    expect(latest.request.body.content_utf8).toBe('newer local input'); acknowledge(latest, 10);
  });

  it('times out a write conservatively and permits only explicit retries', () => {
    open(); recovery.update('timeout input'); recovery.flush(); const request = write();
    vi.advanceTimersByTime(20_000); expect(request.cancelled).toBe(true); expect(recovery.uncertain()).toBe(true);
    vi.advanceTimersByTime(60_000); http.expectNone(candidate => candidate.method === 'PUT');
    recovery.retry(); const retry = write(); expect(retry.request.body).toEqual(request.request.body); acknowledge(retry);
  });

  it('requires a recovery choice and supports explicit restore or local replacement with CAS', () => {
    open([buffer()]); recovery.update('local input'); vi.advanceTimersByTime(2000);
    http.expectNone(request => request.method === 'PUT'); expect(recovery.choiceRequired()).toBe(true);
    expect(recovery.restore()).toBe(true); expect(recovery.content()).toBe('server recovery');
    expect(recovery.status()).toBe('saved'); http.expectNone(request => request.method === 'PUT');
    recovery.update('deliberate replacement'); recovery.flush(); const request = write();
    expect(request.request.body.expected_draft_version).toBe(7); acknowledge(request);
  });

  it('does not retarget an old recovery base without an explicit adoption decision', () => {
    open([{ ...buffer(), base_head_sha: 'b'.repeat(40), base_is_current: false }]);
    expect(recovery.restore()).toBe(false); expect(recovery.content()).toBe('original Git source');
    expect(recovery.restore(true)).toBe(true); recovery.flush(); const request = write();
    expect(request.request.body.base_head_sha).toBe(scope.base); expect(request.request.body.expected_draft_version).toBe(7);
    acknowledge(request);
  });

  it('saves an Undo back to the original source instead of leaving an obsolete recovery copy', () => {
    open(); recovery.update('edit'); recovery.flush(); acknowledge(write());
    recovery.update('original Git source'); expect(recovery.dirty()).toBe(false);
    recovery.flush(); const undo = write(); expect(undo.request.body.content_utf8).toBe('original Git source'); acknowledge(undo, 9);
  });

  it('pauses in-flight work on session loss, rechecks the same actor, and never resends automatically', () => {
    open(); recovery.update('private input'); recovery.flush(); const request = write();
    recovery.identity(null); expect(request.cancelled).toBe(true); expect(recovery.status()).toBe('suspended');
    expect(recovery.uncertain()).toBe(true); expect(recovery.content()).toBe('private input');
    recovery.retry(); http.expectNone(candidate => candidate.method === 'PUT');
    recovery.identity(scope.actor); read().flush({ context, buffers: [buffer('private input', 8)] });
    expect(recovery.status()).toBe('uncertain'); expect(recovery.pending()).toBe(true);
    vi.advanceTimersByTime(5000); http.expectNone(candidate => candidate.method === 'PUT');
    recovery.retry(); const retry = write(); expect(retry.request.headers.get('X-Expected-Actor')).toBe(scope.actor);
    expect(retry.request.body).toEqual(request.request.body); acknowledge(retry, 8, { replayed: true });
  });

  it('purges every private value on a different login or successful logout and cancels stale reads', () => {
    open([buffer()]); recovery.update('private local input'); recovery.identity('actor-b');
    expect(recovery.owner()).toBeNull(); expect(recovery.content()).toBe(''); expect(recovery.remote()).toBeNull();
    expect(recovery.savedBuffer()).toBeNull(); expect(recovery.context()).toBeNull();
    recovery.open(scope, 'private source'); const stale = read(); recovery.identity(null, true);
    expect(stale.cancelled).toBe(true); expect(recovery.content()).toBe('');
    recovery.retry(); vi.advanceTimersByTime(5000); http.expectNone(request => request.method === 'PUT');
  });

  it('keeps closed-branch edits for recovery while policy and permission failures remain explicit', () => {
    open([], { can_edit: false, proposal_state: 'CLOSED' }); recovery.update('closed local edit'); recovery.flush();
    acknowledge(write(), 8, { context: { ...context, can_edit: false, proposal_state: 'CLOSED' } });
    expect(recovery.status()).toBe('saved'); expect(recovery.context()?.can_edit).toBe(false);
    recovery.clear(); open([], { retention_days: null, can_store_recovery: false }); recovery.update('local only');
    recovery.flush(); expect(recovery.status()).toBe('disabled'); expect(recovery.content()).toBe('local only');
    http.expectNone(request => request.method === 'PUT');
  });

  it('rejects mismatched reads and acknowledgements without claiming a save or replacing local input', () => {
    recovery.open(scope, 'original'); read().flush({ context: { ...context, proposal_id: 'foreign' }, buffers: [buffer()] });
    expect(recovery.status()).toBe('error'); expect(recovery.remote()).toBeNull();
    recovery.clear(); open(); recovery.update('keep me'); recovery.flush();
    acknowledge(write(), 8, { buffer: { ...buffer(), document_id: 'DOC-OTHER' } });
    expect(recovery.status()).toBe('uncertain'); expect(recovery.content()).toBe('keep me'); expect(recovery.savedBuffer()).toBeNull();
  });

  it('reads a CAS conflict for comparison without silently adopting the remote version', () => {
    open(); recovery.update('my edit'); recovery.flush(); const request = write();
    request.flush({ error: { code: 'DRAFT_VERSION_CONFLICT' } }, { status: 409, statusText: 'Conflict' });
    recovery.inspect(); read().flush({ context, buffers: [buffer('other tab input', 10)] });
    expect(recovery.content()).toBe('my edit'); expect(recovery.status()).toBe('conflict');
    recovery.flush(); http.expectNone(candidate => candidate.method === 'PUT');
    recovery.keepLocal(); recovery.flush(); const replacement = write();
    expect(replacement.request.body.expected_draft_version).toBe(10); expect(replacement.request.body.content_utf8).toBe('my edit');
    acknowledge(replacement, 11);
  });

  it('pauses on a rejected account binding and bounds UTF-8 bytes while retaining local input', () => {
    open(); recovery.update('א'.repeat(1024 * 1024 + 1)); recovery.flush();
    expect(recovery.error()).toBeTruthy(); http.expectNone(request => request.method === 'PUT');
    recovery.update('valid unfinished **'); recovery.flush(); const request = write();
    request.flush({ error: { code: 'SESSION_IDENTITY_CHANGED' } }, { status: 409, statusText: 'Conflict' });
    expect(recovery.authRequired()).toBe(true); expect(recovery.status()).toBe('suspended');
    expect(recovery.content()).toBe('valid unfinished **');
  });
});
