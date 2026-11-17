import type { ComparisonChange, ComparisonRequest, ComparisonSummary, DiffResponse } from '../../core/api/comparison-api';

export const PAGE_SIZE = 50;
export const uuid = (v: unknown): v is string => typeof v === 'string'
  && /^[a-f0-9]{8}-(?:[a-f0-9]{4}-){3}[a-f0-9]{12}$/.test(v) && v !== '00000000-0000-0000-0000-000000000000';
export const sha = (v: unknown): v is string => typeof v === 'string' && /^(?:[a-f0-9]{2}){20,64}$/.test(v);
export const digest = (v: unknown): v is string => typeof v === 'string' && /^[a-f0-9]{64}$/.test(v);
export const integer = (v: unknown, max: number, min = 0): v is number => Number.isSafeInteger(v) && (v as number) >= min && (v as number) <= max;
export const errorCode = (v: unknown): v is string => typeof v === 'string' && /^[A-Z0-9_]{1,100}$/.test(v);
const states = ['DRAFT', 'IN_REVIEW', 'CHANGES_REQUESTED', 'APPROVED', 'MERGING', 'MERGED', 'CLOSED'];
const docId = (v: unknown): v is string => typeof v === 'string' && /^[A-Z0-9-]{1,120}$/.test(v);
function path(v: unknown): v is string {
  if (typeof v !== 'string' || !v || v !== v.normalize('NFC') || Array.from(v).length > 240 || /[\u0000-\u001f\u007f-\u009f\\:]/.test(v)) return false;
  const parts = v.split('/');
  return parts.length <= 10 && parts.every(part => !!part && !['.', '..', '.git'].includes(part.toLowerCase()));
}
const kind = (p: string) => p === '.specspace/catalog.json' ? 'catalog' : p.startsWith('docs/') && p.endsWith('.md') ? 'document'
  : p.startsWith('assets/') ? 'asset' : 'other';
export function validChange(v: ComparisonChange): boolean {
  if (!v || !digest(v.id) || !['document', 'catalog', 'asset', 'other'].includes(v.kind)
    || !['added', 'deleted', 'modified', 'renamed'].includes(v.status) || !v.before && !v.after) return false;
  for (const side of [v.before, v.after]) {
    if (!side) continue;
    if (!path(side.path) || kind(side.path) !== v.kind || !sha(side.blob_oid) || !integer(side.bytes, Number.MAX_SAFE_INTEGER)
      || typeof side.executable !== 'boolean' || side.sha256 != null && !digest(side.sha256)
      || side.document_id != null && !docId(side.document_id) || side.catalog_position != null && !integer(side.catalog_position, 999)) return false;
    if (['document', 'catalog'].includes(v.kind) && (!digest(side.sha256) || side.bytes > 2 * 1024 * 1024)) return false;
  }
  const a = v.before, b = v.after;
  if (v.status === 'added' && (!!a || !b) || v.status === 'deleted' && (!a || !!b)
    || ['modified', 'renamed'].includes(v.status) && (!a || !b)) return false;
  if (v.status === 'renamed' && a?.path === b?.path || v.status === 'modified' && a?.path !== b?.path) return false;
  const content = !a || !b || a.blob_oid !== b.blob_oid || a.bytes !== b.bytes;
  const mode = !!a && !!b && a.executable !== b.executable;
  const position = !!a && !!b && (a.catalog_position ?? null) !== (b.catalog_position ?? null);
  if (v.content_changed !== content || v.mode_changed !== mode || v.catalog_position_changed !== position) return false;
  if (v.status === 'modified' && !content && !mode && !position) return false;
  if (!a || !b) return v.matched_by == null;
  if (v.matched_by === 'document_id') return v.kind === 'document' && !!a.document_id && a.document_id === b.document_id;
  if (v.matched_by === 'path') return a.path === b.path;
  return v.matched_by === 'exact_blob' && !content;
}
export function changeIdentity(v: ComparisonChange): string {
  const side = (s: ComparisonChange['before']) => s ? [s.path, s.blob_oid, s.bytes, s.executable,
    s.sha256 ?? null, s.document_id ?? null, s.catalog_position ?? null] : null;
  return JSON.stringify([v.id, v.kind, v.status, v.matched_by ?? null, v.content_changed, v.mode_changed,
    v.catalog_position_changed, side(v.before), side(v.after)]);
}
function validSummary(v: ComparisonSummary, proposal: string): boolean {
  if (!v || v.proposal_id !== proposal || !uuid(v.repository_id) || !digest(v.content_id)
    || !integer(v.proposal_version, Number.MAX_SAFE_INTEGER, 1) || !states.includes(v.proposal_state)
    || typeof v.branch !== 'string' || !/^change\/[\x21-\x7e]+$/.test(v.branch) || v.branch.length > 100) return false;
  if (![v.head_sha, v.target_sha, v.merge_base_sha, v.observed_main_sha, v.recorded_head_sha, v.recorded_base_sha].every(sha)
    || v.observed_branch_sha != null && !sha(v.observed_branch_sha)
    || !integer(v.before_file_count, 20000) || !integer(v.after_file_count, 20000) || !integer(v.changes_total, 40000)
    || v.changes_total > v.before_file_count + v.after_file_count) return false;
  if (v.historical_head !== (v.head_sha !== v.recorded_head_sha) || v.target_outdated !== (v.target_sha !== v.observed_main_sha)
    || v.base_outdated !== (v.recorded_base_sha !== v.observed_main_sha)
    || v.external_branch_change !== ((v.observed_branch_sha ?? null) !== v.recorded_head_sha)) return false;
  return [[v.before_findings, v.before_findings_total], [v.after_findings, v.after_findings_total]].every(([items, count]) => {
    if (!Array.isArray(items) || items.length > 1000 || !integer(count, Number.MAX_SAFE_INTEGER) || count < items.length) return false;
    return items.every(item => item && errorCode(item.code) && path(item.path));
  });
}
/** Stable across JSON object-key order; no current mutable proposal fields. */
export function summaryIdentity(v: ComparisonSummary): string {
  return JSON.stringify([v.content_id, v.proposal_id, v.repository_id, v.proposal_version, v.proposal_state, v.branch,
    v.head_sha, v.target_sha, v.merge_base_sha, v.observed_main_sha, v.observed_branch_sha ?? null, v.recorded_head_sha, v.recorded_base_sha,
    v.historical_head, v.target_outdated, v.base_outdated, v.external_branch_change, v.before_file_count, v.after_file_count, v.changes_total,
    v.before_findings_total, v.before_findings.map(f => [f.code, f.path]), v.after_findings_total, v.after_findings.map(f => [f.code, f.path])]);
}
export interface ExpectedComparison {
  proposal: string; snapshot: string | null; job: string | null; summary: string | null; request: Readonly<ComparisonRequest> | null; offset: number;
}
export function validDiff(v: DiffResponse, expected: ExpectedComparison): boolean {
  if (!v || v.proposal_id !== expected.proposal || !uuid(v.snapshot_id) || !uuid(v.job_id)
    || expected.snapshot != null && v.snapshot_id !== expected.snapshot || expected.job != null && v.job_id !== expected.job
    || !integer(v.attempts, 2147483647) || !sha(v.current_head_sha) || !states.includes(v.current_proposal_state)
    || !integer(v.current_proposal_version, Number.MAX_SAFE_INTEGER, 1) || v.error_code != null && !errorCode(v.error_code)) return false;
  if (v.state === 'PENDING') {
    return v.summary == null && v.page == null && v.completed_at == null && v.historical_head == null && v.context_changed === false
      && (v.job_state === 'QUEUED' && ['QUEUED', 'RETRY_WAIT'].includes(v.phase)
        || v.job_state === 'RUNNING' && ['LOADING', 'PUBLISHING'].includes(v.phase));
  }
  if (typeof v.completed_at !== 'string' || !/^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d{1,9})?Z$/.test(v.completed_at)
    || !Number.isFinite(Date.parse(v.completed_at))) return false;
  if (v.state === 'FAILED') return ['FAILED', 'CANCELLED'].includes(v.job_state) && v.phase === 'FAILED' && errorCode(v.error_code)
    && v.summary == null && v.page == null && v.historical_head == null && v.context_changed === false;
  if (v.state !== 'READY' || v.job_state !== 'SUCCEEDED' || v.phase !== 'READY' || v.error_code != null
    || !v.summary || !validSummary(v.summary, expected.proposal) || !v.page) return false;
  const s = v.summary, page = v.page;
  if (expected.summary != null && summaryIdentity(s) !== expected.summary
    || expected.request && s.head_sha !== (expected.request.head ?? s.recorded_head_sha)
    || expected.request?.target && s.target_sha !== expected.request.target) return false;
  if (v.historical_head !== (s.head_sha !== v.current_head_sha) || v.context_changed !== (s.proposal_version !== v.current_proposal_version
    || s.recorded_head_sha !== v.current_head_sha || s.proposal_state !== v.current_proposal_state)) return false;
  if (page.snapshot_id !== v.snapshot_id || page.content_id !== s.content_id || page.total !== s.changes_total || page.offset !== expected.offset
    || !Array.isArray(page.files) || page.files.length !== Math.min(PAGE_SIZE, Math.max(0, page.total - page.offset))) return false;
  const next = page.offset + page.files.length;
  return (page.next_offset ?? null) === (next < page.total ? next : null) && page.files.every(validChange)
    && new Set(page.files.map(file => file.id)).size === page.files.length;
}
