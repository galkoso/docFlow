import type { ParamMap } from '@angular/router';
import { digest, integer, sha, uuid } from './comparison-evidence';

export interface ComparisonLocation {
  snapshot: string | null; request: string | null; head: string | null; target: string | null;
  offset: number; file: string | null; unit: string | null; view: 'readable' | 'raw';
}
const keys = ['snapshot', 'request_id', 'head', 'target', 'offset', 'file', 'unit', 'view'];
/** A locator selects immutable identities. It carries no source, actor identity,
 * authorization, approval or unchecked source-line anchor. */
export function comparisonLocation(query: ParamMap): ComparisonLocation | null {
  if (query.keys.some(key => !keys.includes(key) || query.getAll(key).length !== 1 || query.get(key) === '')) return null;
  const snapshot = query.get('snapshot'), request = query.get('request_id'), head = query.get('head'), target = query.get('target');
  const file = query.get('file'), unit = query.get('unit'), view = query.get('view') ?? 'readable', text = query.get('offset');
  const offset = text == null ? 0 : Number(text);
  if (snapshot != null && !uuid(snapshot) || request != null && !uuid(request) || head != null && !sha(head) || target != null && !sha(target)
    || file != null && !digest(file) || unit != null && !digest(unit) || !['readable', 'raw'].includes(view)
    || !integer(offset, 40000) || text != null && !/^\d+$/.test(text)
    || snapshot && (request || head || target) || !snapshot && (file || unit || text != null) || file && text != null || unit && (!file || view !== 'readable')) return null;
  return { snapshot, request, head, target, file, unit, offset, view: view as ComparisonLocation['view'] };
}
