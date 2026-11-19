import type { DiscussionComments, DiscussionThreads } from '../app/core/api/discussion-api';
import { actor, product, proposal, metadata } from './review-fixtures';

export const thread = '00000000-0000-4000-8000-000000000080', comment = '00000000-0000-4000-8000-000000000081';
export const discussionThreads = (viewer = actor): DiscussionThreads => ({
  actor_id: viewer, proposal_id: proposal, proposal_version: 5, recorded_head_sha: metadata().metadata.summary.head_sha,
  recorded_target_sha: metadata().metadata.summary.target_sha, can_comment: viewer === product, can_open_blocking_thread: viewer === product,
  unresolved_blockers: 1, read_at: '2026-11-19T12:00:00Z', next_after: null, threads: [{
    id: thread, proposal_id: proposal, creator: { id: product, name: 'Product reviewer' }, created_at: '2026-11-19T00:00:00Z',
    version: 1, comment_count: 1, blocking: true, resolved: false, independent_requirement: true, resolution: null,
    last_activity_at: '2026-11-19T00:00:00Z', outdated_context: false, repository_binding_changed: false,
    permissions: { can_comment: viewer === product, can_resolve: viewer === product, can_reopen: false, can_toggle_blocking: viewer === product },
    origin: { repository_id: metadata().metadata.summary.repository_id, branch: 'change/25-review', proposal_version: 4,
      head_sha: metadata().metadata.summary.head_sha, target_sha: metadata().metadata.summary.target_sha,
      snapshot_id: metadata().snapshot_id, fingerprint: metadata().fingerprint, merge_base_sha: metadata().metadata.summary.merge_base_sha,
      anchor: { kind: 'proposal' } },
  }],
});
export const discussionComments = (viewer = actor): DiscussionComments => ({
  actor_id: viewer, proposal_id: proposal, read_at: '2026-11-19T12:00:00Z', thread: discussionThreads(viewer).threads[0], next_after: null,
  comments: [{ id: comment, author: { id: product, name: 'Product reviewer' }, version: 1, created_at: '2026-11-19T00:00:00Z',
    edited_at: null, tombstoned_at: null, reply_to_id: null, body: '<script>bad()</script> שלום 🌍',
    can_edit: viewer === product, can_tombstone: viewer === product }],
});
export const bookDiscussionThreads = (viewer = actor): DiscussionThreads => {
  const page = discussionThreads(viewer);
  page.threads[0].origin.anchor = { kind: 'book_document', entry_id: 'e'.repeat(64),
    source: { path: 'docs/unchanged.md', blob_oid: 'a'.repeat(40), sha256: 'b'.repeat(64), bytes: 11,
      executable: false, document_id: 'DOC-UNCHANGED', catalog_position: 0 },
    text: { source_sha256: 'b'.repeat(64), range_sha256: 'b'.repeat(64), start_byte: 0, end_byte: 11,
      start_line: 1, end_line: 1, quote: '<img src=x>', quote_truncated: false, selection: { kind: 'document', document_id: 'DOC-UNCHANGED' } },
  };
  return page;
};
