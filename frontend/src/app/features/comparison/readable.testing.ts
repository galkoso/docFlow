import contract from '../../../../../fixtures/readable-contract.json';
import type { EventWindow, ReadableFileResponse, ReadablePage, ReadableUnitResponse, ReadableUnitSummary } from '../../core/api/comparison-api';
import { rawScope } from './raw.testing';
import type { RawScope } from './raw-state';
import type { UnitScope } from './readable-window';

export const readableContract = contract as unknown as { page: ReadablePage;
  units: Array<{ side: 'before' | 'after'; unit: ReadableUnitSummary; windows: EventWindow[] }> };
export function readableScope(): RawScope {
  const scope = rawScope();
  for (const side of ['before', 'after'] as const) {
    const summary = readableContract.page[side]; Object.assign(scope.file[side]!, {
      bytes: summary.source_bytes, sha256: summary.sha256, document_id: summary.document_id });
  }
  return scope;
}
export function readableResponse(): ReadableFileResponse {
  const scope = readableScope();
  return { proposal_id: scope.proposal, snapshot_id: scope.snapshot, content_id: scope.content, head_sha: scope.head,
    merge_base_sha: scope.base, file: scope.file, page: structuredClone(readableContract.page) };
}
export function unitScope(kind = 'paragraph', side: 'before' | 'after' = 'before'): UnitScope {
  return { file: readableScope(), side, unit: structuredClone(readableContract.units.find(item => item.side === side && item.unit.kind === kind)!.unit) };
}
export function unitResponse(scope: UnitScope, page = 0): ReadableUnitResponse {
  const source = readableContract.units.find(item => item.side === scope.side && item.unit.index === scope.unit.index)!;
  const file = scope.file;
  return { proposal_id: file.proposal, snapshot_id: file.snapshot, content_id: file.content, head_sha: file.head,
    merge_base_sha: file.base, file: file.file, side: scope.side, unit_index: scope.unit.index, unit: scope.unit,
    state: source.windows.length ? 'READY' : 'RAW_REQUIRED', error_code: source.windows.length ? null : 'READABLE_PROTECTED_SOURCE',
    window: source.windows.length ? structuredClone(source.windows[page]) : null };
}
