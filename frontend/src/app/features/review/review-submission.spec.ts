import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { actor, proposal, other, configuration, detail, head, target, ticket, publication, job } from '../../../testing/review-fixtures';
import { ReviewSubmission } from './review-submission';

describe('Review submission lifecycle', () => {
  let state: ReviewSubmission, http: HttpTestingController;
  const root = `/api/v1/proposals/${proposal}`;
  beforeEach(() => { vi.useFakeTimers(); TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting(), ReviewSubmission] });
    state = TestBed.inject(ReviewSubmission); http = TestBed.inject(HttpTestingController); });
  afterEach(() => { state.clear(); http.verify(); vi.useRealTimers(); });
  function prepare() {
    state.prepare(actor, proposal); const config = http.expectOne(`${root}/review/configuration`), observed = http.expectOne(root);
    expect(config.request.headers.get('X-Expected-Actor')).toBe(actor); expect(observed.request.headers.get('X-Expected-Actor')).toBe(actor);
    config.flush(configuration()); observed.flush(detail());
  }
  const post = () => http.expectOne(`${root}/submit`);
  const poll = () => http.expectOne(`/api/v1/jobs/${ticket.job_id}`);

  it('selects the checked saved version and polls only the accepted immutable request', () => {
    prepare(); expect(state.phase()).toBe('prepared'); state.start();
    const accepted = post(); expect(accepted.request.body).toEqual({ expected_head_sha: head, expected_target_sha: target,
      expected_proposal_version: 4, expected_configuration_version: 2, expected_policy_version: 1 });
    expect(Object.isFrozen(state.submitted())).toBe(true); expect(accepted.request.headers.get('X-Expected-Actor')).toBe(actor);
    expect(accepted.request.headers.get('Idempotency-Key')).toBeTruthy(); state.start(); http.expectNone(`${root}/submit`);
    accepted.flush(ticket); poll().flush(job()); vi.advanceTimersByTime(2000);
    const running = poll(); expect(running.request.headers.get('X-Expected-Actor')).toBe(actor);
    running.flush(job({ state: 'RUNNING', phase: 'PREPARING', attempts: 1 })); vi.advanceTimersByTime(2000);
    poll().flush(job({ state: 'SUCCEEDED', phase: 'SNAPSHOT_READY', attempts: 1, review: publication() }));
    expect(state.phase()).toBe('completed'); expect(state.pending()).toBe(false); expect(state.result()).toEqual(publication());
    vi.advanceTimersByTime(60000); http.expectNone(`${root}/submit`); http.expectNone(`/api/v1/jobs/${ticket.job_id}`);
  });
  it('keeps key/body/actor after a lost POST response, including a later ambiguous retry rejection', () => {
    prepare(); state.start(); const initial = post(), body = initial.request.body, key = initial.request.headers.get('Idempotency-Key');
    initial.error(new ProgressEvent('error')); expect(state.canRetry()).toBe(true);
    vi.advanceTimersByTime(60000); http.expectNone(`${root}/submit`); state.prepare(actor, proposal); http.expectNone(root);
    state.retry(); const retry = post(); expect(retry.request.body).toEqual(body); expect(retry.request.headers.get('Idempotency-Key')).toBe(key);
    retry.flush({ error: { code: 'REVIEW_SUBMISSION_LIMIT' } }, { status: 429, statusText: 'Limited' });
    expect(state.pending()).toBe(true); expect(state.canRetry()).toBe(true);
    state.retry(); post().flush(ticket); poll().flush(job({ state: 'FAILED', phase: 'FAILED', attempts: 1, error_code: 'REVIEW_NO_CHANGES' }));
    expect(state.pending()).toBe(false); expect(state.phase()).toBe('failed'); expect(state.result()).toBeNull();
  });
  it('does not retry a definite admission rejection or create a new intent on a timer', () => {
    prepare(); state.start(); post().flush({ error: { code: 'REVIEW_SUBMISSION_LIMIT' } }, { status: 429, statusText: 'Limited' });
    expect(state.phase()).toBe('failed'); expect(state.pending()).toBe(false); expect(state.canRetry()).toBe(false);
    vi.advanceTimersByTime(60000); http.expectNone(`${root}/submit`);
  });
  it('bounds preflight and POST waits while preserving uncertain mutation identity', () => {
    state.prepare(actor, proposal); const first = http.expectOne(root), config = http.expectOne(`${root}/review/configuration`);
    vi.advanceTimersByTime(20001); expect(first.cancelled && config.cancelled).toBe(true); expect(state.phase()).toBe('error');
    prepare(); state.start(); const sent = post(), key = sent.request.headers.get('Idempotency-Key');
    vi.advanceTimersByTime(20001); expect(sent.cancelled).toBe(true); expect(state.canRetry()).toBe(true);
    state.retry(); const retry = post(); expect(retry.request.headers.get('Idempotency-Key')).toBe(key); retry.error(new ProgressEvent('error'));
  });
  it('suspends reads and resumes only observation for the same actor; account changes purge the attempt', () => {
    prepare(); state.start(); post().flush(ticket); const pending = poll();
    state.identity(null); expect(pending.cancelled).toBe(true); expect(state.phase()).toBe('suspended');
    vi.advanceTimersByTime(3000); http.expectNone(`/api/v1/jobs/${ticket.job_id}`);
    state.identity(actor); poll().flush(job()); http.expectNone(`${root}/submit`);
    state.identity(other); expect(state.owner()).toBeNull(); expect(state.ticket()).toBeNull(); expect(state.submitted()).toBeNull();
    vi.advanceTimersByTime(3000); http.expectNone(`/api/v1/jobs/${ticket.job_id}`);
  });
  it('never sends an uncertain attempt automatically after session recovery', () => {
    prepare(); state.start(); const sent = post(); state.identity(null); expect(sent.cancelled).toBe(true);
    state.identity(actor); expect(state.phase()).toBe('uncertain'); http.expectNone(`${root}/submit`);
    state.retry(); const retry = post(); expect(retry.request.headers.get('X-Expected-Actor')).toBe(actor); retry.error(new ProgressEvent('error'));
    state.identity(null, true); expect(state.submitted()).toBeNull(); expect(state.pending()).toBe(false);
  });
  it.each(['proposal', 'operation', 'version', 'digest', 'phase'])('rejects mismatched %s publication evidence without replacing the original job', kind => {
    prepare(); state.start(); post().flush(ticket); const result = publication();
    if (kind === 'proposal') result.proposal_id = other;
    if (kind === 'operation') result.operation_id = other;
    if (kind === 'version') result.proposal_version = 6;
    if (kind === 'digest') result.evidence_digest = 'invalid';
    poll().flush(job({ state: 'SUCCEEDED', phase: kind === 'phase' ? 'READY' : 'SNAPSHOT_READY', attempts: 1, review: result }));
    expect(state.result()).toBeNull(); expect(state.pending()).toBe(true); expect(state.phase()).toBe('error');
    state.check(); poll().flush(job({ state: 'SUCCEEDED', phase: 'SNAPSHOT_READY', attempts: 1, review: publication() }));
    expect(state.phase()).toBe('completed');
  });
  it('retains an unknown provider reservation for explicit observation without retrying submission', () => {
    prepare(); state.start(); post().flush(ticket);
    poll().flush(job({ state: 'FAILED', phase: 'RECONCILIATION_REQUIRED', attempts: 8 }));
    expect(state.pending()).toBe(true); expect(state.phase()).toBe('reconciling'); expect(state.canRetry()).toBe(false);
    vi.advanceTimersByTime(60000); http.expectNone(`/api/v1/jobs/${ticket.job_id}`); http.expectNone(`${root}/submit`);
    state.check(); poll().flush(job({ state: 'FAILED', phase: 'FAILED', attempts: 8, error_code: 'REVIEW_SUBMISSION_EXHAUSTED' }));
  });
  it('purges revoked permission and hides results when authentication must be refreshed', () => {
    prepare(); state.start(); post().flush(ticket);
    poll().flush({ error: { code: 'REVIEW_FORBIDDEN' } }, { status: 403, statusText: 'Forbidden' });
    expect(state.ticket()).toBeNull(); expect(state.pending()).toBe(false); expect(state.result()).toBeNull();
    prepare(); state.start(); post().flush(ticket);
    poll().flush({ error: { code: 'SESSION_IDENTITY_CHANGED' } }, { status: 409, statusText: 'Conflict' });
    expect(state.authRequired()).toBe(true); expect(state.phase()).toBe('suspended'); expect(state.canCheck()).toBe(false);
  });
  it('does not prepare a mixed version, revoked reviewer or stale Git observation', () => {
    for (const fault of ['version', 'reviewer', 'source', 'author']) {
      state.prepare(actor, proposal); const config = configuration(), observed = detail();
      if (fault === 'version') observed.version = 5;
      if (fault === 'reviewer') config.reviewers[0].qualified = false;
      if (fault === 'source') observed.synchronization.actual_head_sha = target;
      if (fault === 'author') observed.author_id = config.reviewers[0].reviewer_id;
      http.expectOne(`${root}/review/configuration`).flush(config); http.expectOne(root).flush(observed);
      expect(state.context()).toBeNull(); expect(state.phase()).toBe('error'); state.start(); http.expectNone(`${root}/submit`);
    }
  });
  it('restores existing jobs without a writable request and ignores retargeting while a POST outcome is uncertain', () => {
    prepare(); state.start(); const sent = post(), key = sent.request.headers.get('Idempotency-Key'); sent.error(new ProgressEvent('error'));
    state.restore(actor, proposal, other); http.expectNone(`/api/v1/jobs/${other}`);
    state.retry(); const retry = post(); expect(retry.request.headers.get('Idempotency-Key')).toBe(key); retry.flush(ticket);
    poll().flush(job({ state: 'SUCCEEDED', phase: 'SNAPSHOT_READY', attempts: 1, review: publication() }));
    state.clear(); state.restore(actor, proposal, ticket.job_id); expect(state.submitted()).toBeNull(); state.retry(); http.expectNone(`${root}/submit`);
    poll().flush(job({ state: 'SUCCEEDED', phase: 'SNAPSHOT_READY', attempts: 1, review: publication() })); expect(state.result()).toEqual(publication());
  });
});
