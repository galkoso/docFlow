import type { RawFileResponse, RawSegment } from '../../core/api/comparison-api';
import { RawScope } from './raw-state';
import { actor, proposal, snapshot, head, base, file } from './comparison.testing';

export function rawScope(): RawScope { return { actor, proposal, snapshot, head, base, content: 'c'.repeat(64), file: file() }; }
export function rawResponse(): RawFileResponse {
  const segment: RawSegment = { kind: 'delete', chunk_index: 0, old_start_byte: 0, old_end_byte: 3, new_start_byte: 0, new_end_byte: 0,
    old_start_line: 1, new_start_line: 1, old_start_column_bytes: 0, new_start_column_bytes: 0, continues_chunk: false,
    old_text: 'old', new_text: '' };
  return { proposal_id: proposal, snapshot_id: snapshot, head_sha: head, merge_base_sha: base, content_id: 'c'.repeat(64), file: file(),
    raw: { old_sha256: 'a'.repeat(64), new_sha256: 'b'.repeat(64), old_bytes: 3, new_bytes: 3, old_lines: 1, new_lines: 1,
      detailed: true, total_chunks: 2, cursor: { chunk: 0, offset: 0 }, next: { chunk: 1, offset: 0 }, returned_text_bytes: 3,
      segments: [segment] } };
}
