import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { PreparedCreation, ProposalChoices } from '../../core/api/proposal-api';
import { defaultBranch, ProposalCreateState } from './proposal-create-state';

const choices: ProposalChoices = { repositories: [{ repository_id: 'repo', default_branch: 'main', can_create: true }],
  teams: [{ team_id: 'team', name: 'צוות', repository_ids: ['repo'] }],
  owners: [{ owner_id: 'actor', display_name: 'בעלים', repository_ids: ['repo'] }, { owner_id: 'other', display_name: 'שותף', repository_ids: ['repo'] }] };
const context: PreparedCreation = { context_id: 'context1', repository_id: 'repo', number: 25, source_branch: 'main', source_sha: 'a'.repeat(40), expires_at: '2027-01-01T00:00:00Z' };
const ticket = { proposal_id: 'p1', number: 25, job_id: 'j1', operation_id: 'o1' };

describe('Proposal creation intent and recovery', () => {
  let state: ProposalCreateState;
  let http: HttpTestingController;
  beforeEach(() => {
    TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting(), ProposalCreateState] });
    state = TestBed.inject(ProposalCreateState); http = TestBed.inject(HttpTestingController);
  });
  afterEach(() => { state.dispose(); http.verify(); });
  function scope(actor = 'actor', repository = 'repo') {
    state.scope(actor, repository); http.expectOne('/api/v1/proposals/options').flush(choices);
  }
  function open() { scope(); http.expectOne('/api/v1/proposals/context').flush(context); }
  function fill() { state.update('title', 'הצעה חדשה'); state.update('description', 'תיאור מלא של השינוי המוצע'); }

  it('uses the reserved number, preserves a custom branch and validates required reasoning', () => {
    open(); expect(state.form().branch).toBe('change/25-new-proposal'); expect(state.form().owner_id).toBe('actor');
    state.update('title', 'Bulk Delete'); expect(state.form().branch).toBe('change/25-bulk-delete');
    state.update('branch', 'change/custom'); state.update('title', 'שם עברי'); expect(state.form().branch).toBe('change/custom');
    state.suggestBranch(); expect(state.form().branch).toBe('change/25-new-proposal');
    fill(); state.update('ui_impact', 'no'); state.submit(); http.expectNone('/api/v1/proposals'); expect(state.fieldErrors()['reason']).toBeTruthy();
    state.update('reason', 'שינוי לוגיקה בלבד'); state.submit();
    const request = http.expectOne('/api/v1/proposals'); expect(request.request.body.creation_context_id).toBe('context1'); expect(request.request.body.expected_source_sha).toBe(context.source_sha);
    request.flush(ticket); expect(state.ticket()).toEqual(ticket); expect(state.dirty()).toBe(false); expect(state.ticketOwner()).toBe('actor');
    expect(defaultBranch(25, 'שלום עולם')).toBe('change/25-new-proposal');
  });

  it('freezes uncertain requests and retries exactly the same body and key', () => {
    open(); fill(); state.submit(); const original = http.expectOne('/api/v1/proposals');
    const key = original.request.headers.get('Idempotency-Key'), body = original.request.body;
    state.submit(); http.expectNone('/api/v1/proposals');
    original.error(new ProgressEvent('error')); expect(state.uncertain()).toBe(true);
    state.update('title', 'שינוי אסור בזמן המתנה'); state.prepare(true); http.expectNone('/api/v1/proposals/context');
    expect(state.form().title).toBe('הצעה חדשה'); state.submit(); const retry = http.expectOne('/api/v1/proposals');
    expect(retry.request.headers.get('Idempotency-Key')).toBe(key); expect(retry.request.body).toEqual(body);
    retry.flush({ error: { code: 'SOURCE_MOVED', message_he: 'המקור השתנה' } }, { status: 409, statusText: 'Conflict' });
    expect(state.uncertain()).toBe(true); // this response cannot settle an earlier lost response
    state.submit(); const final = http.expectOne('/api/v1/proposals'); expect(final.request.headers.get('Idempotency-Key')).toBe(key);
    final.flush(ticket); expect(state.ticket()).toEqual(ticket);
  });

  it('requires explicit preparation after source movement and preserves entered text', () => {
    open(); fill(); state.submit(); const original = http.expectOne('/api/v1/proposals'); const key = original.request.headers.get('Idempotency-Key');
    original.flush({ error: { code: 'SOURCE_MOVED', message_he: 'המקור השתנה', details: { actual_source_sha: 'b'.repeat(40) } } }, { status: 409, statusText: 'Conflict' });
    expect(state.context()?.source_sha).toBe(context.source_sha); expect(state.movedSource()).toBe('b'.repeat(40)); expect(state.uncertain()).toBe(false);
    http.expectNone('/api/v1/proposals/context'); state.prepare(true);
    http.expectOne('/api/v1/proposals/context').flush({ ...context, context_id: 'context2', number: 26, source_sha: 'b'.repeat(40) });
    expect(state.form().description).toBe('תיאור מלא של השינוי המוצע'); expect(state.form().branch).toBe('change/26-new-proposal');
    http.expectNone('/api/v1/proposals'); state.submit(); const fresh = http.expectOne('/api/v1/proposals');
    expect(fresh.request.headers.get('Idempotency-Key')).not.toBe(key); expect(fresh.request.body.expected_source_sha).toBe('b'.repeat(40));
    fresh.flush({ ...ticket, number: 26 });
  });

  it('recovers only for the same identity and never resubmits on login', () => {
    open(); fill(); state.submit(); const original = http.expectOne('/api/v1/proposals'); const key = original.request.headers.get('Idempotency-Key');
    state.scope(); expect(original.cancelled).toBe(true); expect(state.choices()).toBeNull(); expect(state.context()).toBeNull();
    scope(); expect(state.form().title).toBe('הצעה חדשה'); expect(state.uncertain()).toBe(true); http.expectNone('/api/v1/proposals');
    state.submit(); const retry = http.expectOne('/api/v1/proposals'); expect(retry.request.headers.get('Idempotency-Key')).toBe(key);
    retry.error(new ProgressEvent('error')); scope('other'); http.expectOne('/api/v1/proposals/context').flush({ ...context, context_id: 'other-context', number: 27 });
    expect(state.form().title).toBe(''); expect(state.form().description).toBe(''); expect(state.uncertain()).toBe(false); expect(state.ticket()).toBeNull();
    expect(state.form().owner_id).toBe('other'); http.expectNone('/api/v1/proposals');
  });

  it('reuses preparation keys after network errors and refuses inaccessible explicit scopes', () => {
    scope(); const original = http.expectOne('/api/v1/proposals/context'); const key = original.request.headers.get('Idempotency-Key');
    original.error(new ProgressEvent('error')); state.prepare(false); const retry = http.expectOne('/api/v1/proposals/context');
    expect(retry.request.headers.get('Idempotency-Key')).toBe(key); retry.flush(context);
    state.scope('actor', 'foreign'); http.expectOne('/api/v1/proposals/options').flush(choices);
    http.expectNone('/api/v1/proposals/context'); expect(state.form().repository_id).toBe('foreign'); expect(state.context()).toBeNull(); expect(state.error()).toContain('הרשאה');
  });

  it('does not route or claim acceptance when the returned identity contradicts the reserved number', () => {
    open(); fill(); state.submit(); http.expectOne('/api/v1/proposals').flush({ ...ticket, number: 999 });
    expect(state.ticket()).toBeNull(); expect(state.uncertain()).toBe(true); expect(state.error()).toContain('אינה תואמת');
  });
  it('retains an explicit owner when eligibility changes instead of silently transferring ownership', () => {
    open(); fill(); state.update('owner_id', 'other'); state.scope('actor', 'repo');
    http.expectOne('/api/v1/proposals/options').flush({ ...choices, owners: choices.owners.filter(owner => owner.owner_id === 'actor') });
    http.expectOne('/api/v1/proposals/context').flush(context);
    expect(state.form().owner_id).toBe('other'); expect(state.fieldErrors()['owner_id']).toBeTruthy();
    state.submit(); http.expectNone('/api/v1/proposals');
    state.update('owner_id', 'actor'); state.submit();
    const request = http.expectOne('/api/v1/proposals'); expect(request.request.body.owner_id).toBe('actor'); request.flush(ticket);
  });

});
