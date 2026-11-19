# Discussions

Implementation of specification REV-03–06 is in progress. The source-selection
engine, pure authorization rules and append-only database schema are available.
Authenticated mutation services, APIs, reviewer UI and required-check integration
are not yet connected.

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

## Default discussion policy

`specspace-domain::discussion` uses current membership and repository access for
every action. Any active non-Reader member can comment without being an assigned
editor. Only the original message author may edit or tombstone that message; even
an Admin does not gain permission to rewrite another person's words. Neither
action withdraws an official review decision or resolves a thread.

A thread's creator or a current Reviewer, Maintainer or Admin may resolve or
reopen it. The creator or a current Maintainer/Admin may change its blocking
status. When an independent Reviewer/Maintainer/Admin creates or enables a
blocking requirement, the thread retains that provenance permanently. The
proposal author cannot resolve or unmark that thread, including when the author
is an Admin. This conservative restriction remains after role changes or an
independent person's intervening resolution/unmarking; reopening preserves it.
The original creator can withdraw their own requirement while they retain
comment permission, including after a role change to Contributor. A Reader or
revoked member cannot mutate their former threads/messages.

An unresolved blocking thread blocks merge regardless of its snapshot's age or
whether any message was tombstoned. These pure rules neither update proposal
state nor alter official-review decisions. Database code must serialize them with
the final merge check and persist thread versions, resolution identity/time,
monotonic independent provenance, immutable comment revisions, replay receipts,
audit and outbox events atomically. They are not runtime merge enforcement yet.

Comment bodies preserve exact submitted text, with a limit of 10,000 Unicode
scalar values (PostgreSQL `char_length`), rather than UTF-8 bytes or JavaScript
UTF-16 code units. Whitespace-only text and NUL are rejected.

## Verified changed-file anchors

`ComparisonFileReader::load_discussion_anchor` connects source selection to the
existing immutable file reader. It resolves scope and manifest from its configured
server authorization, verifies provider bytes against the recorded size, blob and
SHA-256, then captures a selection from the requested before/after side. Renamed
and historical files retain their original path and merge-base/head commit; the
current proposal head is never substituted. An absent side cannot borrow the
other side, and an empty side has no selectable text line.

The opaque `VerifiedFileAnchor` retains complete actor/organization/proposal/
snapshot/provider-binding context and exact selected-side metadata. It cannot be
deserialized from a request. Input selections reject unknown fields, including
paths, quotes and hashes. Malformed section IDs and line bounds fail before any
source call. Processing uses the reader's shared capacity and overall deadline;
source and anchor computation both recheck current authorization, including cache
hits. A further explicit recheck is available for later awaits.

This layer grants read evidence only. A future mutation service must check comment
permission before source I/O and lock/recheck current write authorization, expected
snapshot/fingerprint and context when storing the thread. Configure this reader
with shared-review authorization for shared discussion operations, never the
private comparison-job namespace. Document/section anchors for unchanged files
still need an authorized full-book resolver; the source-selection engine already
supports their content independently of the changed-file manifest.

## Durable revisions

Migration 020 stores immutable thread origins and ordered revisions. Each revision
captures current blocking/resolved state and monotonic independent-reviewer
provenance. Message changes have a separate comment version: edits preserve prior
text, tombstones contain no replacement body, and a tombstoned message cannot be
resurrected. Thread history and current message projections will be derived from
these records; deleting a message never erases its thread or official decisions.

Database guards serialize revisions on the proposal, enforce current membership,
message ownership and thread policy, and reject stale versions. Origins bind
current repository settings and either recorded proposal refs or an immutable
READY snapshot and its exact file-side manifest. A source quote is plain text;
browser claims alone cannot construct the application's verified anchor proof.

Every committed revision requires its matching audit and content-free outbox
envelope. Origins cannot exist without their opening revision. Admission attempts
bind request IDs to actor/proposal/body hash and count even when later source
processing fails. A serialized ten-minute quota allows 60 attempts per actor and
600 per organization. Exact successful replay must be checked before charging a
new attempt. Origins, revisions, admissions and event envelopes are protected
against update/delete/truncate; event delivery timestamps may advance once.

These schema constraints are defense in depth. Application code must still load
fresh policy, validate full Unicode comment bodies, compare expected versions,
perform source work without a database transaction, and recheck the final scope.
No discussion HTTP endpoint or runtime merge enforcement is provided by the
schema alone.
