import type { ReviewFiles, ReviewHistory, ReviewMedia, ReviewSnapshot } from '../../core/api/review-api';
import { digest, errorCode, integer, sha, uuid, validChange } from '../comparison/comparison-evidence';

export const text = (v: unknown, max: number, min = 0): v is string => typeof v === 'string'
  && Array.from(v).length >= min && Array.from(v).length <= max && !v.includes('\0');
const positive = (v: unknown): v is number => integer(v, Number.MAX_SAFE_INTEGER, 1);
const branch = (v: unknown): v is string => typeof v === 'string' && /^change\/[\x21-\x7e]{1,93}$/.test(v);
const states = ['PROVISIONING', 'DRAFT', 'IN_REVIEW', 'CHANGES_REQUESTED', 'APPROVED', 'MERGING', 'MERGED', 'CLOSED'];
export const utc = (v: unknown): v is string => typeof v === 'string' && v.length <= 64
  && /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{1,9})?(?:Z|\+00:00)$/.test(v) && Number.isFinite(Date.parse(v));
// Preserve sub-millisecond PostgreSQL ordering; Date.parse alone loses it.
const order = (v: string): string => { const [base, fraction = ''] = v.replace(/(?:Z|\+00:00)$/, '').split('.'); return `${base}.${fraction.padEnd(9, '0')}`; };
export function validHistory(v: ReviewHistory, proposal: string, after?: string): boolean {
  if (!v || v.proposal_id !== proposal || !positive(v.number) || !text(v.title, 120, 3) || !branch(v.branch) || !text(v.target_branch, 1024, 1)
    || !positive(v.current_proposal_version) || !states.includes(v.current_proposal_state)
    || v.active_snapshot_id != null && !uuid(v.active_snapshot_id) || !Array.isArray(v.snapshots) || v.snapshots.length > 20) return false;
  let previous: { time: string; id: string } | null = null;
  const ids = new Set<string>();
  for (const item of v.snapshots) {
    if (!item || !uuid(item.snapshot_id) || ids.has(item.snapshot_id) || item.snapshot_id === after || !digest(item.fingerprint)
      || !text(item.title, 120, 3) || !branch(item.branch) || !sha(item.head_sha) || !sha(item.target_sha)
      || !positive(item.prepared_version) || item.prepared_version > v.current_proposal_version
      || !positive(item.configuration_version) || item.configuration_version > item.prepared_version || !positive(item.policy_version)
      || !integer(item.file_count, 40000, 1) || !integer(item.media_count, 20000) || !utc(item.created_at) || !uuid(item.created_by)) return false;
    const time = order(item.created_at);
    if (previous && (time > previous.time || time === previous.time && item.snapshot_id >= previous.id)) return false;
    previous = { time, id: item.snapshot_id }; ids.add(item.snapshot_id);
  }
  return v.next_after == null || v.snapshots.length === 20 && v.next_after === v.snapshots.at(-1)?.snapshot_id;
}
export function validSnapshot(v: ReviewSnapshot, proposal: string, snapshot: string): boolean {
  if (!v || v.snapshot_id !== snapshot || v.proposal_id !== proposal || !digest(v.fingerprint) || !digest(v.evidence_digest)
    || !integer(v.media_validation_version, 2147483647, 1) || !text(v.title, 120, 3) || !positive(v.proposal_number)
    || !['PLANNED', 'IMPLEMENTED', 'RELEASED'].includes(v.delivery_status) || !utc(v.created_at) || !uuid(v.created_by)
    || !integer(v.file_count, 40000, 1) || !integer(v.media_count, 20000) || typeof v.current_metadata !== 'boolean') return false;
  const data = v.metadata, summary = data?.summary, context = data?.context, validation = data?.validation;
  if (!data || data.schema_version !== 1 || !summary || !context || !validation || summary.proposal_id !== proposal
    || !uuid(summary.repository_id) || !digest(summary.fingerprint) || !digest(summary.comparison_content_id) || !branch(summary.branch)
    || ![summary.head_sha, summary.target_sha, summary.merge_base_sha, summary.recorded_base_sha].every(sha)
    || !positive(summary.expected_proposal_version) || !positive(summary.configuration_version) || !positive(summary.policy_version)
    || summary.configuration_version > summary.expected_proposal_version || summary.changed_files !== v.file_count
    || summary.media_checks_required !== v.media_count || summary.base_outdated !== (summary.recorded_base_sha !== summary.target_sha)
    || !['YES', 'NO', 'UNDECIDED'].includes(summary.ui_impact) || context.ui_impact !== summary.ui_impact
    || ![context.author_id, context.owner_id, context.team_id].every(uuid) || !text(context.description, 10000, 10)
    || context.ui_impact_reason != null && !text(context.ui_impact_reason, 10000)
    || context.ui_impact === 'NO' && !context.ui_impact_reason?.trim()) return false;
  if (!Array.isArray(data.assignments) || !data.assignments.length || data.assignments.length > 200) return false;
  const selected = new Set<string>();
  for (const item of data.assignments) {
    if (!item || !uuid(item.reviewer_id) || item.reviewer_id === context.author_id || !['PRODUCT', 'DEVELOPMENT', 'DESIGN'].includes(item.domain)
      || typeof item.required !== 'boolean' || selected.has(`${item.reviewer_id}:${item.domain}`)) return false;
    selected.add(`${item.reviewer_id}:${item.domain}`);
  }
  if (!data.assignments.some(product => product.domain === 'PRODUCT' && data.assignments.some(development =>
    development.domain === 'DEVELOPMENT' && product.reviewer_id !== development.reviewer_id))
    || summary.ui_impact === 'YES' && !data.assignments.some(item => item.domain === 'DESIGN')) return false;
  if (!validation.complete || validation.blockers_total !== 0 || !Array.isArray(validation.findings) || validation.findings.length > 1000
    || !integer(validation.findings_total, Number.MAX_SAFE_INTEGER) || validation.findings_total < validation.findings.length
    || summary.content_findings_total !== validation.findings_total
    || validation.findings_truncated !== (validation.findings_total > validation.findings.length)) return false;
  return validation.findings.every(finding => finding && errorCode(finding.code) && text(finding.path, 240)
    && typeof finding.path_truncated === 'boolean' && finding.severity === 'warning'
    && [finding.source_start_byte, finding.source_end_byte].every(value => value == null || integer(value, 2 * 1024 * 1024))
    && (finding.source_start_byte == null || finding.source_end_byte == null || finding.source_start_byte <= finding.source_end_byte));
}
export function validFiles(v: ReviewFiles, metadata: ReviewSnapshot, offset: number): boolean {
  if (!v || v.snapshot_id !== metadata.snapshot_id || v.offset !== offset || v.total !== metadata.file_count || !Array.isArray(v.files)
    || v.files.length !== Math.min(50, Math.max(0, v.total - offset))) return false;
  const end = offset + v.files.length;
  return (v.next_offset ?? null) === (end < v.total ? end : null) && new Set(v.files.map(file => file?.id)).size === v.files.length && v.files.every(validChange);
}
export function validMedia(v: ReviewMedia, metadata: ReviewSnapshot, offset: number): boolean {
  if (!v || v.snapshot_id !== metadata.snapshot_id || v.offset !== offset || v.total !== metadata.media_count || !Array.isArray(v.receipts)
    || v.receipts.length !== Math.min(50, Math.max(0, v.total - offset))) return false;
  const end = offset + v.receipts.length;
  if ((v.next_offset ?? null) !== (end < v.total ? end : null)) return false;
  let previous = '';
  return v.receipts.every(receipt => {
    if (!receipt || !/^assets\/[a-f0-9]{64}\.(png|jpg|jpeg|webp)$/.test(receipt.path) || receipt.path <= previous
      || !sha(receipt.blob_oid) || ![40, 64].includes(receipt.blob_oid.length) || !digest(receipt.source_sha256) || !digest(receipt.display_sha256)
      || receipt.path.slice(7, 71) !== receipt.source_sha256 || !integer(receipt.bytes, 5 * 1024 * 1024)
      || !integer(receipt.display_width, 2048, 1) || !integer(receipt.display_height, 2048, 1)) return false;
    previous = receipt.path; return true;
  });
}
