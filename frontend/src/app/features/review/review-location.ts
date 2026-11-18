import type { ParamMap } from '@angular/router';
import { digest, integer, uuid } from '../comparison/comparison-evidence';
import type { ReviewLocation } from './shared-review-state';
export interface ReviewPageLocation extends ReviewLocation { job?: string; file?: string; mode?: 'raw' | 'readable'; unit?: string; }
export function reviewLocation(query: ParamMap): ReviewPageLocation | null {
  if (query.keys.some(key => !['snapshot', 'files', 'media', 'after', 'job', 'file', 'mode', 'unit'].includes(key) || query.getAll(key).length !== 1)) return null;
  const snapshot = query.get('snapshot'), after = query.get('after'), job = query.get('job');
  if ([snapshot, after, job].some(value => value !== null && !uuid(value))) return null;
  const offset = (key: string, max: number): number | null => {
    const value = query.get(key); if (value === null) return 0;
    return /^(0|[1-9]\d{0,4})$/.test(value) && integer(Number(value), max) ? Number(value) : null;
  };
  const file = query.get('file'), mode = query.get('mode'), unit = query.get('unit');
  if (file !== null && (!digest(file) || !snapshot) || mode !== null && (!file || !['raw', 'readable'].includes(mode))
    || unit !== null && (!file || mode === 'raw' || !digest(unit))) return null;
  const files = offset('files', 40000), media = offset('media', 20000);
  if (files === null || media === null || !snapshot && (files !== 0 || media !== 0)) return null;
  return { ...(snapshot ? { snapshot } : {}), ...(after ? { after } : {}), ...(job ? { job } : {}), files, media, ...(file ? { file } : {}), ...(mode ? { mode: mode as 'raw' | 'readable' } : {}), ...(unit ? { unit } : {}) };
}
