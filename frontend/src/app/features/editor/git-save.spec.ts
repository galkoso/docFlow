import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting, TestRequest } from '@angular/common/http/testing';
import { SaveAccepted, SaveJob, SaveRequest, SaveResult } from '../../core/api/editor-api';
import { GitSave } from './git-save';

const actor = 'actor-a', proposal = 'proposal-a', base = 'a'.repeat(40), head = 'b'.repeat(40);
const ticket: SaveAccepted = { proposal_id: proposal, job_id: '00000000-0000-4000-8000-000000000001',
  operation_id: '00000000-0000-4000-8000-000000000002' };
function input(): SaveRequest {
  return { expected_head_sha: base, expected_proposal_version: 7, message: 'Update chapters', documents: [
    { document_id: 'DOC-ONE', expected_document_sha256: 'a'.repeat(64), markdown: 'first exact source\r\n' },
    { document_id: 'DOC-TWO', expected_document_sha256: 'b'.repeat(64), markdown: 'second exact source\r\n' },
  ] };
}
function result(): SaveResult {
  return { proposal_id: proposal, head_sha: head, proposal_version: 8, no_op: false, reconciled: false,
    committed_at: '2026-11-16T00:00:00Z', documents: [{ document_id: 'DOC-ONE', path: 'docs/one.md', sha256: 'c'.repeat(64),
      bytes: 19, normalization_changed: true, normalization_only: false }] };
}
function job(override: Partial<SaveJob> = {}): SaveJob {
  return { job_id: ticket.job_id, proposal_id: proposal, kind: 'SAVE_PROPOSAL', state: 'QUEUED', attempts: 0,
    phase: 'QUEUED', creation: null, save: null, save_failure: null, error_code: null, ...override };
}

describe('Git save lifecycle', () => {
  let save: GitSave, http: HttpTestingController;
  beforeEach(() => {
    vi.useFakeTimers();
    TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting(), GitSave] });
    save = TestBed.inject(GitSave); http = TestBed.inject(HttpTestingController);
  });
  afterEach(() => { save.clear(); http.verify(); vi.useRealTimers(); });
  function post() { return http.expectOne(request => request.method === 'POST' && request.url.endsWith('/commits')); }
  function poll() { return http.expectOne(`/api/v1/jobs/${ticket.job_id}`); }
  function start() { expect(save.start(actor, proposal, input())).toBe(true); return post(); }
  function accept(request: TestRequest = start()) { request.flush(ticket); return poll(); }
  function completed(value: SaveResult = result()) { accept().flush(job({ state: 'SUCCEEDED', phase: 'READY', save: value })); }

  it('submits two documents atomically and waits for verified publication before applying a baseline', () => {
    const request = start();
    expect(request.request.body.documents).toHaveLength(2);
    expect(request.request.headers.get('X-Expected-Actor')).toBe(actor);
    expect(save.result()).toBeNull(); expect(save.pending()).toBe(true);
    accept(request).flush(job()); expect(save.status()).toBe('queued'); expect(save.result()).toBeNull();
    vi.advanceTimersByTime(1999); http.expectNone(`/api/v1/jobs/${ticket.job_id}`);
    vi.advanceTimersByTime(1); poll().flush(job({ state: 'RUNNING', phase: 'SAVING', attempts: 1 }));
    expect(save.status()).toBe('running'); vi.advanceTimersByTime(2000);
    poll().flush(job({ state: 'SUCCEEDED', phase: 'READY', attempts: 1, save: result() }));
    expect(save.status()).toBe('completed'); expect(save.result()?.head_sha).toBe(head); expect(save.pending()).toBe(true);
    expect(save.start(actor, proposal, input())).toBe(false); expect(save.applied('foreign-job')).toBe(false);
    expect(save.applied(ticket.job_id)).toBe(true); expect(save.pending()).toBe(false); expect(save.submitted()).toBeNull();
    expect(save.result()?.committed_at).toBe('2026-11-16T00:00:00Z');
    vi.advanceTimersByTime(60000); http.expectNone(request => request.method === 'GET');
  });

  it('freezes body and key across later caller edits and a lost response followed by a conflict', () => {
    const caller = input(); expect(save.start(actor, proposal, caller)).toBe(true); const request = post();
    const frozen = request.request.body, key = request.request.headers.get('Idempotency-Key');
    caller.documents[0].markdown = 'later input'; caller.documents.reverse(); caller.message = 'different';
    expect(Object.isFrozen(frozen)).toBe(true); expect(Object.isFrozen(frozen.documents)).toBe(true);
    expect(Object.isFrozen(frozen.documents[0])).toBe(true);
    request.error(new ProgressEvent('error')); expect(save.uncertain()).toBe(true);
    vi.advanceTimersByTime(60000); http.expectNone(request => request.method === 'POST');
    expect(save.start(actor, proposal, caller)).toBe(false);
    save.retry(); const retry = post(); expect(retry.request.body).toEqual(frozen);
    expect(retry.request.headers.get('Idempotency-Key')).toBe(key);
    retry.flush({ error: { code: 'SAVE_CONTEXT_CHANGED' } }, { status: 409, statusText: 'Conflict' });
    expect(save.uncertain()).toBe(true); expect(save.pending()).toBe(true); expect(save.contextChanged()).toBe(true);
    save.retry(); const final = post(); expect(final.request.body).toEqual(frozen);
    expect(final.request.headers.get('X-Expected-Actor')).toBe(actor);
    expect(final.request.headers.get('Idempotency-Key')).toBe(key);
    accept(final).flush(job({ state: 'SUCCEEDED', phase: 'READY', save: { ...result(), reconciled: true } }));
    expect(save.result()?.reconciled).toBe(true);
  });

  it('times out conservatively and never sends another POST after acceptance', () => {
    const first = start(); vi.advanceTimersByTime(20000);
    expect(first.cancelled).toBe(true); expect(save.status()).toBe('uncertain');
    save.retry(); accept(post()).flush(job());
    save.retry(); http.expectNone(request => request.method === 'POST');
    vi.advanceTimersByTime(2000); const read = poll(); vi.advanceTimersByTime(20000);
    expect(read.cancelled).toBe(true); expect(save.status()).toBe('error'); expect(save.pending()).toBe(true);
    vi.advanceTimersByTime(60000); http.expectNone(request => request.method === 'GET');
    save.check(); poll().flush(job({ state: 'SUCCEEDED', phase: 'READY', save: result() })); expect(save.result()).toEqual(result());
  });

  it('retains an exhausted UNKNOWN reservation and observes manually without a new mutation', () => {
    accept().flush(job({ state: 'FAILED', phase: 'RECONCILIATION_REQUIRED', attempts: 8, error_code: 'SAVE_ATTEMPTS_EXHAUSTED' }));
    expect(save.status()).toBe('reconciling'); expect(save.pending()).toBe(true); expect(save.uncertain()).toBe(true);
    vi.advanceTimersByTime(60000); http.expectNone(request => request.method === 'GET' || request.method === 'POST');
    expect(save.start(actor, proposal, input())).toBe(false); save.retry(); http.expectNone(request => request.method === 'POST');
    save.check(); poll().flush(job({ state: 'FAILED', phase: 'RECONCILIATION_REQUIRED', attempts: 8 }));
    expect(save.pending()).toBe(true); expect(save.result()).toBeNull();
  });

  it('accepts no-op evidence without inventing a commit time or version advance', () => {
    completed({ proposal_id: proposal, head_sha: base, proposal_version: 7, no_op: true, reconciled: false, committed_at: null, documents: [] });
    expect(save.status()).toBe('completed'); expect(save.result()?.no_op).toBe(true); expect(save.result()?.committed_at).toBeNull();
    expect(save.applied(ticket.job_id)).toBe(true);
  });

  it('distinguishes definite rejection from uncertainty and creates a fresh key only for a new deliberate save', () => {
    const first = start(), key = first.request.headers.get('Idempotency-Key');
    first.flush({ error: { code: 'SAVE_CONTEXT_CHANGED' } }, { status: 409, statusText: 'Conflict' });
    expect(save.status()).toBe('failed'); expect(save.pending()).toBe(false); expect(save.result()).toBeNull();
    const second = start(); expect(second.request.headers.get('Idempotency-Key')).not.toBe(key);
    second.flush({ error: { code: 'SAVE_UNAVAILABLE' } }, { status: 503, statusText: 'Unavailable' });
    expect(save.status()).toBe('uncertain'); expect(save.pending()).toBe(true);
  });

  it('stops a failed job and exposes bounded source-free validation while preserving its submitted intent', () => {
    const validation = { complete: true, findings_total: 1, blockers_total: 1, findings_truncated: false, findings: [
      { code: 'SECTION_MISSING', path: 'docs/other.md', path_truncated: false, severity: 'blocker', source_start_byte: 14, source_end_byte: 20 },
    ] };
    accept().flush(job({ state: 'FAILED', phase: 'FAILED', error_code: 'SAVE_VALIDATION_FAILED', save_failure: { validation } }));
    expect(save.status()).toBe('failed'); expect(save.pending()).toBe(false);
    expect(save.failure()?.validation?.findings[0].path).toBe('docs/other.md'); expect(save.submitted()?.documents[0].markdown).toContain('first exact');
    vi.advanceTimersByTime(60000); http.expectNone(request => request.method === 'GET');
  });

  it('rejects mismatched submission receipts as uncertain and keeps the original request for manual retry', () => {
    const request = start(), key = request.request.headers.get('Idempotency-Key');
    request.flush({ ...ticket, proposal_id: 'foreign' }); expect(save.status()).toBe('uncertain'); expect(save.ticket()).toBeNull();
    save.retry(); const retry = post(); expect(retry.request.headers.get('Idempotency-Key')).toBe(key);
    accept(retry).flush(job());
  });

  it('rejects mismatched jobs and inconsistent success evidence without claiming or permitting another save', () => {
    const cases: Partial<SaveJob>[] = [
      { job_id: 'foreign' }, { proposal_id: 'foreign' }, { kind: 'CREATE_PROPOSAL' }, { state: 'QUEUED', phase: 'READY', save: result() },
      { state: 'SUCCEEDED', phase: 'READY', save: { ...result(), head_sha: base } },
      { state: 'SUCCEEDED', phase: 'READY', save: { ...result(), proposal_version: 9 } },
      { state: 'SUCCEEDED', phase: 'READY', save: { ...result(), committed_at: 'invalid date' } },
      { state: 'SUCCEEDED', phase: 'READY', save: { ...result(), documents: [{ ...result().documents[0], document_id: 'DOC-FOREIGN' }] } },
      { state: 'SUCCEEDED', phase: 'READY', save: { ...result(), no_op: true } },
    ];
    for (const value of cases) {
      save.clear(); accept().flush(job(value));
      expect(save.result()).toBeNull(); expect(save.pending()).toBe(true); expect(save.status()).toBe('error');
      expect(save.start(actor, proposal, input())).toBe(false);
    }
  });

  it('pauses an unacknowledged request on session loss and never retries automatically after login', () => {
    const request = start(), key = request.request.headers.get('Idempotency-Key'); save.identity(null);
    expect(request.cancelled).toBe(true); expect(save.status()).toBe('suspended'); expect(save.uncertain()).toBe(true);
    save.retry(); http.expectNone(request => request.method === 'POST'); save.identity(actor);
    expect(save.status()).toBe('uncertain'); vi.advanceTimersByTime(60000); http.expectNone(request => request.method === 'POST');
    save.retry(); const retry = post(); expect(retry.request.headers.get('Idempotency-Key')).toBe(key);
    accept(retry).flush(job());
  });

  it('resumes only safe polling for accepted work and preserves a completed receipt until the baseline is applied', () => {
    const read = accept(); save.identity(null); expect(read.cancelled).toBe(true);
    save.identity(actor); const next = poll(); expect(next.request.headers.get('X-Expected-Actor')).toBe(actor);
    next.flush(job({ state: 'SUCCEEDED', phase: 'READY', save: result() }));
    save.identity(null); expect(save.status()).toBe('suspended'); save.identity(actor);
    expect(save.status()).toBe('completed'); expect(save.result()?.head_sha).toBe(head); expect(save.pending()).toBe(true);
    http.expectNone(request => request.method === 'GET' || request.method === 'POST');
  });

  it('requests reauthentication without losing the original actor/key and clears every private value on account change', () => {
    const request = start(), key = request.request.headers.get('Idempotency-Key');
    request.flush({ error: { code: 'SESSION_EXPIRED' } }, { status: 401, statusText: 'Unauthorized' });
    expect(save.authRequired()).toBe(true); expect(save.status()).toBe('suspended'); save.identity(actor); save.retry();
    const retry = post(); expect(retry.request.headers.get('X-Expected-Actor')).toBe(actor);
    expect(retry.request.headers.get('Idempotency-Key')).toBe(key); const read = accept(retry);
    save.identity('actor-b'); expect(read.cancelled).toBe(true); expect(save.owner()).toBeNull(); expect(save.submitted()).toBeNull();
    expect(save.ticket()).toBeNull(); expect(save.result()).toBeNull(); expect(save.failure()).toBeNull(); expect(save.pending()).toBe(false);
    vi.advanceTimersByTime(60000); http.expectNone(request => request.method === 'GET');
    completed(); save.identity(null, true); expect(save.result()).toBeNull(); expect(save.submitted()).toBeNull();
  });

  it('keeps private job failures explicit after access revocation without blindly resubmitting', () => {
    accept().flush({ error: { code: 'NOT_FOUND' } }, { status: 404, statusText: 'Not Found' });
    expect(save.contextChanged()).toBe(true); expect(save.status()).toBe('error'); expect(save.pending()).toBe(true);
    save.retry(); http.expectNone(request => request.method === 'POST');
    save.check(); poll().flush(job({ state: 'FAILED', phase: 'FAILED', error_code: 'SAVE_FORBIDDEN' }));
    expect(save.status()).toBe('failed'); expect(save.pending()).toBe(false);
  });

  it('bounds local source before sending and drops oversized or malformed diagnostic projections', () => {
    const invalid = input(); invalid.documents[0].markdown = 'x'.repeat(MAX_FILE + 1);
    expect(save.start(actor, proposal, invalid)).toBe(false); http.expectNone(request => request.method === 'POST');
    invalid.documents[0].markdown = '\0'; expect(save.start(actor, proposal, invalid)).toBe(false);
    accept().flush(job({ state: 'FAILED', phase: 'FAILED', save_failure: { validation: {
      complete: true, findings_total: 1, blockers_total: 1, findings_truncated: false,
      findings: [{ code: 'BAD', path: 'x'.repeat(241), path_truncated: false, severity: 'blocker' }],
    } } }));
    expect(save.failure()?.validation).toBeNull();
  });
});
const MAX_FILE = 2 * 1024 * 1024;
