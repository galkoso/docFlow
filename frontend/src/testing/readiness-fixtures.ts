import type { StoredReviewReadiness } from '../app/core/api/review-api';
import { metadata, product, proposal } from './review-fixtures';
export function storedReadiness(actor = product, questions = false): StoredReviewReadiness {
  const snapshot = metadata(), s = snapshot.metadata.summary;
  return { actor_id: actor, proposal_id: proposal, snapshot_id: snapshot.snapshot_id, fingerprint: snapshot.fingerprint,
    head_sha: s.head_sha, target_sha: s.target_sha, policy_version: s.policy_version, configuration_version: s.configuration_version,
    current_for_metadata: true, provider_refs_verified: false, read_at: '2026-11-25T10:01:00Z', evaluation: { conclusion: 'PENDING',
      issues: [{ gate: 'EVIDENCE', conclusion: 'PENDING', code: 'PROVIDER_REFS_UNVERIFIED' },
        ...(questions ? [{ gate: 'BLOCKING_QUESTIONS' as const, conclusion: 'PENDING' as const, code: 'POLICY_EVIDENCE_PENDING' }] : [])] } };
}
