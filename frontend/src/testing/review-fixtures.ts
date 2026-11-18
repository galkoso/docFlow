import type { ReviewConfiguration } from '../app/core/api/review-configuration-api';
import type { ReviewAccepted, ReviewDetail, ReviewFiles, ReviewHistory, ReviewJob, ReviewMedia,
  ReviewPublication, ReviewSnapshot } from '../app/core/api/review-api';

export const proposal = '00000000-0000-4000-8000-000000000001', actor = '00000000-0000-4000-8000-000000000004';
export const product = '00000000-0000-4000-8000-000000000010', development = '00000000-0000-4000-8000-000000000020';
export const snapshot = '00000000-0000-4000-8000-000000000030', other = '00000000-0000-4000-8000-000000000099';
export const head = 'a'.repeat(40), target = 'b'.repeat(40), base = '0'.repeat(40);
export const configuration = (): ReviewConfiguration => ({ proposal_id: proposal, number: 25, title: 'Synthetic review', branch: 'change/25-review',
  target_branch: 'main', head_sha: head, proposal_version: 4, configuration_version: 2, policy_version: 1, state: 'DRAFT', can_edit: true,
  settings: { ui_impact: 'no', ui_impact_reason: 'No UI changes', assignments: [
    { reviewer_id: product, domain: 'PRODUCT', required: true }, { reviewer_id: development, domain: 'DEVELOPMENT', required: true },
  ] }, reviewers: [
    { reviewer_id: product, domain: 'PRODUCT', required: true, display_name: 'Product reviewer', active: true, qualified: true },
    { reviewer_id: development, domain: 'DEVELOPMENT', required: true, display_name: 'Development reviewer', active: true, qualified: true },
  ] });
export const detail = (): ReviewDetail => ({ proposal_id: proposal, repository_id: '00000000-0000-4000-8000-000000000002',
  team_id: '00000000-0000-4000-8000-000000000003', author_id: actor, owner_id: actor, number: 25, title: 'Synthetic review',
  branch: 'change/25-review', target_branch: 'main', head_sha: head, base_sha: base, target_sha: target, state: 'DRAFT', version: 4,
  updated_at: '2026-11-18T00:00:00Z', owner_name: 'Synthetic owner', team_name: 'Synthetic team', description: 'Saved synthetic proposal',
  initial_mode: 'manual', ui_impact: 'no', ui_impact_reason: 'No UI changes', links: [],
  capabilities: { can_edit: true, can_close: true, can_reopen: false },
  synchronization: { state: 'in_sync', actual_head_sha: head, current_main_sha: target, base_outdated: true } });
export const ticket: ReviewAccepted = { proposal_id: proposal, job_id: '00000000-0000-4000-8000-000000000040',
  operation_id: '00000000-0000-4000-8000-000000000050' };
export const publication = (): ReviewPublication => ({ ...ticket, snapshot_id: snapshot, fingerprint: 'd'.repeat(64), evidence_digest: 'e'.repeat(64),
  proposal_version: 5, activated: true, replayed: false });
export const job = (patch: Partial<ReviewJob> = {}): ReviewJob => ({ job_id: ticket.job_id, proposal_id: proposal, kind: 'SUBMIT_REVIEW', state: 'QUEUED',
  attempts: 0, phase: 'QUEUED', error_code: null, ...patch });
export const metadata = (): ReviewSnapshot => ({ snapshot_id: snapshot, proposal_id: proposal, fingerprint: 'd'.repeat(64), evidence_digest: 'e'.repeat(64),
  media_validation_version: 1, title: 'Synthetic review', proposal_number: 25, delivery_status: 'PLANNED', created_at: '2026-11-18T00:00:00Z',
  created_by: actor, current_metadata: true, file_count: 1, media_count: 0, metadata: { schema_version: 1,
    summary: { proposal_id: proposal, repository_id: detail().repository_id, branch: 'change/25-review', fingerprint: 'c'.repeat(64),
      head_sha: head, target_sha: target, merge_base_sha: base, recorded_base_sha: base, comparison_content_id: 'f'.repeat(64),
      expected_proposal_version: 4, policy_version: 1, configuration_version: 2, ui_impact: 'NO', base_outdated: true,
      changed_files: 1, content_findings_total: 0, media_checks_required: 0 },
    context: { author_id: actor, owner_id: actor, team_id: detail().team_id, description: 'Saved synthetic proposal', ui_impact: 'NO', ui_impact_reason: 'No UI changes' },
    assignments: configuration().settings.assignments,
    validation: { complete: true, blockers_total: 0, findings: [], findings_total: 0, findings_truncated: false },
  } });
export const history = (): ReviewHistory => ({ proposal_id: proposal, number: 25, title: 'Synthetic review', branch: 'change/25-review',
  target_branch: 'main', active_snapshot_id: snapshot, current_proposal_version: 5,
  current_proposal_state: 'IN_REVIEW', next_after: null, snapshots: [{ snapshot_id: snapshot, fingerprint: 'd'.repeat(64), title: 'Synthetic review',
    branch: 'change/25-review', head_sha: head, target_sha: target, prepared_version: 4, configuration_version: 2, policy_version: 1,
    file_count: 1, media_count: 0, created_at: '2026-11-18T00:00:00Z', created_by: actor }] });
export const files = (): ReviewFiles => ({ snapshot_id: snapshot, offset: 0, total: 1, next_offset: null, files: [{ id: '1'.repeat(64), kind: 'document',
  status: 'modified', matched_by: 'document_id', content_changed: true, mode_changed: false, catalog_position_changed: false,
  before: { path: 'docs/a.md', blob_oid: base, bytes: 10, executable: false, sha256: '2'.repeat(64), document_id: 'DOC-A', catalog_position: 0 },
  after: { path: 'docs/a.md', blob_oid: head, bytes: 20, executable: false, sha256: '3'.repeat(64), document_id: 'DOC-A', catalog_position: 0 },
}] });
export const media = (): ReviewMedia => ({ snapshot_id: snapshot, offset: 0, total: 0, next_offset: null, receipts: [] });
