import type { ConfigurationUpdated, ReviewConfiguration, ReviewConfigurationRequest, ReviewerCandidates } from '../../core/api/review-configuration-api';
import { canonical, validCandidates, validConfiguration, validReceipt } from './review-evidence';

const proposal = '00000000-0000-4000-8000-000000000001', reviewer = '00000000-0000-4000-8000-000000000002';
const selected = { reviewer_id: reviewer, domain: 'PRODUCT' as const, required: true };
const data = (): ReviewConfiguration => ({ proposal_id: proposal, number: 25, title: 'Synthetic review', branch: 'change/25-review', target_branch: 'main',
  head_sha: 'a'.repeat(40), proposal_version: 2, configuration_version: 2, policy_version: 1, state: 'DRAFT', can_edit: true,
  settings: { ui_impact: 'no', ui_impact_reason: 'No UI change', assignments: [{ ...selected }] },
  reviewers: [{ ...selected, display_name: 'Reviewer', active: true, qualified: true }] });

describe('Review response evidence', () => {
  it.each(['proposal', 'versions', 'head', 'state', 'assignment', 'status', 'reason', 'name'])('rejects invalid %s context before it can become editable state', kind => {
    const value = data();
    if (kind === 'proposal') value.proposal_id = reviewer;
    if (kind === 'versions') value.configuration_version = Number.MAX_SAFE_INTEGER + 1;
    if (kind === 'head') value.head_sha = 'short';
    if (kind === 'state') value.state = 'MERGED';
    if (kind === 'assignment') value.settings.assignments.push({ ...selected });
    if (kind === 'status') value.reviewers[0].domain = 'DESIGN';
    if (kind === 'reason') value.settings.ui_impact_reason = '  ';
    if (kind === 'name') value.reviewers[0].display_name = 'א'.repeat(201);
    expect(validConfiguration(value, proposal)).toBe(false);
  });

  it('validates matching statuses and Unicode bounds while retaining revoked historical assignments', () => {
    const value = data(); value.reviewers[0].active = false; value.reviewers[0].qualified = false;
    value.reviewers[0].display_name = '😀'.repeat(200); expect(validConfiguration(value, proposal)).toBe(true);
    value.reviewers[0].qualified = true; expect(validConfiguration(value, proposal)).toBe(false);
  });

  it('canonicalizes equivalent form edits without changing required assignment semantics', () => {
    const value = data().settings;
    value.assignments.push({ reviewer_id: proposal, domain: 'DEVELOPMENT', required: false });
    value.ui_impact_reason = '  No UI change  ';
    const normalized = canonical(value); expect(normalized.assignments[0].domain).toBe('DEVELOPMENT');
    expect(normalized.assignments[0].required).toBe(false); expect(normalized.ui_impact_reason).toBe('No UI change');
    expect(value.assignments[0].domain).toBe('PRODUCT'); expect(value.ui_impact_reason).toBe('  No UI change  ');
    value.ui_impact_reason = '\u0085No UI change\u0085'; expect(canonical(value).ui_impact_reason).toBe('No UI change');
    value.ui_impact_reason = '\ufeffNo UI change\ufeff'; expect(canonical(value).ui_impact_reason).toBe(value.ui_impact_reason);
  });

  it.each(['scope', 'version', 'domain', 'order', 'duplicate', 'cursor', 'size'])('rejects candidate pages with mismatched %s', kind => {
    const value: ReviewerCandidates = { proposal_id: proposal, configuration_version: 2, domain: 'PRODUCT',
      reviewers: [{ reviewer_id: reviewer, display_name: 'Reviewer' }], next_after: null };
    if (kind === 'scope') value.proposal_id = reviewer;
    if (kind === 'version') value.configuration_version = 1;
    if (kind === 'domain') value.domain = 'DESIGN';
    if (kind === 'order') value.reviewers.push({ reviewer_id: proposal, display_name: 'Earlier' });
    if (kind === 'duplicate') value.reviewers.push({ ...value.reviewers[0] });
    if (kind === 'cursor') value.next_after = reviewer;
    if (kind === 'size') value.reviewers = Array.from({ length: 51 }, () => ({ ...value.reviewers[0] }));
    expect(validCandidates(value, proposal, 2, 'PRODUCT')).toBe(false);
  });

  it('binds the acknowledgement to the exact intent but allows a later current configuration', () => {
    const intent: ReviewConfigurationRequest = { request_id: proposal, expected_head_sha: 'a'.repeat(40),
      expected_proposal_version: 1, expected_configuration_version: 1, settings: data().settings };
    const receipt: ConfigurationUpdated = { proposal_id: proposal, request_id: proposal, applied_proposal_version: 2,
      applied_configuration_version: 2, changed: true, replayed: false, current: data() };
    expect(validReceipt(receipt, proposal, intent)).toBe(true);
    receipt.request_id = reviewer; expect(validReceipt(receipt, proposal, intent)).toBe(false); receipt.request_id = proposal;
    receipt.current.head_sha = 'b'.repeat(40); expect(validReceipt(receipt, proposal, intent)).toBe(false);
    receipt.current.proposal_version = 3; expect(validReceipt(receipt, proposal, intent)).toBe(true);
    receipt.current.settings.ui_impact_reason = 'Another intent'; expect(validReceipt(receipt, proposal, intent)).toBe(false);
    receipt.current.configuration_version = 3; expect(validReceipt(receipt, proposal, intent)).toBe(true);
  });
});
