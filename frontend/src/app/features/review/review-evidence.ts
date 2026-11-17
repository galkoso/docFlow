import type { ConfigurationUpdated, ReviewArea, ReviewConfiguration, ReviewerCandidates,
  ReviewConfigurationRequest, ReviewSettings } from '../../core/api/review-configuration-api';

export const areas: ReviewArea[] = ['PRODUCT', 'DEVELOPMENT', 'DESIGN'];
export const uuid = (v: unknown): v is string => typeof v === 'string'
  && /^[a-f0-9]{8}-(?:[a-f0-9]{4}-){3}[a-f0-9]{12}$/.test(v) && v !== '00000000-0000-0000-0000-000000000000';
const positive = (v: unknown): v is number => Number.isSafeInteger(v) && (v as number) > 0;
const text = (v: unknown, max: number): v is string => typeof v === 'string' && Array.from(v).length <= max && !v.includes('\0');
const sha = (v: unknown): v is string => typeof v === 'string' && /^[a-f0-9]{40}(?:[a-f0-9]{24})?$/.test(v);
// Match Rust str::trim (Unicode White_Space), including U+0085 and excluding
// U+FEFF. JavaScript's built-in trim uses a different whitespace set.
export const trimReason = (value: string): string => value.replace(/^\p{White_Space}+|\p{White_Space}+$/gu, '');
export function canonical(settings: ReviewSettings): ReviewSettings {
  return { ui_impact: settings.ui_impact, ui_impact_reason: trimReason(settings.ui_impact_reason ?? '') || null,
    assignments: settings.assignments.map(item => ({ ...item })).sort((a, b) =>
      a.domain < b.domain ? -1 : a.domain > b.domain ? 1 : a.reviewer_id < b.reviewer_id ? -1 : a.reviewer_id > b.reviewer_id ? 1 : 0) };
}
export function validConfiguration(v: ReviewConfiguration, proposal: string): boolean {
  if (!v || v.proposal_id !== proposal || !positive(v.number) || !text(v.title, 120) || !v.title.trim()
    || !text(v.branch, 100) || !v.branch.startsWith('change/') || !text(v.target_branch, 1024) || !v.target_branch
    || !positive(v.proposal_version) || !positive(v.configuration_version) || !positive(v.policy_version)
    || v.configuration_version > v.proposal_version
    || typeof v.can_edit !== 'boolean' || v.head_sha != null && !sha(v.head_sha)
    || !['PROVISIONING', 'DRAFT', 'IN_REVIEW', 'CHANGES_REQUESTED', 'APPROVED', 'MERGING', 'MERGED', 'CLOSED'].includes(v.state)) return false;
  if (v.can_edit && (!v.head_sha || !['DRAFT', 'IN_REVIEW', 'CHANGES_REQUESTED', 'APPROVED'].includes(v.state))) return false;
  const settings = v.settings;
  if (!settings || !['yes', 'no', 'undecided'].includes(settings.ui_impact)
    || settings.ui_impact_reason != null && !text(settings.ui_impact_reason, 10000)
    || settings.ui_impact === 'no' && !trimReason(settings.ui_impact_reason ?? '')
    || !Array.isArray(settings.assignments) || settings.assignments.length > 200
    || !Array.isArray(v.reviewers) || v.reviewers.length !== settings.assignments.length) return false;
  const keys = new Set<string>();
  return settings.assignments.every(item => {
    if (!item || !uuid(item.reviewer_id) || !areas.includes(item.domain) || typeof item.required !== 'boolean') return false;
    const key = `${item.reviewer_id}:${item.domain}`;
    if (keys.has(key)) return false;
    keys.add(key);
    const statuses = v.reviewers.filter(status => status?.reviewer_id === item.reviewer_id && status.domain === item.domain);
    return statuses.length === 1 && statuses[0].required === item.required && text(statuses[0].display_name, 200)
      && typeof statuses[0].active === 'boolean' && typeof statuses[0].qualified === 'boolean' && (!statuses[0].qualified || statuses[0].active);
  });
}
export function validReceipt(v: ConfigurationUpdated, proposal: string, intent: ReviewConfigurationRequest): boolean {
  return !!v && v.proposal_id === proposal && v.request_id === intent.request_id && typeof v.changed === 'boolean'
    && typeof v.replayed === 'boolean' && v.applied_proposal_version === intent.expected_proposal_version + Number(v.changed)
    && v.applied_configuration_version === intent.expected_configuration_version + Number(v.changed)
    && validConfiguration(v.current, proposal) && v.current.proposal_version >= v.applied_proposal_version
    && v.current.configuration_version >= v.applied_configuration_version
    && (v.current.proposal_version !== v.applied_proposal_version || v.current.head_sha === intent.expected_head_sha)
    && (v.current.configuration_version !== v.applied_configuration_version
      || JSON.stringify(canonical(v.current.settings)) === JSON.stringify(canonical(intent.settings)));
}
export function validCandidates(v: ReviewerCandidates, proposal: string, version: number, domain: ReviewArea, after?: string): boolean {
  if (!v || v.proposal_id !== proposal || v.configuration_version !== version || v.domain !== domain
    || !Array.isArray(v.reviewers) || v.reviewers.length > 50) return false;
  let previous = after ?? '';
  for (const item of v.reviewers) {
    if (!item || !uuid(item.reviewer_id) || item.reviewer_id <= previous || !text(item.display_name, 200)) return false;
    previous = item.reviewer_id;
  }
  return v.next_after === null || uuid(v.next_after) && v.reviewers.length === 50 && v.next_after === previous;
}
