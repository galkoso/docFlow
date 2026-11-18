import type { ReviewPrStatus, ReviewSnapshot } from '../../core/api/review-api';
import { sha, uuid } from '../comparison/comparison-evidence';
import { utc } from './snapshot-evidence';

const dispatches = ['AWAITING', 'WAITING', 'ENQUEUED', 'SKIPPED', 'REJECTED', 'UNTRACKED'];
const phases = ['QUEUED', 'OBSERVING', 'RETRY_WAIT', 'RECONCILING', 'OBSERVED_SYNCED', 'SUPERSEDED', 'FAILED'];
const reasons = ['BUSY', 'QUOTA', 'AUTHORITY_REQUIRED', 'SETUP_REQUIRED', 'SOURCE_CHANGED', 'BINDING_CHANGED', 'INVALID_PUBLICATION',
  'INVALID_PROJECTION', 'PROVIDER_UNAVAILABLE', 'PROVIDER_RATE_LIMIT', 'PROVIDER_REJECTED', 'CONFLICT', 'UNCERTAIN_RESULT', 'OBSERVATION_PENDING', 'OTHER'];
const reason = (v: unknown) => v == null || typeof v === 'string' && reasons.includes(v);
const time = (v: unknown) => v == null || utc(v);
const order = (v: string) => { const [base, fraction = ''] = v.replace(/(?:Z|\+00:00)$/, '').split('.'); return `${base}.${fraction.padEnd(9, '0')}`; };
export function validPrStatus(v: ReviewPrStatus, proposal: string, snapshot: ReviewSnapshot): boolean {
  const summary = snapshot.metadata.summary;
  if (!v || v.proposal_id !== proposal || v.snapshot_id !== snapshot.snapshot_id || v.fingerprint !== snapshot.fingerprint
    || v.head_sha !== summary.head_sha || v.target_sha !== summary.target_sha || !utc(v.read_at)
    || typeof v.current_for_sync !== 'boolean' || typeof v.binding_current !== 'boolean' || typeof v.uncertain_write !== 'boolean'
    || !v.binding_current && (v.current_for_sync || v.pr != null)) return false;
  const d = v.dispatch, s = v.sync, p = v.pr;
  if (v.recovery != null && (!['RETRY_UNSTARTED', 'OBSERVE_STARTED'].includes(v.recovery) || !v.binding_current
    || v.recovery === 'RETRY_UNSTARTED' && !v.current_for_sync
    || v.recovery === 'OBSERVE_STARTED' && !v.uncertain_write)) return false;
  if (!d || !dispatches.includes(d.phase) || !reason(d.reason) || !time(d.next_attempt_at)
    || (d.phase === 'WAITING') !== (d.next_attempt_at != null)) return false;
  if (s != null && (d.phase !== 'ENQUEUED' || !phases.includes(s.phase) || !reason(s.reason) || !time(s.next_attempt_at)
    || typeof s.automatic !== 'boolean' || ['OBSERVED_SYNCED', 'SUPERSEDED', 'FAILED'].includes(s.phase) && s.automatic
    || s.phase === 'RETRY_WAIT' && s.next_attempt_at == null)) return false;
  if (p != null) {
    if (typeof p.number !== 'string' || !/^[1-9]\d{0,18}$/.test(p.number) || BigInt(p.number) > 9223372036854775807n
      || !uuid(p.snapshot_id) || !sha(p.head_sha) || !sha(p.target_sha) || typeof p.superseded !== 'boolean'
      || !utc(p.recorded_at) || order(p.recorded_at) > order(v.read_at) || typeof p.url !== 'string' || p.url.length > 2048) return false;
    try {
      const url = new URL(p.url);
      if (url.href !== p.url || url.protocol !== 'https:' || url.host !== 'github.com' || url.username || url.password || url.search || url.hash
        || !/^\/[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+\/pull\/[1-9]\d*$/.test(url.pathname) || url.pathname.split('/').at(-1) !== p.number) return false;
    } catch { return false; }
  }
  return s?.phase !== 'OBSERVED_SYNCED' || !!p && v.current_for_sync && !v.uncertain_write && !p.superseded
    && p.snapshot_id === v.snapshot_id && p.head_sha === v.head_sha && p.target_sha === v.target_sha;
}
