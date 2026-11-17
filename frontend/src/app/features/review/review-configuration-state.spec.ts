import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting, type TestRequest } from '@angular/common/http/testing';
import type { ReviewConfiguration, ReviewArea } from '../../core/api/review-configuration-api';
import { ReviewConfigurationState } from './review-configuration-state';

const proposal = '00000000-0000-4000-8000-000000000001';
const product = '00000000-0000-4000-8000-000000000010', development = '00000000-0000-4000-8000-000000000020';
const url = `/api/v1/proposals/${proposal}/review/configuration`;
const config = (overrides: Partial<ReviewConfiguration> = {}): ReviewConfiguration => ({
  proposal_id: proposal, number: 25, title: 'Synthetic review', branch: 'change/25-review', target_branch: 'main',
  head_sha: 'a'.repeat(40), proposal_version: 1, configuration_version: 1, policy_version: 1, state: 'DRAFT', can_edit: true,
  settings: { ui_impact: 'undecided', ui_impact_reason: null, assignments: [] }, reviewers: [], ...overrides,
});
function setup() {
  TestBed.configureTestingModule({ providers: [ReviewConfigurationState, provideHttpClient(), provideHttpClientTesting()] });
  const state = TestBed.inject(ReviewConfigurationState), http = TestBed.inject(HttpTestingController);
  const choices = (domain: ReviewArea, items = domain === 'PRODUCT' ? [{ reviewer_id: product, display_name: 'Product reviewer' }]
    : domain === 'DEVELOPMENT' ? [{ reviewer_id: development, display_name: 'Development reviewer' }] : []) =>
    ({ proposal_id: proposal, configuration_version: 1, domain, reviewers: items, next_after: null });
  const flushChoices = (version = 1) => {
    for (const request of http.match(req => req.url.endsWith('/review/reviewers'))) {
      const domain = request.request.params.get('domain') as ReviewArea;
      request.flush({ ...choices(domain), configuration_version: version });
    }
  };
  const open = (value = config()) => {
    state.open('actor-a', proposal);
    const request = http.expectOne(url);
    expect(request.request.headers.get('X-Expected-Actor')).toBe('actor-a');
    request.flush(value);
    if (value.can_edit) flushChoices(value.configuration_version);
  };
  const ack = (request: TestRequest, current?: ReviewConfiguration) => {
    const body = request.request.body;
    request.flush({ proposal_id: proposal, request_id: body.request_id, applied_proposal_version: 2,
      applied_configuration_version: 2, changed: true, replayed: false,
      current: current ?? config({ proposal_version: 2, configuration_version: 2, settings: body.settings,
        reviewers: body.settings.assignments.map((item: { reviewer_id: string }) => ({ ...item,
          display_name: item.reviewer_id === product ? 'Product reviewer' : 'Development reviewer', active: true, qualified: true })) }) });
  };
  return { state, http, open, ack, flushChoices, choices };
}

describe('Review configuration state', () => {
  it('saves all assignments with the captured head and versions and locks edits until acknowledged', () => {
    const { state, http, open, ack, flushChoices } = setup(); open();
    state.add('PRODUCT', product); state.add('PRODUCT', product); state.add('DEVELOPMENT', development);
    expect(state.settings().assignments.length).toBe(2);
    state.required(state.settings().assignments[0], false);
    state.setImpact('no'); expect(state.validation()).toBeTruthy();
    state.save(); http.expectNone(req => req.method === 'PUT');
    state.setReason('  No UI change  '); state.save();
    const save = http.expectOne(req => req.method === 'PUT');
    expect(save.request.headers.get('X-Expected-Actor')).toBe('actor-a');
    expect(save.request.body).toMatchObject({ expected_head_sha: 'a'.repeat(40), expected_proposal_version: 1,
      expected_configuration_version: 1, settings: { ui_impact: 'no', ui_impact_reason: 'No UI change' } });
    expect(save.request.body.settings.assignments.map((item: { domain: string }) => item.domain)).toEqual(['DEVELOPMENT', 'PRODUCT']);
    expect(save.request.body.settings.assignments[1].required).toBe(false);
    state.setReason('Later typing'); state.remove(state.settings().assignments[0]); state.save();
    expect(state.settings().ui_impact_reason).toBe('  No UI change  ');
    http.expectNone(req => req.method === 'PUT');
    ack(save); flushChoices(2);
    expect(state.phase()).toBe('ready'); expect(state.dirty()).toBe(false);
    expect(state.settings().ui_impact_reason).toBe('No UI change');
    expect(state.notice()).toContain('נשמרה'); state.clear(); http.verify();
  });

  it('retries an uncertain write with the exact original actor, request key, settings and versions', () => {
    const { state, http, open, ack, flushChoices } = setup(); open();
    state.setImpact('yes'); state.save();
    const first = http.expectOne(req => req.method === 'PUT'), body = structuredClone(first.request.body);
    first.error(new ProgressEvent('error'));
    expect(state.phase()).toBe('uncertain'); state.setImpact('no'); state.open('other', proposal);
    expect(state.owner()).toBe('actor-a'); expect(state.settings().ui_impact).toBe('yes');
    state.suspend(); state.retry();
    const retry = http.expectOne(req => req.method === 'PUT');
    expect(retry.request.headers.get('X-Expected-Actor')).toBe('actor-a'); expect(retry.request.body).toEqual(body);
    ack(retry, config({ proposal_version: 4, configuration_version: 4, settings: { ui_impact: 'undecided', assignments: [], ui_impact_reason: null } }));
    flushChoices(4); expect(state.notice()).toContain('חדשה יותר'); expect(state.dirty()).toBe(false);
    state.clear(); http.verify();
  });

  it('keeps a timed-out save uncertain and never resubmits it automatically', () => {
    vi.useFakeTimers();
    try {
      const { state, http, open } = setup(); open(); state.setImpact('yes'); state.save();
      const save = http.expectOne(req => req.method === 'PUT');
      vi.advanceTimersByTime(20001);
      expect(save.cancelled).toBe(true); expect(state.phase()).toBe('uncertain');
      vi.advanceTimersByTime(60000); http.expectNone(req => req.method === 'PUT'); state.clear(); http.verify();
    } finally { vi.useRealTimers(); }
  });

  it('preserves edits after a stale response and requires an explicit reload before a new intent', () => {
    const { state, http, open } = setup(); open(); state.setImpact('yes'); state.save();
    http.expectOne(req => req.method === 'PUT').flush({ error: { code: 'REVIEW_CONFIGURATION_STALE' } }, { status: 409, statusText: 'Conflict' });
    expect(state.reloadRequired()).toBe(true); expect(state.dirty()).toBe(true); expect(state.settings().ui_impact).toBe('yes');
    state.save(); http.expectNone(req => req.method === 'PUT'); state.open('actor-a', proposal);
    http.expectOne(url).flush(config({ can_edit: false, proposal_version: 2 }));
    expect(state.settings().ui_impact).toBe('undecided'); expect(state.editable()).toBe(false); state.clear(); http.verify();
  });

  it('allows correction after a definite validation failure and gives the new intent a new key', () => {
    const { state, http, open } = setup(); open(); state.add('PRODUCT', product); state.setImpact('yes'); state.save();
    const first = http.expectOne(req => req.method === 'PUT'), key = first.request.body.request_id;
    first.flush({ error: { code: 'REVIEWER_NOT_ELIGIBLE' } }, { status: 422, statusText: 'Unprocessable Entity' });
    expect(state.editable()).toBe(true); expect(state.settings().assignments.length).toBe(1);
    state.remove(state.settings().assignments[0]); state.save();
    const next = http.expectOne(req => req.method === 'PUT'); expect(next.request.body.request_id).not.toBe(key);
    expect(next.request.body.expected_configuration_version).toBe(1);
    expect(next.request.body.settings.assignments).toEqual([]); state.clear(); expect(next.cancelled).toBe(true); http.verify();
  });

  it('purges private state immediately when a candidate page detects revoked access', () => {
    const { state, http, open } = setup(); open(); state.setReason('Private rationale'); state.candidates('PRODUCT');
    http.expectOne(req => req.url.endsWith('/review/reviewers')).flush({}, { status: 404, statusText: 'Not Found' });
    expect(state.data()).toBeNull(); expect(state.owner()).toBeNull(); expect(state.settings().ui_impact_reason).toBeNull();
    expect(state.names()).toEqual({}); expect(state.choices().PRODUCT.items).toEqual([]); http.verify();
  });

  it('treats a mismatched success receipt as uncertain rather than replacing local settings', () => {
    const { state, http, open, ack, flushChoices } = setup(); open(); state.setImpact('yes'); state.save();
    const first = http.expectOne(req => req.method === 'PUT');
    ack(first, config({ proposal_version: 2, configuration_version: 2 }));
    expect(state.phase()).toBe('uncertain'); expect(state.settings().ui_impact).toBe('yes');
    state.retry(); const retry = http.expectOne(req => req.method === 'PUT'); ack(retry); flushChoices(2);
    expect(state.phase()).toBe('ready'); expect(state.settings().ui_impact).toBe('yes'); state.clear(); http.verify();
  });

  it('pages candidates without losing selected names and checks every continuation identity', () => {
    const { state, http, choices } = setup(); state.open('actor-a', proposal); http.expectOne(url).flush(config());
    const first = Array.from({ length: 50 }, (_, index) => ({ reviewer_id: `00000000-0000-4000-8000-${(index + 100).toString(16).padStart(12, '0')}`, display_name: `Reviewer ${index}` }));
    for (const request of http.match(req => req.url.endsWith('/review/reviewers'))) {
      const domain = request.request.params.get('domain') as ReviewArea;
      request.flush(domain === 'PRODUCT' ? { ...choices(domain, first), next_after: first[49].reviewer_id } : choices(domain));
    }
    state.add('PRODUCT', first[0].reviewer_id); state.candidates('PRODUCT', 'next');
    const next = http.expectOne(req => req.url.endsWith('/review/reviewers'));
    expect(next.request.params.get('after')).toBe(first[49].reviewer_id);
    next.flush(choices('PRODUCT', [{ reviewer_id: '00000000-0000-4000-8000-000000000100', display_name: 'Later reviewer' }]));
    expect(state.choices().PRODUCT.items.length).toBe(1); expect(state.names()[first[0].reviewer_id]).toBe('Reviewer 0');
    state.candidates('PRODUCT', 'previous');
    const previous = http.expectOne(req => req.url.endsWith('/review/reviewers'));
    expect(previous.request.params.has('after')).toBe(false);
    previous.flush({ ...choices('PRODUCT', first), configuration_version: 9, next_after: first[49].reviewer_id });
    expect(state.reloadRequired()).toBe(true); expect(state.choices().PRODUCT.error).toBeTruthy(); state.clear(); http.verify();
  });

  it('keeps selected ineligible reviewers visible on a read-only proposal without requesting candidates', () => {
    const { state, http, open } = setup();
    const selected = { reviewer_id: product, domain: 'PRODUCT' as const, required: true };
    open(config({ can_edit: false, state: 'CLOSED', settings: { ui_impact: 'yes', assignments: [selected] },
      reviewers: [{ ...selected, display_name: 'Revoked reviewer', active: false, qualified: false }] }));
    expect(state.ineligible(selected)).toBe(true); expect(state.names()[product]).toBe('Revoked reviewer');
    state.remove(selected); expect(state.settings().assignments.length).toBe(1); http.expectNone(req => req.url.endsWith('/review/reviewers'));
    state.clear(); http.verify();
  });

  it('cancels private reads when switching proposals and refuses malformed configuration evidence', () => {
    const { state, http } = setup(); state.open('actor-a', proposal); const old = http.expectOne(url);
    const another = '00000000-0000-4000-8000-000000000002'; state.open('actor-a', another);
    expect(old.cancelled).toBe(true);
    http.expectOne(`/api/v1/proposals/${another}/review/configuration`).flush(config());
    expect(state.phase()).toBe('error'); expect(state.data()).toBeNull(); state.clear(); http.verify();
  });
});
