import type { components } from '../../core/api/generated/schema';
import type { ReadableFileResponse, ReadablePage, ReadableRow, ReadableUnitSummary } from '../../core/api/comparison-api';
import { changeIdentity, digest, errorCode, integer, sha, uuid, validChange } from './comparison-evidence';
import type { RawScope } from './raw-state';

type SideSummary = components['schemas']['ReadableSideSummary'];
export function scopeKey(scope: RawScope): string | null {
  if (![scope.actor, scope.proposal, scope.snapshot].every(uuid) || !digest(scope.content) || !sha(scope.head) || !sha(scope.base)
    || !validChange(scope.file) || !['document', 'catalog'].includes(scope.file.kind)) return null;
  return JSON.stringify([scope.actor, scope.proposal, scope.snapshot, scope.content, scope.head, scope.base, changeIdentity(scope.file)]);
}
export function sameFile(value: Pick<ReadableFileResponse, 'proposal_id' | 'snapshot_id' | 'content_id' | 'head_sha' | 'merge_base_sha' | 'file'>,
  scope: RawScope): boolean {
  return !!value && value.proposal_id === scope.proposal && value.snapshot_id === scope.snapshot && value.content_id === scope.content
    && value.head_sha === scope.head && value.merge_base_sha === scope.base && validChange(value.file)
    && changeIdentity(value.file) === changeIdentity(scope.file);
}
export function unitIdentity(unit: ReadableUnitSummary): string {
  return JSON.stringify([unit.index, unit.kind, unit.source_start_byte, unit.body_start_byte, unit.source_end_byte, unit.raw_sha256,
    unit.semantic_sha256 ?? null, unit.heading_id ?? null, unit.protected_reason ?? null, unit.section_ids_total, unit.code_blocks_total, unit.events_total]);
}
export function validUnit(unit: ReadableUnitSummary, side: SideSummary): boolean {
  return !!unit && side.state === 'READY' && integer(unit.index, side.units_total - 1)
    && ['metadata', 'heading', 'paragraph', 'list', 'quote', 'code', 'table', 'protected', 'trailing'].includes(unit.kind)
    && integer(unit.source_start_byte, side.source_bytes) && integer(unit.body_start_byte, side.source_bytes, unit.source_start_byte)
    && integer(unit.source_end_byte, side.source_bytes, unit.body_start_byte) && digest(unit.raw_sha256)
    && (unit.semantic_sha256 == null || digest(unit.semantic_sha256))
    && (unit.heading_id == null || typeof unit.heading_id === 'string' && unit.heading_id.length <= 120)
    && (unit.protected_reason == null || typeof unit.protected_reason === 'string' && unit.protected_reason.length <= 2000)
    && integer(unit.section_ids_total, 10000) && integer(unit.code_blocks_total, 32000) && integer(unit.events_total, 96000)
    && (unit.protected_reason == null ? digest(unit.semantic_sha256) : unit.semantic_sha256 == null);
}
function validSide(side: SideSummary, source: RawScope['file']['before']): boolean {
  if (!side || !integer(side.source_bytes, 2 * 1024 * 1024) || !integer(side.units_total, 10002)) return false;
  if (!source) return side.state === 'ABSENT' && side.sha256 == null && side.document_id == null && side.source_bytes === 0
    && side.units_total === 0 && side.error_code == null;
  if (side.sha256 !== source.sha256 || side.source_bytes !== source.bytes) return false;
  if (side.state === 'RAW_REQUIRED') return side.units_total === 0 && side.document_id == null && errorCode(side.error_code);
  return side.state === 'READY' && side.error_code == null && side.units_total > 0
    && typeof side.document_id === 'string' && /^[A-Z0-9-]{1,120}$/.test(side.document_id)
    && (source.document_id == null || source.document_id === side.document_id);
}
export function rowIdentity(row: ReadableRow): string {
  const c = row.change;
  return JSON.stringify([c.id, c.before_index ?? null, c.after_index ?? null, c.status, c.matched_by ?? null,
    c.raw_changed, c.render_changed ?? null, c.position_changed, row.before ? unitIdentity(row.before) : null, row.after ? unitIdentity(row.after) : null]);
}
function validRow(row: ReadableRow, page: ReadablePage): boolean {
  if (!row?.change || !digest(row.change.id) || !row.before && !row.after) return false;
  const a = row.before, b = row.after, c = row.change;
  if (a && !validUnit(a, page.before) || b && !validUnit(b, page.after)
    || (c.before_index ?? null) !== (a?.index ?? null) || (c.after_index ?? null) !== (b?.index ?? null)) return false;
  const raw = (a?.raw_sha256 ?? null) !== (b?.raw_sha256 ?? null);
  const rendered = a && b ? a.semantic_sha256 != null && b.semantic_sha256 != null ? a.semantic_sha256 !== b.semantic_sha256 : null
    : (a ?? b)?.semantic_sha256 != null ? true : null;
  if (c.status !== (!a ? 'added' : !b ? 'deleted' : raw ? 'modified' : 'unchanged') || c.raw_changed !== raw
    || (c.render_changed ?? null) !== rendered || c.position_changed !== (!!a && !!b && a.index !== b.index)) return false;
  if (!a || !b) return c.matched_by == null;
  if (a.kind !== b.kind || !['structural', 'heading_id', 'exact_source', 'rendered_content', 'section_position'].includes(c.matched_by ?? '')) return false;
  if (c.matched_by === 'heading_id') return a.kind === 'heading' && !!a.heading_id && a.heading_id === b.heading_id;
  if (c.matched_by === 'exact_source') return !raw;
  if (c.matched_by === 'rendered_content') return a.semantic_sha256 != null && rendered === false;
  return true;
}
export function pageIdentity(page: ReadablePage): string {
  const side = (s: SideSummary) => [s.state, s.sha256 ?? null, s.document_id ?? null, s.source_bytes, s.units_total, s.error_code ?? null];
  return JSON.stringify([page.ready, page.raw_changed, page.total, side(page.before), side(page.after)]);
}
export function validPage(response: ReadableFileResponse, scope: RawScope, offset: number | null): boolean {
  if (!sameFile(response, scope)) return false;
  const page = response.page;
  if (!page || !validSide(page.before, scope.file.before) || !validSide(page.after, scope.file.after)
    || page.ready !== ![page.before.state, page.after.state].includes('RAW_REQUIRED')
    || page.raw_changed !== ((scope.file.before?.sha256 ?? null) !== (scope.file.after?.sha256 ?? null))
    || !integer(page.total, page.before.units_total + page.after.units_total) || (offset == null ? !integer(page.offset, 20004) || page.offset % 50 !== 0 : page.offset !== offset)
    || !Array.isArray(page.rows) || page.rows.length !== Math.min(50, Math.max(0, page.total - page.offset))) return false;
  if (!page.ready && page.total !== 0 || page.ready && page.total < Math.max(page.before.units_total, page.after.units_total)) return false;
  const next = page.offset + page.rows.length;
  if ((page.next_offset ?? null) !== (next < page.total ? next : null)) return false;
  const ids = new Set<string>(), before = new Set<number>(), after = new Set<number>();
  return page.rows.every(row => {
    if (!validRow(row, page) || ids.has(row.change.id) || row.before && before.has(row.before.index) || row.after && after.has(row.after.index)) return false;
    ids.add(row.change.id); if (row.before) before.add(row.before.index); if (row.after) after.add(row.after.index); return true;
  });
}
