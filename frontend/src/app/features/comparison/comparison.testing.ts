import type { ComparisonChange, DiffResponse } from '../../core/api/comparison-api';

export const id = (n: number) => `00000000-0000-4000-8000-${String(n).padStart(12, '0')}`;
export const actor = id(1), proposal = id(2), snapshot = id(3), job = id(4), repository = id(5);
export const head = '2'.repeat(40), base = '1'.repeat(40), target = '3'.repeat(40);
export function file(n = 0): ComparisonChange {
  return { id: n.toString(16).padStart(64, '0'), kind: 'document', status: 'modified', matched_by: 'document_id',
    before: { path: `docs/${n}.md`, bytes: 3, blob_oid: 'a'.repeat(40), sha256: 'a'.repeat(64), executable: false,
      document_id: `DOC-${n}`, catalog_position: n },
    after: { path: `docs/${n}.md`, bytes: 3, blob_oid: 'b'.repeat(40), sha256: 'b'.repeat(64), executable: false,
      document_id: `DOC-${n}`, catalog_position: n }, content_changed: true, mode_changed: false, catalog_position_changed: false };
}
export function ready(total = 1, offset = 0): DiffResponse {
  return { proposal_id: proposal, snapshot_id: snapshot, job_id: job, state: 'READY', job_state: 'SUCCEEDED', phase: 'READY', attempts: 1,
    error_code: null, completed_at: '2026-11-17T00:00:00.000Z', current_head_sha: head, current_proposal_version: 7,
    current_proposal_state: 'DRAFT', context_changed: false, historical_head: false,
    summary: { proposal_id: proposal, repository_id: repository, content_id: 'c'.repeat(64), proposal_version: 7,
      proposal_state: 'DRAFT', branch: 'change/one', head_sha: head, target_sha: target, merge_base_sha: base,
      observed_main_sha: target, observed_branch_sha: head, recorded_head_sha: head, recorded_base_sha: base,
      historical_head: false, target_outdated: false, base_outdated: true, external_branch_change: false,
      before_file_count: 200, after_file_count: 200, changes_total: total, before_findings: [], before_findings_total: 0,
      after_findings: [], after_findings_total: 0 },
    page: { snapshot_id: snapshot, content_id: 'c'.repeat(64), offset, total,
      next_offset: offset + 50 < total ? offset + 50 : null,
      files: Array.from({ length: Math.min(50, Math.max(0, total - offset)) }, (_, n) => file(offset + n)) } };
}
export function pending(phase = 'QUEUED'): DiffResponse {
  return { ...ready(), state: 'PENDING', job_state: ['LOADING', 'PUBLISHING'].includes(phase) ? 'RUNNING' : 'QUEUED', phase,
    attempts: 0, completed_at: null, summary: null, page: null, historical_head: null };
}
export function failed(): DiffResponse {
  return { ...pending(), state: 'FAILED', job_state: 'FAILED', phase: 'FAILED', attempts: 1,
    completed_at: '2026-11-17T00:00:00.000Z', error_code: 'COMPARISON_SETUP_REQUIRED' };
}

