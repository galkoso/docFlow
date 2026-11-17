import type { RenderEvent } from '../specification/render-model';
import { wordChanges } from './word-changes';

const event = (kind: RenderEvent['kind'], values: Partial<RenderEvent> = {}): RenderEvent =>
  ({ kind, source_start_byte: 0, source_end_byte: 1, ...values });
function paragraph(value: string, id = 'block'): RenderEvent[] {
  return [event('open', { tag: 'paragraph' }), event('text', { block_id: id, offset_utf16: 0, text: value }), event('close', { tag: 'paragraph' })];
}
const values = (source: string, ranges: ReturnType<typeof wordChanges>['before']) => ranges.map(range => source.slice(range.start_utf16, range.end_utf16));

describe('Bounded visible word changes', () => {
  it('marks replacements, additions, removals and punctuation while keeping unchanged words plain', () => {
    const before = 'The brown fox, jumps.', after = 'The quick fox jumps!';
    const result = wordChanges(paragraph(before, 'old'), paragraph(after, 'new'));
    expect(result.precise).toBe(true); expect(values(before, result.before)).toEqual(['brown', ',', '.']);
    expect(values(after, result.after)).toEqual(['quick', '!']);
    const added = wordChanges(paragraph('The fox'), paragraph('The quick fox'));
    expect(added.before).toEqual([]); expect(values('The quick fox', added.after)).toEqual(['quick']);
    const removed = wordChanges(paragraph('The quick fox'), paragraph('The fox'));
    expect(values('The quick fox', removed.before)).toEqual(['quick']); expect(removed.after).toEqual([]);
  });
  it('preserves Hebrew niqqud, astral emoji, ZWJ and exact Unicode/case distinctions', () => {
    const before = 'שָׁלוֹם 👩🏽‍💻 עולם', after = 'שָׁלוֹם 👨‍💻 חדש';
    const result = wordChanges(paragraph(before), paragraph(after));
    expect(result.precise).toBe(true); expect(values(before, result.before)).toEqual(['👩🏽‍💻 עולם']);
    expect(values(after, result.after)).toEqual(['👨‍💻 חדש']);
    for (const [a, b] of [['שָׁלוֹם', 'שלום'], ['café', 'cafe\u0301'], ['Hello', 'hello']]) {
      const change = wordChanges(paragraph(a), paragraph(b)); expect(change.precise).toBe(true);
      expect(values(a, change.before)).toEqual([a]); expect(values(b, change.after)).toEqual([b]);
    }
  });
  it('maps a changed word across formatting leaves to the same visible block coordinates', () => {
    const before = [event('open', { tag: 'paragraph' }), event('text', { text: 'red co', block_id: 'old', offset_utf16: 0 }),
      event('open', { tag: 'strong' }), event('text', { text: 'lor', block_id: 'old', offset_utf16: 6 }), event('close', { tag: 'strong' }),
      event('text', { text: ' blue', block_id: 'old', offset_utf16: 9 }), event('close', { tag: 'paragraph' })];
    const result = wordChanges(before, paragraph('red colour blue', 'new'));
    expect(result.precise).toBe(true); expect(result.before).toEqual([{ block_id: 'old', start_utf16: 4, end_utf16: 9, kind: 'removed' }]);
    expect(result.after).toEqual([{ block_id: 'new', start_utf16: 4, end_utf16: 10, kind: 'added' }]);
  });
  it('does not invent word identities for ambiguous repetitions or changed block structure', () => {
    for (const [before, after] of [['go go', 'go'], ['a b', 'b a'], ['same x same', 'same']]) {
      expect(wordChanges(paragraph(before), paragraph(after))).toEqual({ before: [], after: [], precise: false });
    }
    expect(wordChanges(paragraph('one'), [event('open', { tag: 'heading' }), event('text', { text: 'two', block_id: 'b', offset_utf16: 0 }),
      event('close', { tag: 'heading' })]).precise).toBe(false);
    expect(wordChanges(paragraph('same'), paragraph('same'))).toEqual({ before: [], after: [], precise: true });
  });
  it('keeps whitespace-only changes exact and rejects incomplete or discontinuous block coordinates', () => {
    const change = wordChanges(paragraph('a b'), paragraph('a  b'));
    expect(change.before).toEqual([]); expect(values('a  b', change.after)).toEqual([' ']);
    const incomplete = paragraph('tail'); incomplete[1].offset_utf16 = 10;
    expect(wordChanges(incomplete, paragraph('other')).precise).toBe(false);
    const image = [...paragraph('text'), event('image', { text: 'alt' })];
    expect(wordChanges(image, image).precise).toBe(false);
  });
  it('bounds input, token and alignment work before allocating an unbounded matrix', () => {
    expect(wordChanges(paragraph('a'.repeat(32769)), paragraph('b')).precise).toBe(false);
    expect(wordChanges(paragraph('a '.repeat(1100)), paragraph('b '.repeat(1100))).precise).toBe(false);
    expect(wordChanges(paragraph(Array.from({ length: 2001 }, (_, n) => String(n)).join(' ')), paragraph('b')).precise).toBe(false);
  });
  it('agrees with exhaustive optimal token matchings on all short repeated sequences', () => {
    const sequences: string[][] = [[]];
    for (let length = 1; length <= 3; length++) for (let bits = 0; bits < 2 ** length; bits++) {
      sequences.push(Array.from({ length }, (_, n) => bits & (1 << n) ? 'b' : 'a'));
    }
    function optimal(a: string[], b: string[]): number {
      const matches = new Set<string>(); let longest = -1;
      function visit(i: number, j: number, pairs: Array<[number, number]>) {
        if (pairs.length > longest) { matches.clear(); longest = pairs.length; }
        if (pairs.length === longest) matches.add(JSON.stringify(pairs));
        for (let x = i; x < a.length; x++) for (let y = j; y < b.length; y++) {
          if (a[x] === b[y]) visit(x + 1, y + 1, [...pairs, [x, y]]);
        }
      }
      visit(0, 0, []); return matches.size;
    }
    for (const a of sequences) for (const b of sequences) {
      expect(wordChanges(paragraph(a.join(' ')), paragraph(b.join(' '))).precise, `${a}/${b}`).toBe(optimal(a, b) === 1);
    }
  });
});
