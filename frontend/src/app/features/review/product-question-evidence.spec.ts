import { actor, product, proposal, other } from '../../../testing/review-fixtures';
import { question, questionPage, questionHistory, questionReceipt, questionId } from '../../../testing/product-question-fixtures';
import { questionText, validProductQuestions, validProductQuestionHistory, validProductQuestionReceipt, type QuestionIntent } from './product-question-evidence';

describe('Product question evidence', () => {
  it('accepts an imported suggestion without treating it as a Product decision', () => {
    expect(validProductQuestions(questionPage(), product, proposal)).toBe(true);
    expect(validProductQuestionHistory(questionHistory(), product, proposal, questionId)).toBe(true);
  });
  it.each(['actor', 'proposal', 'origin', 'source', 'state', 'permission', 'counter', 'cursor', 'duplicate', 'answer', 'time', 'version'])('rejects mismatched %s evidence', fault => {
    const page = questionPage(), q = page.questions[0];
    if (fault === 'actor') page.actor_id = actor;
    if (fault === 'proposal') q.proposal_id = other;
    if (fault === 'origin') q.origin.head_sha = 'not a sha';
    if (fault === 'source') q.source_reference = null;
    if (fault === 'state') { q.current.state = 'DECIDED'; q.current.decision = { actor: q.creator, recorded_at: q.current.recorded_at }; }
    if (fault === 'permission') q.permissions.can_decide = 'true' as never;
    if (fault === 'counter') page.unresolved_blockers = 0;
    if (fault === 'cursor') page.next_after = q.id;
    if (fault === 'duplicate') page.questions.push(structuredClone(q));
    if (fault === 'answer') q.current.answer = '\u2003';
    if (fault === 'time') q.current.recorded_at = '2027-01-01T00:00:00Z';
    if (fault === 'version') q.origin.proposal_version = 3;
    expect(validProductQuestions(page, product, proposal)).toBe(false);
  });
  it('requires an explicit decision revision and records the actual decider', () => {
    const q = question(); q.current.version = 2; q.current.kind = 'DECIDE'; q.current.state = 'DECIDED'; q.permissions.can_propose = false;
    q.current.decision = { actor: q.current.actor, recorded_at: q.current.recorded_at };
    expect(validProductQuestions(questionPage(product, [q]), product, proposal)).toBe(true);
    q.current.decision.actor = { id: other, name: 'Forged decider' };
    expect(validProductQuestions(questionPage(product, [q]), product, proposal)).toBe(false);
  });
  it('rejects revision gaps and a current snapshot that contradicts the final history revision', () => {
    const page = questionHistory(); page.question.current.version = 2; page.question.current.kind = 'PROPOSE';
    expect(validProductQuestionHistory(page, product, proposal, questionId)).toBe(false);
    page.revisions.push(structuredClone(page.question.current)); page.revisions[1].id = other;
    expect(validProductQuestionHistory(page, product, proposal, questionId)).toBe(false);
  });
  it('uses Unicode scalar bounds and preserves byte-relevant whitespace', () => {
    expect(questionText('🦀'.repeat(10000))).toBe(true); expect(questionText('🦀'.repeat(10001))).toBe(false);
    expect(questionText('\u2003')).toBe(false); expect(questionText('a\0b')).toBe(false); expect(questionText(' a\r\n')).toBe(true);
  });
  it('binds receipts to the exact actor, question, nonce, action and expected version', () => {
    const intent: QuestionIntent = { kind: 'change', question: questionId, body: { request_id: other, expected_version: 1, action: { kind: 'decide', answer: 'Explicit answer' } } };
    const receipt = questionReceipt(intent.body);
    expect(validProductQuestionReceipt(receipt, product, proposal, intent)).toBe(true);
    for (const fault of [{ question_id: other }, { actor_id: actor }, { request_id: actor }, { version: 1 }, { kind: 'OPEN' as const }]) {
      expect(validProductQuestionReceipt({ ...receipt, ...fault }, product, proposal, intent)).toBe(false);
    }
  });
});
