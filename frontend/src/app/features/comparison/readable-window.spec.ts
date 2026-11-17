import { renderTree } from '../specification/render-model';
import { readableContract, readableScope, unitResponse, unitScope } from './readable.testing';
import { validUnitResponse, windowEvents } from './readable-window';

describe('Rust to Angular readable event windows', () => {
  it('accepts every real engine window and builds a balanced inert render tree including resumed lists and code', () => {
    let continued = 0, codes = 0;
    for (const item of readableContract.units) {
      const scope = { file: readableScope(), side: item.side, unit: item.unit };
      if (!item.windows.length) {
        const result = unitResponse(scope); expect(validUnitResponse(result, scope, { event: 0, offset: 0 })).toEqual([]); continue;
      }
      for (const [page, window] of item.windows.entries()) {
        const events = validUnitResponse(unitResponse(scope, page), scope, window.cursor);
        expect(events, `${item.side}/${item.unit.kind}/${page}`).not.toBeNull(); expect(() => renderTree(events!)).not.toThrow();
        if (window.cursor.event || window.cursor.offset) continued++;
        if (window.code_info.length) codes++;
      }
    }
    expect(continued).toBeGreaterThan(3); expect(codes).toBeGreaterThan(2);
  });

  it('rejects wrong source identity, unit metadata, cursor, unbalanced events and invented list numbering', () => {
    const scope = unitScope('list'), corrupt: Array<(v: ReturnType<typeof unitResponse>) => void> = [
      v => v.snapshot_id = '00000000-0000-4000-8000-000000000009', v => v.side = 'after',
      v => v.unit_index++, v => v.window!.document_sha256 = '9'.repeat(64), v => v.window!.unit.raw_sha256 = '9'.repeat(64),
      v => v.window!.cursor.event++, v => v.window!.fragments[0].event.kind = 'close',
      v => v.window!.fragments[0].text_end_byte = 1, v => v.window!.next = { event: 0, offset: 1 },
      v => v.window!.end_stack.pop(), v => v.window!.end_ordered_lists[0].item_start++,
      v => v.window!.returned_text_bytes++, v => v.window!.fragments[0].event.source_end_byte = Number.MAX_SAFE_INTEGER,
    ];
    for (const mutate of corrupt) {
      const value = structuredClone(unitResponse(scope)); mutate(value);
      expect(validUnitResponse(value, scope, { event: 0, offset: 0 })).toBeNull();
    }
  });

  it('rejects missing nested evidence and unsupported raw-fallback claims without throwing', () => {
    const scope = unitScope(), value = unitResponse(scope);
    for (const field of ['unit', 'cursor', 'fragments', 'start_stack', 'end_ordered_lists'] as const) {
      const broken = structuredClone(value); Reflect.deleteProperty(broken.window!, field);
      expect(() => validUnitResponse(broken, scope, { event: 0, offset: 0 })).not.toThrow();
      expect(validUnitResponse(broken, scope, { event: 0, offset: 0 })).toBeNull();
    }
    const protectedScope = unitScope('protected'), raw = unitResponse(protectedScope); raw.error_code = 'UNKNOWN';
    expect(validUnitResponse(raw, protectedScope, { event: 0, offset: 0 })).toBeNull();
  });

  it('uses a resumed list item number while keeping the original source event unchanged', () => {
    const scope = unitScope('list'), window = unitResponse(scope, 1).window!;
    const original = structuredClone(window); const events = windowEvents(window, scope.unit)!;
    expect(events[0].list_start).toBe(window.start_ordered_lists[0].item_start); expect(window).toEqual(original);
  });
});
