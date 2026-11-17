import type { RenderEvent, TextChange } from '../specification/render-model';

interface Block { id: string; text: string; }
interface Token { text: string; start: number; end: number; }
export interface WordChanges { before: TextChange[]; after: TextChange[]; precise: boolean; }
const MAX_TEXT = 32768, MAX_TOKENS = 2000, MAX_CELLS = 1_000_000, MAX_RANGES = 2048;
const graphemes = new Intl.Segmenter('und', { granularity: 'grapheme' });
const words = new Intl.Segmenter('und', { granularity: 'word' });
const inline = new Set(['emphasis', 'strong', 'strike', 'inline_code', 'link']);
const empty = (): WordChanges => ({ before: [], after: [], precise: false });

function blocks(events: RenderEvent[]): { shape: string; blocks: Block[] } | null {
  if (events.length > 256) return null;
  const shape: string[] = [], output: Block[] = [], known = new Set<string>();
  let size = 0;
  for (const event of events) {
    if (event.kind === 'open' || event.kind === 'close') {
      if (!event.tag) return null;
      if (!inline.has(event.tag)) shape.push(`${event.kind}:${event.tag}`);
    } else if (event.kind === 'text' || event.kind === 'break') {
      if (!event.block_id || event.offset_utf16 == null || typeof event.text !== 'string') return null;
      size += event.text.length; if (size > MAX_TEXT) return null;
      let block = output.at(-1);
      if (block?.id !== event.block_id) {
        if (known.has(event.block_id)) return null;
        known.add(event.block_id); block = { id: event.block_id, text: '' }; output.push(block);
      }
      if (event.offset_utf16 !== block!.text.length) return null;
      block!.text += event.text;
    } else return null; // Images, task state, rules and unsupported atoms need their own comparison.
  }
  return { shape: JSON.stringify(shape), blocks: output };
}
function tokens(text: string): Token[] | null {
  const boundaries = new Set<number>([0, text.length]);
  for (const item of graphemes.segment(text)) boundaries.add(item.index);
  const result: Token[] = [];
  for (const item of words.segment(text)) {
    if (/^\s+$/u.test(item.segment)) continue;
    const end = item.index + item.segment.length;
    if (!boundaries.has(item.index) || !boundaries.has(end)) return null;
    result.push({ text: item.segment, start: item.index, end });
    if (result.length > MAX_TOKENS) return null;
  }
  return result;
}
/** Return the only optimal ordered matching of visible tokens. Multiple edit
 * orders are fine; multiple possible matched-token identities are ambiguous. */
function anchors(a: Token[], b: Token[]): Array<[number, number]> | null {
  const width = b.length + 1, suffix = new Uint16Array((a.length + 1) * width);
  for (let i = a.length - 1; i >= 0; i--) for (let j = b.length - 1; j >= 0; j--) {
    suffix[i * width + j] = a[i].text === b[j].text ? 1 + suffix[(i + 1) * width + j + 1]
      : Math.max(suffix[(i + 1) * width + j], suffix[i * width + j + 1]);
  }
  const length = suffix[0], result: Array<[number, number] | undefined> = Array(length);
  let previous = new Uint16Array(width);
  for (let i = 0; i < a.length; i++) {
    const current = new Uint16Array(width);
    for (let j = 0; j < b.length; j++) {
      if (a[i].text === b[j].text) {
        current[j + 1] = previous[j] + 1;
        if (previous[j] + 1 + suffix[(i + 1) * width + j + 1] === length) {
          if (result[previous[j]]) return null;
          result[previous[j]] = [i, j];
        }
      } else current[j + 1] = Math.max(previous[j + 1], current[j]);
    }
    previous = current;
  }
  return result.filter(Boolean).length === length ? result as Array<[number, number]> : null;
}
function sharedWhitespace(a: string, b: string): { start: number; oldEnd: number; newEnd: number } {
  const old = [...graphemes.segment(a)], next = [...graphemes.segment(b)];
  let start = 0, first = 0, oldLast = old.length, newLast = next.length;
  while (first < oldLast && first < newLast && old[first].segment === next[first].segment && /^\s+$/u.test(old[first].segment)) {
    start += old[first++].segment.length;
  }
  let oldEnd = a.length, newEnd = b.length;
  while (oldLast > first && newLast > first && old[oldLast - 1].segment === next[newLast - 1].segment
    && /^\s+$/u.test(old[oldLast - 1].segment)) {
    oldEnd -= old[--oldLast].segment.length; newEnd -= next[--newLast].segment.length;
  }
  return { start, oldEnd, newEnd };
}
function changedRange(block: Block, start: number, end: number, kind: TextChange['kind']): TextChange | null {
  const text = block.text.slice(start, end), trimmed = text.trim();
  // Emphasize words/punctuation rather than incidental surrounding spaces.
  // A whitespace-only difference still gets its own exact visible range.
  if (trimmed) {
    const from = text.length - text.trimStart().length, to = from + trimmed.length;
    const boundaries = new Set<number>([0, text.length]);
    for (const item of graphemes.segment(text)) boundaries.add(item.index);
    if (boundaries.has(from) && boundaries.has(to)) { start += from; end = start + trimmed.length; }
  }
  return end > start ? { block_id: block.id, start_utf16: start, end_utf16: end, kind } : null;
}
/** Exact visible text only: no Unicode/case/whitespace normalization and no
 * inferred pairing across a changed block structure. All work has fixed bounds. */
export function wordChanges(before: RenderEvent[], after: RenderEvent[]): WordChanges {
  const a = blocks(before), b = blocks(after);
  if (!a || !b || a.shape !== b.shape || a.blocks.length !== b.blocks.length
    || [...a.blocks, ...b.blocks].reduce((sum, block) => sum + block.text.length, 0) > MAX_TEXT) return empty();
  const result: WordChanges = { before: [], after: [], precise: true };
  let cells = 0, count = 0;
  for (let index = 0; index < a.blocks.length; index++) {
    const old = a.blocks[index], next = b.blocks[index];
    if (old.text === next.text) continue;
    const oldTokens = tokens(old.text), newTokens = tokens(next.text);
    if (!oldTokens || !newTokens) return empty();
    cells += (oldTokens.length + 1) * (newTokens.length + 1); count += oldTokens.length + newTokens.length;
    if (cells > MAX_CELLS || count > MAX_TOKENS) return empty();
    const matching = anchors(oldTokens, newTokens); if (!matching) return empty();
    let oldEnd = 0, newEnd = 0;
    for (const [i, j] of [...matching, [oldTokens.length, newTokens.length]]) {
      const oldStart = oldTokens[i]?.start ?? old.text.length, newStart = newTokens[j]?.start ?? next.text.length;
      if (old.text.slice(oldEnd, oldStart) !== next.text.slice(newEnd, newStart)) {
        const gap = sharedWhitespace(old.text.slice(oldEnd, oldStart), next.text.slice(newEnd, newStart));
        const removed = changedRange(old, oldEnd + gap.start, oldEnd + gap.oldEnd, 'removed');
        const added = changedRange(next, newEnd + gap.start, newEnd + gap.newEnd, 'added');
        if (removed) result.before.push(removed); if (added) result.after.push(added);
      }
      oldEnd = oldTokens[i]?.end ?? old.text.length; newEnd = newTokens[j]?.end ?? next.text.length;
    }
    if (result.before.length + result.after.length > MAX_RANGES) return empty();
  }
  return result;
}
