import type { ParamMap } from '@angular/router';
import { integer, uuid } from '../comparison/comparison-evidence';
import type { ReviewLocation } from './shared-review-state';
export interface ReviewPageLocation extends ReviewLocation { job?: string; }
export function reviewLocation(query: ParamMap): ReviewPageLocation | null {
  if (query.keys.some(key => !['snapshot', 'files', 'media', 'after', 'job'].includes(key) || query.getAll(key).length !== 1)) return null;
  const snapshot = query.get('snapshot'), after = query.get('after'), job = query.get('job');
  if ([snapshot, after, job].some(value => value !== null && !uuid(value))) return null;
  const offset = (key: string, max: number): number | null => {
    const value = query.get(key); if (value === null) return 0;
    return /^(0|[1-9]\d{0,4})$/.test(value) && integer(Number(value), max) ? Number(value) : null;
  };
  const files = offset('files', 40000), media = offset('media', 20000);
  if (files === null || media === null || !snapshot && (files !== 0 || media !== 0)) return null;
  return { ...(snapshot ? { snapshot } : {}), ...(after ? { after } : {}), ...(job ? { job } : {}), files, media };
}
