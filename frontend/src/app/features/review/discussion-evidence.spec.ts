import { validDiscussionComments, validDiscussionReceipt, validDiscussionThreads } from './discussion-evidence';
import type { DiscussionIntent } from '../../core/api/discussion-api';
import { discussionComments, discussionThreads, thread, comment } from '../../../testing/discussion-fixtures';
import { actor, other, product, proposal, files } from '../../../testing/review-fixtures';

describe('Discussion evidence', () => {
  it('accepts scoped reader and writer history and rejects foreign actors, proposals and duplicated threads', () => {
    expect(validDiscussionThreads(discussionThreads(), actor, proposal)).toBe(true);
    const v = discussionThreads(product);
    expect(validDiscussionThreads(v, product, proposal)).toBe(true);
    expect(validDiscussionThreads(v, actor, proposal)).toBe(false);
    expect(validDiscussionThreads(v, product, other)).toBe(false);
    v.threads.push(v.threads[0]); expect(validDiscussionThreads(v, product, proposal)).toBe(false);
  });
  it('requires coherent state, cursor, times and blocking counts', () => {
    for (const change of [
      (v: ReturnType<typeof discussionThreads>) => { v.next_after = thread; },
      (v: ReturnType<typeof discussionThreads>) => { v.threads[0].resolved = true; },
      (v: ReturnType<typeof discussionThreads>) => { v.threads[0].last_activity_at = '2026-11-20T00:00:00Z'; },
      (v: ReturnType<typeof discussionThreads>) => { v.unresolved_blockers = 0; },
      (v: ReturnType<typeof discussionThreads>) => { v.threads[0].repository_binding_changed = true; },
    ]) { const v = discussionThreads(); change(v); expect(validDiscussionThreads(v, actor, proposal)).toBe(false); }
  });
  it('requires immutable source hashes, safe paths and bounded selections', () => {
    const v = discussionThreads(), source = files().files[0].after!;
    const anchor = { kind: 'file' as const, file_id: files().files[0].id, side: 'after' as const, source,
      text: { source_sha256: source.sha256!, range_sha256: 'e'.repeat(64), start_byte: 0, end_byte: 4, start_line: 1, end_line: 1,
        quote: 'test', quote_truncated: false, selection: { kind: 'lines' as const } } };
    v.threads[0].origin.anchor = anchor; expect(validDiscussionThreads(v, actor, proposal)).toBe(true);
    anchor.text.source_sha256 = '0'.repeat(64); expect(validDiscussionThreads(v, actor, proposal)).toBe(false);
    anchor.text.source_sha256 = source.sha256!; source.path = 'docs/../private.md'; expect(validDiscussionThreads(v, actor, proposal)).toBe(false);
    source.path = 'docs/a.md'; anchor.text.end_line = 501; expect(validDiscussionThreads(v, actor, proposal)).toBe(false);
  });
  it('rejects mismatched versions, foreign message capabilities and tombstones carrying deleted text', () => {
    const v = discussionComments(product);
    expect(validDiscussionComments(v, product, proposal, thread, 1)).toBe(true);
    expect(validDiscussionComments(v, product, proposal, thread, 2)).toBe(false);
    expect(validDiscussionComments(v, product, proposal, other)).toBe(false);
    v.comments[0].author.id = other; expect(validDiscussionComments(v, product, proposal, thread)).toBe(false);
    v.comments[0].author.id = product; v.comments[0].tombstoned_at = '2026-11-19T01:00:00Z';
    expect(validDiscussionComments(v, product, proposal, thread)).toBe(false);
    v.comments[0].body = null; v.comments[0].can_edit = false; v.comments[0].can_tombstone = false;
    expect(validDiscussionComments(v, product, proposal, thread)).toBe(true);
  });
  it('accepts receipts only for the actor, nonce, thread and expected message revision', () => {
    const intent: DiscussionIntent = { kind: 'action', thread, body: { request_id: other, expected_thread_version: 2,
      mutation: { kind: 'edit', comment_id: comment, expected_comment_version: 1, body: 'edited' } } };
    const v = { id: actor, actor_id: product, proposal_id: proposal, request_id: other, thread_id: thread,
      thread_version: 3, comment_id: comment, comment_version: 2, recorded_at: '2026-11-19T00:00:00Z', replayed: true };
    expect(validDiscussionReceipt(v, product, proposal, intent)).toBe(true);
    for (const patch of [{ actor_id: actor }, { request_id: actor }, { thread_id: other }, { thread_version: 2 }, { comment_version: 1 }]) {
      expect(validDiscussionReceipt({ ...v, ...patch }, product, proposal, intent)).toBe(false);
    }
  });
});
