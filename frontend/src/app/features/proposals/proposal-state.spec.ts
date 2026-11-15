import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { JobStatus, ProposalChoices, ProposalDetail, ProposalSummary } from '../../core/api/proposal-api';
import { ProposalDetailState, ProposalListState } from './proposal-state';

const summary: ProposalSummary = { proposal_id: 'p1', repository_id: 'repo', number: 25, title: 'הצעה חדשה', branch: 'change/25-new-proposal',
  state: 'DRAFT', owner_id: 'owner', owner_name: 'בעלים', team_id: 'team', team_name: 'צוות', target_branch: 'main', version: 2,
  updated_at: '2026-11-15T00:00:00Z', base_sha: 'a'.repeat(40), head_sha: 'a'.repeat(40), target_sha: 'a'.repeat(40) };
const detail: ProposalDetail = { ...summary, description: 'תיאור הצעת השינוי', author_id: 'owner', initial_mode: 'manual', ui_impact: 'undecided', links: [],
  capabilities: { can_edit: true, can_close: false, can_reopen: false }, synchronization: { state: 'in_sync', actual_head_sha: 'a'.repeat(40) } };
const choices: ProposalChoices = { repositories: [{ repository_id: 'repo', default_branch: 'main', can_create: true }],
  teams: [{ team_id: 'team', name: 'צוות', repository_ids: ['repo'] }], owners: [{ owner_id: 'owner', display_name: 'בעלים', repository_ids: ['repo'] }] };
const pending: ProposalDetail = { ...detail, state: 'PROVISIONING', creation_job_id: 'job1', head_sha: null,
  synchronization: { state: 'provisioning' }, capabilities: { can_edit: false, can_close: false, can_reopen: false } };
function job(state = 'QUEUED'): JobStatus { return { job_id: 'job1', proposal_id: 'p1', kind: 'CREATE_PROPOSAL', state, attempts: 1, phase: state === 'SUCCEEDED' ? 'READY' : 'QUEUED' }; }

describe('Scoped proposal read state', () => {
  let list: ProposalListState;
  let selected: ProposalDetailState;
  let http: HttpTestingController;
  beforeEach(() => {
    vi.useFakeTimers();
    vi.spyOn(document, 'visibilityState', 'get').mockReturnValue('visible');
    TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting(), ProposalListState, ProposalDetailState] });
    list = TestBed.inject(ProposalListState); selected = TestBed.inject(ProposalDetailState); http = TestBed.inject(HttpTestingController);
  });
  afterEach(() => { list.clear(); selected.clear(); http.verify(); vi.restoreAllMocks(); vi.useRealTimers(); });
  function page(q: string) { return http.expectOne(req => req.url === '/api/v1/proposals' && req.params.get('q') === q); }
  function prepare() { selected.load('p1'); http.expectOne('/api/v1/proposals/p1').flush(pending); }

  it('cancels superseded list queries and clears metadata on identity disposal', () => {
    list.loadChoices(); http.expectOne('/api/v1/proposals/options').flush(choices);
    list.load({ q: 'old', repository_id: 'repo' }); const old = page('old');
    list.load({ q: 'מחיקה%', cursor: 'next', state: 'DRAFT' }); expect(old.cancelled).toBe(true);
    const current = page('מחיקה%'); expect(current.request.params.get('cursor')).toBe('next'); expect(current.request.params.get('limit')).toBe('50');
    current.flush({ proposals: [summary], next_cursor: 'later' }); expect(list.rows()[0].proposal_id).toBe('p1');
    expect(list.nextCursor()).toBe('later'); list.clear(); expect(list.rows()).toEqual([]); expect(list.choices()).toBeNull();
  });

  it('preserves failures as errors and cancels choices after denied list access', () => {
    list.load({ q: 'query' }); page('query').flush({ error: { message_he: 'השירות אינו זמין' } }, { status: 503, statusText: 'Unavailable' });
    expect(list.error()).toContain('אינו זמין'); expect(list.rows()).toEqual([]);
    list.loadChoices(); const options = http.expectOne('/api/v1/proposals/options');
    list.load({ q: 'query' }); page('query').flush({}, { status: 404, statusText: 'Not Found' });
    expect(options.cancelled).toBe(true); expect(list.choices()).toBeNull(); expect(list.nextCursor()).toBeNull();
    list.load({ q: '😀'.repeat(201) }); http.expectNone(req => req.url === '/api/v1/proposals'); expect(list.error()).toContain('200');
  });

  it('polls real provisioning status and reloads recorded metadata after success', () => {
    prepare(); http.expectOne('/api/v1/jobs/job1').flush(job());
    expect(selected.detail()?.head_sha).toBeNull(); expect(selected.polling()).toBe(true);
    vi.advanceTimersByTime(1999); http.expectNone('/api/v1/jobs/job1'); vi.advanceTimersByTime(1);
    http.expectOne('/api/v1/jobs/job1').flush(job('SUCCEEDED'));
    http.expectOne('/api/v1/proposals/p1').flush(detail);
    expect(selected.detail()?.state).toBe('DRAFT'); expect(selected.detail()?.head_sha).toBe('a'.repeat(40));
    expect(selected.polling()).toBe(false); vi.advanceTimersByTime(10000); http.expectNone('/api/v1/jobs/job1');
  });

  it('stops terminal failures and drops private metadata on denied job reads', () => {
    prepare(); http.expectOne('/api/v1/jobs/job1').flush({ ...job('FAILED'), phase: 'RECONCILIATION_REQUIRED', error_code: 'CREATE_RESULT_UNKNOWN' });
    expect(selected.polling()).toBe(false); expect(selected.job()?.error_code).toBe('CREATE_RESULT_UNKNOWN');
    expect(selected.detail()?.state).toBe('PROVISIONING'); vi.advanceTimersByTime(10000); http.expectNone('/api/v1/jobs/job1');
    prepare(); http.expectOne('/api/v1/jobs/job1').flush({}, { status: 403, statusText: 'Forbidden' });
    expect(selected.detail()).toBeNull(); expect(selected.job()).toBeNull(); expect(selected.error()).toBeTruthy();
  });

  it('slows hidden-tab polling and cancels it across proposal changes', () => {
    vi.spyOn(document, 'visibilityState', 'get').mockReturnValue('hidden');
    prepare(); http.expectOne('/api/v1/jobs/job1').flush(job());
    vi.advanceTimersByTime(9999); http.expectNone('/api/v1/jobs/job1'); vi.advanceTimersByTime(1);
    const old = http.expectOne('/api/v1/jobs/job1'); selected.load('p2'); expect(old.cancelled).toBe(true);
    http.expectOne('/api/v1/proposals/p2').flush({ ...detail, proposal_id: 'p2' });
    vi.advanceTimersByTime(10000); http.expectNone('/api/v1/jobs/job1'); expect(selected.detail()?.proposal_id).toBe('p2');
  });

  it('bounds waiting and refuses mismatched proposal or job identities', () => {
    selected.load('p1'); http.expectOne('/api/v1/proposals/p1').flush({ ...detail, proposal_id: 'wrong' }); expect(selected.detail()).toBeNull();
    prepare(); http.expectOne('/api/v1/jobs/job1').flush({ ...job(), proposal_id: 'wrong' }); expect(selected.job()).toBeNull(); expect(selected.jobError()).toBeTruthy();
    prepare();
    for (let check = 0; check < 90; ++check) { http.expectOne('/api/v1/jobs/job1').flush(job()); vi.advanceTimersByTime(2000); }
    http.expectOne('/api/v1/jobs/job1').flush(job()); expect(selected.polling()).toBe(false); expect(selected.jobError()).toContain('זמן רב');
    vi.advanceTimersByTime(10000); http.expectNone('/api/v1/jobs/job1');
    prepare(); http.expectOne('/api/v1/jobs/job1').flush(job('SUCCEEDED'));
    http.expectOne('/api/v1/proposals/p1').flush(pending); http.expectOne('/api/v1/jobs/job1').flush(job('SUCCEEDED'));
    expect(selected.polling()).toBe(false); expect(selected.jobError()).toContain('אינם תואמים');
    http.expectNone('/api/v1/proposals/p1');
  });
});
