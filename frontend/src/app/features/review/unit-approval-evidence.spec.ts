import { metadata, product, proposal, other, reviewUnits } from '../../../testing/review-fixtures';
import { unitApprovalPage, unitApprovalReceipt, unitApprovalRequest } from '../../../testing/unit-approval-fixtures';
import { validUnitApprovalPage, validUnitApprovalReceipt } from './unit-approval-evidence';

describe('Individual approval evidence', () => {
  const page = () => reviewUnits(product);
  const valid = (value = unitApprovalPage(), manifest = page()) => validUnitApprovalPage(value, product, proposal, metadata(), manifest, 'PRODUCT');
  const receipt = (value = unitApprovalReceipt(), request = unitApprovalRequest()) => validUnitApprovalReceipt(value, product, proposal, metadata(), request);
  it('accepts unapproved and exact approved records while preserving historical and legacy facts', () => {
    expect(valid()).toBe(true); const value = unitApprovalPage(), saved = unitApprovalReceipt();
    value.units[0] = { unit_id: saved.unit_id, approval_id: saved.id, approved_at: saved.recorded_at }; value.approved_units = 1; value.unapproved_units = 1;
    expect(valid(value)).toBe(true); expect(valid({ ...value, current_for_metadata: false })).toBe(true);
    const legacy = { ...page(), available: false, total: 0, next_offset: null, units: [] };
    expect(valid(unitApprovalPage(legacy), legacy)).toBe(true); expect(valid({ ...unitApprovalPage(legacy), available: true }, legacy)).toBe(false);
    expect(receipt()).toBe(true); expect(receipt({ ...saved, replayed: true })).toBe(true);
  });
  it.each([{ actor_id: other }, { proposal_id: other }, { snapshot_id: other }, { fingerprint: '0'.repeat(64) },
    { head_sha: '0'.repeat(40) }, { target_sha: '0'.repeat(40) }, { policy_version: 2 }, { configuration_version: 3 },
    { domain: 'DESIGN' }, { current_for_metadata: null }, { available: false }, { total_units: 3 }, { offset: 20 }, { next_offset: 1 },
    { approved_units: 1 }, { unapproved_units: -1 }])('rejects inconsistent page scope or summary: %j', patch => {
    expect(valid({ ...unitApprovalPage(), ...patch } as never)).toBe(false);
  });
  it('requires exact ordered unit identities, paired valid receipt IDs/times and possible global counts', () => {
    const reversed = unitApprovalPage(); reversed.units.reverse(); expect(valid(reversed)).toBe(false);
    const duplicate = unitApprovalPage(); duplicate.units[1] = duplicate.units[0]; expect(valid(duplicate)).toBe(false);
    const half = unitApprovalPage(); half.units[0].approved_at = unitApprovalReceipt().recorded_at; expect(valid(half)).toBe(false);
    half.units[0].approval_id = other; half.units[0].approved_at = 'yesterday'; expect(valid(half)).toBe(false);
    half.units[0].approved_at = unitApprovalReceipt().recorded_at; expect(valid(half)).toBe(false);
    half.approved_units = 1; half.unapproved_units = 1; expect(valid(half)).toBe(true);
    half.units[1] = { ...half.units[0], unit_id: page().units[1].id }; half.approved_units = 2; half.unapproved_units = 0;
    expect(valid(half)).toBe(false);
    const p = reviewUnits(product, 20, 22), global = unitApprovalPage(p); global.approved_units = 15; global.unapproved_units = 7;
    expect(valid(global, p)).toBe(true); global.approved_units = 21; global.unapproved_units = 1; expect(valid(global, p)).toBe(false);
  });
  it.each([{ id: 'bad' }, { actor_id: other }, { proposal_id: other }, { snapshot_id: other }, { request_id: product },
    { unit_id: '0'.repeat(64) }, { fingerprint: '0'.repeat(64) }, { head_sha: '0'.repeat(40) }, { target_sha: '0'.repeat(40) },
    { domain: 'DESIGN' }, { policy_version: 2 }, { configuration_version: 3 }, { recorded_at: 'yesterday' }, { replayed: null }])('rejects mixed receipt evidence: %j', patch => {
    expect(receipt({ ...unitApprovalReceipt(), ...patch } as never)).toBe(false);
  });
  it('rejects known author, assignment and malformed captured intent mismatches', () => {
    const s = metadata(); s.metadata.context.author_id = product;
    expect(validUnitApprovalPage(unitApprovalPage(), product, proposal, s, page(), 'PRODUCT')).toBe(false);
    expect(validUnitApprovalReceipt(unitApprovalReceipt(), product, proposal, s, unitApprovalRequest())).toBe(false);
    s.metadata.context.author_id = other; s.metadata.assignments = [];
    expect(validUnitApprovalPage(unitApprovalPage(), product, proposal, s, page(), 'PRODUCT')).toBe(false);
    expect(validUnitApprovalReceipt(unitApprovalReceipt(), product, proposal, s, unitApprovalRequest())).toBe(false);
    expect(receipt(unitApprovalReceipt(), { ...unitApprovalRequest(), request_id: 'bad' })).toBe(false);
    expect(receipt(unitApprovalReceipt(), { ...unitApprovalRequest(), expected_fingerprint: '0'.repeat(64) })).toBe(false);
  });
  it('keeps unsupported work unapproved until a supported acknowledgement mechanism exists', () => {
    const p = page(); p.units[0].requirement = { kind: 'file', reason: 'non_document' }; p.units[0].before = null;
    p.units[0].after = { source: { path: `assets/${'3'.repeat(64)}.png`, blob_oid: 'a'.repeat(40), bytes: 10, executable: false }, range: null };
    const value = unitApprovalPage(p); expect(valid(value, p)).toBe(true);
    const saved = unitApprovalReceipt(); value.units[0] = { unit_id: p.units[0].id, approval_id: saved.id, approved_at: saved.recorded_at };
    value.approved_units = 1; value.unapproved_units = 1; expect(valid(value, p)).toBe(false);
  });
});
