import type { components } from '../../core/api/generated/schema';

export type RenderEvent = components['schemas']['RenderEvent'];
export type SearchHit = components['schemas']['SearchHit'];
export type TextHighlight = Pick<SearchHit, 'block_id' | 'start_utf16' | 'end_utf16' | 'match_id'>;
export type MatchWindow = components['schemas']['MatchWindow'];
export const TEXT_WINDOW_UTF16 = 1024;
export interface TextChunk { event: RenderEvent; window: MatchWindow | null; }
export interface RenderNode { event: RenderEvent; children: RenderNode[]; chunks?: TextChunk[]; alignment?: 'left' | 'center' | 'right'; }
export interface TextChange { block_id: string; start_utf16: number; end_utf16: number; kind: 'added' | 'removed'; }
export interface TextPart { text: string; matchId?: string; change?: TextChange['kind']; }
export interface AssetContext { repository: string; commit: string; proposal?: string; }

/** Split display leaves without splitting graphemes. A giant grapheme needs only
 * one short request window: canonical matches cover its entire original range. */
export function textChunks(event: RenderEvent): TextChunk[] {
  const text = event.text ?? '';
  const chunks: TextChunk[] = [];
  const add = (start: number, end: number) => {
    const offset = event.offset_utf16 == null ? null : event.offset_utf16 + start;
    const leaf = { ...event, text: text.slice(start, end), offset_utf16: offset };
    const window = event.block_id && offset != null && end > start
      ? { block_id: event.block_id, start_utf16: offset, end_utf16: offset + Math.min(TEXT_WINDOW_UTF16, end - start) } : null;
    chunks.push({ event: leaf, window });
  };
  if (text.length <= TEXT_WINDOW_UTF16) { add(0, text.length); return chunks; }
  let start = 0;
  let end = 0;
  for (const grapheme of new Intl.Segmenter('und', { granularity: 'grapheme' }).segment(text)) {
    const next = grapheme.index + grapheme.segment.length;
    if (next - start > TEXT_WINDOW_UTF16 && end > start) { add(start, end); start = end; }
    end = next;
  }
  add(start, end);
  return chunks;
}

export function assetSource(context: AssetContext | null, destination?: string | null): string | null {
  const uuid = /^[a-f0-9]{8}(-[a-f0-9]{4}){3}-[a-f0-9]{12}$/i;
  if (!context || !destination || !/^assets\/[a-f0-9]{64}\.(png|jpg|jpeg|webp)$/.test(destination)
    || !uuid.test(context.repository) || (context.proposal !== undefined && !uuid.test(context.proposal))
    || !/^[a-f0-9]{40,128}$/.test(context.commit) || context.commit.length % 2 !== 0) return null;
  const scope = context.proposal ? `proposals/${context.proposal}` : `repositories/${context.repository}`;
  return `/api/v1/${scope}/assets/${destination.slice(7)}?commit=${context.commit}`;
}

/** A bounded tree of inert events; markup is created only by the Angular template. */
export function renderTree(events: RenderEvent[]): RenderNode[] {
  if (events.length > 100_000) throw new Error('RENDER_EVENT_LIMIT');
  const root: RenderNode[] = [];
  const stack: RenderNode[] = [];
  for (const event of events) {
    if (event.kind === 'close') {
      if (!stack.length || stack.pop()!.event.tag !== event.tag) throw new Error('RENDER_UNBALANCED');
      continue;
    }
    const node: RenderNode = { event, children: [] };
    if (event.kind === 'text' || event.kind === 'break') node.chunks = textChunks(event);
    if (event.kind === 'open' && event.tag === 'table_cell') {
      const table = [...stack].reverse().find(parent => parent.event.tag === 'table');
      const column = stack.at(-1)?.children.length ?? 0;
      const alignment = table?.event.alignments?.[column];
      if (alignment === 'left' || alignment === 'center' || alignment === 'right') node.alignment = alignment;
    }
    (stack.at(-1)?.children ?? root).push(node);
    if (event.kind === 'open') {
      if (!event.tag || stack.length >= 64) throw new Error('RENDER_DEPTH_LIMIT');
      stack.push(node);
    }
  }
  if (stack.length) throw new Error('RENDER_UNBALANCED');
  return root;
}

/** Intersect server UTF-16 ranges with each text leaf, including formatted leaves. */
export function textParts(event: RenderEvent, hits: TextHighlight[], changes: TextChange[] = []): TextPart[] {
  const text = event.text ?? '';
  const start = event.offset_utf16;
  if (start == null || !event.block_id) return [{ text }];
  const ranges = hits.filter(hit => hit.block_id === event.block_id && hit.end_utf16 > start && hit.start_utf16 < start + text.length)
    .sort((a, b) => a.start_utf16 - b.start_utf16);
  const parts: TextPart[] = [];
  let offset = 0;
  for (const range of ranges) {
    const from = Math.max(offset, range.start_utf16 - start, 0);
    const to = Math.min(text.length, range.end_utf16 - start);
    if (to <= from) continue;
    if (from > offset) parts.push({ text: text.slice(offset, from) });
    parts.push({ text: text.slice(from, to), matchId: range.match_id });
    offset = to;
  }
  if (offset < text.length) parts.push({ text: text.slice(offset) });
  const overlays = changes.filter(change => change.block_id === event.block_id && change.end_utf16 > start
    && change.start_utf16 < start + text.length);
  if (!overlays.length) return parts;
  const output: TextPart[] = []; let position = start;
  for (const part of parts) {
    const end = position + part.text.length, cuts = new Set([position, end]);
    for (const change of overlays) {
      if (change.start_utf16 > position && change.start_utf16 < end) cuts.add(change.start_utf16);
      if (change.end_utf16 > position && change.end_utf16 < end) cuts.add(change.end_utf16);
    }
    const boundaries = [...cuts].sort((a, b) => a - b);
    for (let index = 0; index + 1 < boundaries.length; index++) {
      const from = boundaries[index], to = boundaries[index + 1];
      const change = overlays.find(change => change.start_utf16 <= from && change.end_utf16 >= to)?.kind;
      output.push({ ...part, text: part.text.slice(from - position, to - position), ...(change ? { change } : {}) });
    }
    position = end;
  }
  return output;
}

export function externalLink(destination?: string | null): string | null {
  if (!destination || /[\u0000-\u0020\u007f]/u.test(destination)) return null;
  try {
    const url = new URL(destination);
    return url.protocol === 'https:' && !url.username && !url.password ? url.href : null;
  } catch { return null; }
}
