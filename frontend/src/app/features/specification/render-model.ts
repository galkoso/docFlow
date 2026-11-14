import type { components } from '../../core/api/generated/schema';

export type RenderEvent = components['schemas']['RenderEvent'];
export type SearchHit = components['schemas']['SearchHit'];
export interface RenderNode { event: RenderEvent; children: RenderNode[]; }
export interface TextPart { text: string; matchId?: string; }

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
export function textParts(event: RenderEvent, hits: SearchHit[]): TextPart[] {
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
  return parts;
}

export function externalLink(destination?: string | null): string | null {
  if (!destination || /[\u0000-\u0020\u007f]/u.test(destination)) return null;
  try {
    const url = new URL(destination);
    return url.protocol === 'https:' && !url.username && !url.password ? url.href : null;
  } catch { return null; }
}
