import type {
  ProductQuestions, ProductQuestionHistory, ProductQuestionCreate, ProductQuestionChange, ProductQuestionRecorded,
} from '../../core/api/product-question-api';
import { integer, sha, uuid } from '../comparison/comparison-evidence';
import { text, utc } from './snapshot-evidence';

export type Question = ProductQuestions['questions'][number];
type Revision = Question['current'];
export type QuestionIntent = { kind: 'create'; body: Readonly<ProductQuestionCreate> }
  | { kind: 'change'; question: string; body: Readonly<ProductQuestionChange> };
const count = (v: unknown, min = 0) => integer(v, Number.MAX_SAFE_INTEGER, min);
const actor = (v: { id: string; name: string }) => !!v && uuid(v.id) && text(v.name, 200);
const order = (v: string) => { const [base, fraction = ''] = v.replace(/(?:Z|\+00:00)$/, '').split('.'); return `${base}.${fraction.padEnd(9, '0')}`; };
const before = (a: string, b: string) => utc(a) && utc(b) && order(a) <= order(b);
export const questionText = (v: unknown): v is string => text(v, 10000, 1) && !!v.trim();
function revision(v: Revision, readAt: string): boolean {
  if (!v || !uuid(v.id) || !count(v.version, 1) || !actor(v.actor) || !before(v.recorded_at, readAt)
    || typeof v.blocking !== 'boolean' || !['OPEN', 'PROPOSE', 'DECIDE', 'REOPEN', 'SET_BLOCKING'].includes(v.kind)
    || !['OPEN', 'PROPOSED', 'DECIDED'].includes(v.state) || (v.version === 1) !== (v.kind === 'OPEN')) return false;
  if (v.state === 'OPEN' ? v.answer != null || v.decision != null
    : !questionText(v.answer) || (v.state === 'DECIDED' ? !v.decision || !actor(v.decision.actor) || !before(v.decision.recorded_at, v.recorded_at) : v.decision != null)) return false;
  if (v.kind === 'OPEN') return v.state !== 'DECIDED';
  if (v.kind === 'PROPOSE') return v.state === 'PROPOSED';
  if (v.kind === 'REOPEN') return v.state === 'OPEN';
  if (v.kind === 'DECIDE') return v.state === 'DECIDED' && v.decision?.actor.id === v.actor.id && order(v.decision.recorded_at) === order(v.recorded_at);
  return true;
}
export function validProductQuestion(v: Question, proposal: string, readAt: string): boolean {
  if (!v || !uuid(v.id) || v.proposal_id !== proposal || !actor(v.creator) || !questionText(v.text) || !revision(v.current, readAt)
    || !before(v.created_at, v.current.recorded_at) || typeof v.outdated_context !== 'boolean'
    || v.current.version === 1 && v.current.actor.id !== v.creator.id) return false;
  const o = v.origin, p = v.permissions;
  if (!o || !uuid(o.repository_id) || !/^change\/[\x21-\x7e]{1,93}$/.test(o.branch) || !sha(o.head_sha) || !sha(o.target_sha)
    || !count(o.proposal_version, 1) || !p || ![p.can_propose, p.can_decide, p.can_reopen, p.can_toggle_blocking].every(v => typeof v === 'boolean')
    || p.can_propose && v.current.state === 'DECIDED' || p.can_reopen && v.current.state === 'OPEN') return false;
  return v.source === 'HUMAN' ? v.source_reference == null : v.source === 'IMPORTED' && uuid(v.source_reference);
}
export function validProductQuestions(v: ProductQuestions, actorId: string, proposal: string, after?: string): boolean {
  return !!v && v.actor_id === actorId && v.proposal_id === proposal && count(v.proposal_version, 1) && utc(v.read_at)
    && typeof v.can_create === 'boolean' && count(v.unresolved_blockers) && Array.isArray(v.questions) && v.questions.length <= 20
    && new Set(v.questions.map(q => q?.id)).size === v.questions.length
    && v.questions.every(q => q?.id !== after && validProductQuestion(q, proposal, v.read_at) && q.origin.proposal_version <= v.proposal_version)
    && v.unresolved_blockers >= v.questions.filter(q => q.current.blocking && q.current.state !== 'DECIDED').length
    && (v.next_after == null || v.questions.length === 20 && v.next_after === v.questions.at(-1)?.id);
}
function identity(v: Revision): string {
  return JSON.stringify([v.id, v.version, v.actor.id, v.actor.name, v.kind, v.state, v.blocking, v.answer ?? null,
    v.decision ? [v.decision.actor.id, v.decision.actor.name, order(v.decision.recorded_at)] : null, order(v.recorded_at)]);
}
export function validProductQuestionHistory(v: ProductQuestionHistory, actorId: string, proposal: string, question: string, after?: number): boolean {
  if (!v || v.actor_id !== actorId || v.proposal_id !== proposal || !utc(v.read_at) || v.question?.id !== question
    || !validProductQuestion(v.question, proposal, v.read_at) || !Array.isArray(v.revisions) || v.revisions.length > 20
    || new Set(v.revisions.map(r => r?.id)).size !== v.revisions.length || after != null && !count(after, 1)) return false;
  let previous = after ?? 0;
  for (const r of v.revisions) {
    if (!revision(r, v.read_at) || r.version !== previous + 1 || r.version > v.question.current.version || !before(v.question.created_at, r.recorded_at)
      || r.version === 1 && r.actor.id !== v.question.creator.id) return false;
    previous = r.version;
  }
  if (previous === v.question.current.version && v.revisions.length && identity(v.revisions.at(-1)!) !== identity(v.question.current)) return false;
  return v.next_after == null ? previous === v.question.current.version
    : v.revisions.length === 20 && v.next_after === previous && previous < v.question.current.version;
}
export function validProductQuestionReceipt(v: ProductQuestionRecorded, actorId: string, proposal: string, intent: QuestionIntent): boolean {
  return !!v && uuid(v.id) && v.actor_id === actorId && v.proposal_id === proposal && uuid(v.request_id) && v.request_id === intent.body.request_id
    && uuid(v.question_id) && count(v.version, 1) && utc(v.recorded_at) && typeof v.replayed === 'boolean'
    && (intent.kind === 'create' ? v.kind === 'OPEN' && v.version === 1
      : v.question_id === intent.question && v.kind === intent.body.action.kind.toUpperCase() && v.version === intent.body.expected_version + 1);
}
