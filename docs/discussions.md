# Discussions

Implementation of specification REV-03–06 is in progress. The source-selection
engine is available; discussion persistence, authorization, APIs, reviewer UI and
required-check integration are not yet connected.

## Source anchors

`specspace-document-engine::discussion_anchor::TextAnchor` captures a document,
stable section or an inclusive range of source lines. It records exact UTF-8 byte
and line positions, full-source and selected-range SHA-256 digests, and a plain
source quote limited to 1,000 Unicode scalar values. Quote truncation does not
truncate the selected range or its digest. Quotes require plain-text rendering.

Document and section selection checks the document's front-matter identity. A
section includes its stable marker, heading and all source through the next
heading marker, including images, code and blank lines. Duplicate or unmarked
headings fail section selection rather than producing an ambiguous anchor. A
section's extent ends at the next heading regardless of heading depth; a parent
heading does not implicitly select all child sections. Markers in code fences are
ordinary source. The engine operates on any supplied document, including one
unchanged in a review; it does not depend on the changed-file manifest.

Line ranges are one-based and inclusive, limited to 500 lines of a source no
larger than 2 MiB. An empty file has no selectable line. A final newline does not
create a phantom line. CRLF, Hebrew, combining marks, emoji and a missing final
newline retain their original bytes.

Anchors are constructed from source and cannot be deserialized as trusted
evidence. They never relocate by matching a line number, quote or section ID in a
new revision. Full-source equality is conservative: even a change outside the
selection requires the later application layer to report old-version context.

The engine does not establish repository access or Git provenance. Before a
discussion can be stored, the application must resolve source from the selected
authorized immutable snapshot, bind the anchor to organization/proposal/snapshot,
repository/provider binding, side, path and blob, and recheck current permissions
and the expected context after source I/O. Browser-supplied source, refs, paths,
hashes and quotes must never supply that proof. Historical anchors remain attached
to their original context, and outdated status must never clear a blocking thread.
