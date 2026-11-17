import type { EventCursor, EventWindow, ReadableUnitResponse, ReadableUnitSummary } from '../../core/api/comparison-api';
import type { RenderEvent } from '../specification/render-model';
import { integer } from './comparison-evidence';
import { sameFile, unitIdentity } from './readable-evidence';
import type { RawScope } from './raw-state';

export interface UnitScope { file: RawScope; side: 'before' | 'after'; unit: ReadableUnitSummary; }
const bytes = (value: string) => new TextEncoder().encode(value).length;
const tags = ['paragraph', 'heading', 'quote', 'list', 'item', 'table', 'table_head', 'table_row', 'table_cell',
  'emphasis', 'strong', 'strike', 'code_block', 'inline_code', 'link'];
const kinds = ['open', 'close', 'text', 'break', 'rule', 'image', 'unsupported', 'task_marker'];
export const cursorIdentity = (cursor: EventCursor) => `${cursor.event}:${cursor.offset}`;
export const eventIdentity = (event: RenderEvent) => JSON.stringify([event.kind, event.tag ?? null, event.text ?? null,
  event.source_start_byte, event.source_end_byte, event.block_id ?? null, event.offset_utf16 ?? null, event.section_id ?? null,
  event.level ?? null, event.list_start ?? null, event.destination ?? null, event.title ?? null, event.alignments ?? [], event.checked ?? null,
  event.hard_break ?? null]);
export const stackIdentity = (stack: RenderEvent[]) => JSON.stringify(stack.map(eventIdentity));
function eventValid(event: RenderEvent, unit: ReadableUnitSummary): boolean {
  if (!event || !kinds.includes(event.kind) || !integer(event.source_start_byte, unit.source_end_byte, unit.body_start_byte)
    || !integer(event.source_end_byte, unit.source_end_byte, event.source_start_byte)) return false;
  if (['open', 'close'].includes(event.kind) ? !tags.includes(event.tag ?? '') : event.tag != null) return false;
  for (const text of [event.text, event.block_id, event.section_id, event.destination, event.title]) {
    if (text != null && (typeof text !== 'string' || text.length > 256 * 1024)) return false;
  }
  if (event.kind === 'text' && typeof event.text !== 'string' || event.kind === 'open' && event.tag === 'heading' && !integer(event.level, 6, 1)
    || event.list_start != null && !integer(event.list_start, Number.MAX_SAFE_INTEGER)
    || event.offset_utf16 != null && !integer(event.offset_utf16, 8 * 1024 * 1024)
    || event.checked != null && typeof event.checked !== 'boolean' || event.hard_break != null && typeof event.hard_break !== 'boolean'
    || event.alignments != null && (!Array.isArray(event.alignments) || event.alignments.length > 10000
      || event.alignments.some(alignment => !['none', 'left', 'center', 'right'].includes(alignment)))) return false;
  return true;
}
function validCursor(cursor: EventCursor, total: number): boolean {
  return !!cursor && integer(cursor.event, total) && integer(cursor.offset, 8 * 1024 * 1024) && (cursor.event < total || cursor.offset === 0);
}
function activeItem(stack: RenderEvent[], depth: number): boolean {
  for (const event of stack.slice(depth + 1)) { if (event.tag === 'list') break; if (event.tag === 'item') return true; }
  return false;
}
/** Validate and balance a standalone page using known inert events only. Source
 * markup, destinations and language info are never interpreted as HTML. */
export function windowEvents(window: EventWindow, unit: ReadableUnitSummary): RenderEvent[] | null {
  if (!Array.isArray(window.start_stack) || !Array.isArray(window.end_stack) || window.start_stack.length > 64 || window.end_stack.length > 64
    || !Array.isArray(window.fragments) || window.fragments.length > 128 || !Array.isArray(window.code_info) || window.code_info.length > 192
    || !Array.isArray(window.start_ordered_lists) || !Array.isArray(window.end_ordered_lists)
    || window.start_ordered_lists.length > 64 || window.end_ordered_lists.length > 64
    || !validCursor(window.cursor, unit.events_total) || window.next != null && !validCursor(window.next, unit.events_total)) return null;
  if (window.start_stack.some(event => !eventValid(event, unit) || event.kind !== 'open')
    || window.end_stack.some(event => !eventValid(event, unit) || event.kind !== 'open')) return null;
  if (window.cursor.event === 0 && window.start_stack.length !== 0) return null;
  const stack = [...window.start_stack], nextItems = new Map<RenderEvent, number>();
  function resumes(values: EventWindow['start_ordered_lists'], current: RenderEvent[], initialize: boolean): boolean {
    const wanted = current.flatMap((event, depth) => event.tag === 'list' && event.list_start != null ? [depth] : []);
    if (values.length !== wanted.length) return false;
    return values.every((value, index) => {
      const depth = wanted[index], event = current[depth];
      if (!value || value.stack_depth !== depth || !integer(value.item_start, Number.MAX_SAFE_INTEGER, event.list_start ?? 0)) return false;
      if (initialize) nextItems.set(event, value.item_start + Number(activeItem(current, depth)));
      else if (value.item_start !== nextItems.get(event)! - Number(activeItem(current, depth))) return false;
      return true;
    });
  }
  if (!resumes(window.start_ordered_lists, stack, true)) return null;
  let index = window.cursor.event, offset = window.cursor.offset, total = 0;
  for (const fragment of window.fragments) {
    const event = fragment?.event;
    if (!eventValid(event, unit) || fragment.event_index !== index || index >= unit.events_total
      || fragment.text_start_byte !== offset || !integer(fragment.text_end_byte, 8 * 1024 * 1024, offset)
      || event.kind !== 'text' && offset !== 0 || typeof event.text === 'string' && event.text.length > 32768
      || bytes(event.text ?? '') !== fragment.text_end_byte - offset) return null;
    total += bytes(event.text ?? '');
    if (event.kind === 'open') {
      if (stack.length >= 64) return null;
      if (event.tag === 'list' && event.list_start != null) nextItems.set(event, event.list_start);
      if (event.tag === 'item') {
        const list = [...stack].reverse().find(parent => parent.tag === 'list'); if (!list) return null;
        if (list.list_start != null) nextItems.set(list, nextItems.get(list)! + 1);
      }
      stack.push(event);
    } else if (event.kind === 'close') {
      const open = stack.pop(); if (!open || open.tag !== event.tag) return null; nextItems.delete(open);
    }
    ++index; offset = 0;
  }
  if (total !== window.returned_text_bytes || total > 32768 || stackIdentity(stack) !== stackIdentity(window.end_stack)
    || !resumes(window.end_ordered_lists, stack, false)) return null;
  const last = window.fragments.at(-1);
  if (window.next) {
    if (!last || window.next.event >= unit.events_total) return null;
    if (window.next.event === last.event_index) {
      if (last.event.kind !== 'text' || window.next.offset !== last.text_end_byte || last.text_end_byte <= last.text_start_byte) return null;
    } else if (window.next.event !== index || window.next.offset !== 0) return null;
  } else if (index !== unit.events_total || stack.length) return null;
  const info = new Set<number>(); let infoBytes = 0;
  for (const code of window.code_info) {
    if (!code || !integer(code.event_index, unit.events_total - 1) || info.has(code.event_index) || typeof code.info !== 'string'
      || code.info.length > 32768) return null;
    info.add(code.event_index); infoBytes += bytes(code.info);
  }
  if (infoBytes > 32768 || bytes(JSON.stringify(window.start_stack)) > 32768 || bytes(JSON.stringify(window.end_stack)) > 32768
    || bytes(JSON.stringify(window)) > 384 * 1024) return null;
  const events = window.start_stack.map((event, depth) => {
    const resume = window.start_ordered_lists.find(value => value.stack_depth === depth);
    return resume ? { ...event, list_start: resume.item_start } : event;
  });
  events.push(...window.fragments.map(fragment => fragment.event));
  events.push(...[...window.end_stack].reverse().map(event => ({ ...event, kind: 'close' as const })));
  return events;
}
export function validUnitResponse(response: ReadableUnitResponse, scope: UnitScope, cursor: EventCursor): RenderEvent[] | null {
  if (!sameFile(response, scope.file) || response.side !== scope.side || response.unit_index !== scope.unit.index
    || !response.unit || unitIdentity(response.unit) !== unitIdentity(scope.unit)) return null;
  if (response.state === 'RAW_REQUIRED') return response.window == null && typeof response.error_code === 'string'
    && (response.error_code === 'READABLE_PROTECTED_SOURCE' ? scope.unit.protected_reason != null
      : response.error_code === 'READABLE_WINDOW_TOO_LARGE' && scope.unit.protected_reason == null) ? [] : null;
  const window = response.window;
  if (response.state !== 'READY' || response.error_code != null || !window || scope.unit.protected_reason != null
    || !window.unit || !validCursor(window.cursor, scope.unit.events_total)
    || window.document_sha256 !== scope.file.file[scope.side]?.sha256 || unitIdentity(window.unit) !== unitIdentity(scope.unit)
    || cursorIdentity(window.cursor) !== cursorIdentity(cursor)) return null;
  return windowEvents(window, scope.unit);
}
