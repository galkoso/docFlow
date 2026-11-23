import { product, actor, proposal, other, metadata, officialHistory } from '../../../testing/review-fixtures';
import { validOfficialHistory } from './official-review-evidence';

describe('Official review evidence validation', () => {
  it('accepts current caller assignments without claiming live provider readiness', () => {
    expect(validOfficialHistory(officialHistory(product), product, proposal, metadata())).toBe(true);
    expect(validOfficialHistory(officialHistory(actor), actor, proposal, metadata())).toBe(true);
  });
  it.each(['snapshot', 'head', 'target', 'fingerprint', 'policy', 'domain', 'warning', 'availability'])('rejects mismatched %s evidence', fault => {
    const v = officialHistory(product);
    if (fault === 'snapshot') v.snapshot_id = other;
    if (fault === 'head') v.head_sha = 'c'.repeat(40);
    if (fault === 'target') v.target_sha = 'c'.repeat(40);
    if (fault === 'fingerprint') v.fingerprint = 'c'.repeat(64);
    if (fault === 'policy') v.policy_version++;
    if (fault === 'domain') v.domains = ['DESIGN'];
    if (fault === 'warning') v.unviewed_content_warning = false;
    if (fault === 'availability') v.approve_all_available = true;
    expect(validOfficialHistory(v, product, proposal, metadata())).toBe(false);
  });
  it('rejects a comment masquerading as an effective approval and revoked decisions marked valid', () => {
    const v = officialHistory(product);
    v.entries = [{ id: other, reviewer_id: product, reviewer_name: 'Reviewer', domain: 'PRODUCT', decision: 'COMMENT', explanation: 'Note',
      recorded_at: '2026-11-19T00:00:00Z', effective: true, reviewer_eligible: true, valid_for_stored_context: true }];
    expect(validOfficialHistory(v, product, proposal, metadata())).toBe(false);
    v.entries[0].decision = 'APPROVE_ALL'; v.entries[0].reviewer_eligible = false;
    expect(validOfficialHistory(v, product, proposal, metadata())).toBe(false);
    v.entries[0].valid_for_stored_context = false; expect(validOfficialHistory(v, product, proposal, metadata())).toBe(true);
  });
  it('accepts complete read evidence without granting approval readiness', () => {
    const value = officialHistory(product); value.unviewed_domains = []; value.unviewed_content_warning = false;
    expect(validOfficialHistory(value, product, proposal, metadata())).toBe(true); expect(value.approve_all_available).toBe(false);
  });
  it.each(['missing', 'duplicate', 'unassigned', 'unknown', 'nonboolean', 'ineligible', 'empty'])('rejects inconsistent %s warning evidence', fault => {
    const value = officialHistory(product);
    if (fault === 'missing') delete (value as Partial<typeof value>).unviewed_domains;
    if (fault === 'duplicate') value.unviewed_domains = ['PRODUCT', 'PRODUCT'];
    if (fault === 'unassigned') value.unviewed_domains = ['DESIGN'];
    if (fault === 'unknown') value.unviewed_domains = ['UNKNOWN' as never];
    if (fault === 'nonboolean') value.unviewed_content_warning = 'true' as never;
    if (fault === 'ineligible') { value.domains = []; value.unviewed_domains = []; value.unviewed_content_warning = false; }
    if (fault === 'empty') value.unviewed_domains = [];
    expect(validOfficialHistory(value, product, proposal, metadata())).toBe(false);
  });
});
