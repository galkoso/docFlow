import type { ProductQuestions, ProductQuestionHistory, ProductQuestionRecorded, ProductQuestionChange } from '../app/core/api/product-question-api';
import { actor, product, proposal } from './review-fixtures';
import type { Question } from '../app/features/review/product-question-evidence';

export const questionId = '00000000-0000-4000-8000-000000000081';
export const questionRevision = '00000000-0000-4000-8000-000000000082';
export const questionTime = '2026-11-25T10:00:00Z';
export function question(): Question {
  return { id: questionId, proposal_id: proposal, creator: { id: actor, name: 'Creator' }, text: 'שאלה <script>unsafe()</script>\r\n🦀',
    source: 'IMPORTED', source_reference: '00000000-0000-4000-8000-000000000083',
    origin: { repository_id: '00000000-0000-4000-8000-000000000084', branch: 'change/questions', head_sha: 'a'.repeat(40), target_sha: 'b'.repeat(40), proposal_version: 2 },
    outdated_context: false, created_at: questionTime, current: { id: questionRevision, version: 1, actor: { id: actor, name: 'Creator' }, kind: 'OPEN',
      state: 'PROPOSED', blocking: true, answer: 'תשובה מוצעת <img src=x> 🦀', decision: null, recorded_at: questionTime },
    permissions: { can_propose: true, can_decide: true, can_reopen: true, can_toggle_blocking: true } };
}
export function questionPage(user = product, items: Question[] = [question()]): ProductQuestions {
  return { actor_id: user, proposal_id: proposal, proposal_version: 2, can_create: true,
    unresolved_blockers: items.filter(q => q.current.blocking && q.current.state !== 'DECIDED').length,
    read_at: '2026-11-25T10:01:00Z', questions: items, next_after: null };
}
export function questionHistory(user = product, value = question()): ProductQuestionHistory {
  return { actor_id: user, proposal_id: proposal, question: value, read_at: '2026-11-25T10:01:00Z', revisions: [structuredClone(value.current)], next_after: null };
}
export function questionReceipt(request: { request_id: string } | ProductQuestionChange, user = product, create = false): ProductQuestionRecorded {
  return { id: '00000000-0000-4000-8000-000000000085', actor_id: user, proposal_id: proposal, question_id: questionId,
    request_id: request.request_id, version: 'expected_version' in request ? request.expected_version + 1 : 1,
    kind: create ? 'OPEN' : 'action' in request ? request.action.kind.toUpperCase() as ProductQuestionRecorded['kind'] : 'PROPOSE',
    recorded_at: '2026-11-25T10:00:30Z', replayed: false };
}
export function decidedQuestionHistory(body: ProductQuestionChange, user = product): ProductQuestionHistory {
  const q = question(), first = structuredClone(q.current), receipt = questionReceipt(body, user);
  q.current = { ...q.current, id: receipt.id, version: 2, kind: 'DECIDE', state: 'DECIDED', actor: { id: user, name: 'Product' },
    answer: body.action.kind === 'decide' ? body.action.answer : 'Answer', recorded_at: receipt.recorded_at,
    decision: { actor: { id: user, name: 'Product' }, recorded_at: receipt.recorded_at } };
  q.permissions.can_propose = false;
  return { ...questionHistory(user, q), revisions: [first, structuredClone(q.current)] };
}
