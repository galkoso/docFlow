# Discussions

Implementation of specification REV-03–06 is in progress. The source-selection
engine, pure authorization rules, append-only database schema and authenticated
mutation services and member-scoped read/write APIs are available. Reviewer UI,
unchanged-document anchoring and required-check integration are not yet connected.

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

## Mutation service

`PgDiscussions::admit_open` validates text/selection intent and current write
permission, checks the expected proposal version or snapshot fingerprint, handles
exact recorded replay, and commits a durable admission. It returns an opaque
`OpenAdmission`, with no database connection held. A caller resolves any requested
file anchor through the shared immutable reader only after this admission.

`complete_open` accepts that admission and the requested opaque file proof. It
locks current proposal/membership/repository scope again, rejects intervening
scope changes, and checks the proof's actor, organization, proposal, snapshot,
provider binding, refs, selected side/file and selection. Missing, opposite-side
or differently selected evidence cannot open the thread. A historical snapshot
retains its historical refs even after the recorded proposal head advances.
Proposal-wide discussions work before the first saved review and require no
provider read. Snapshot-wide discussions also require no file proof.

`mutate` handles replies, author-only edits/tombstones, resolution, reopening and
blocking changes in one database transaction. Every new action compares the
expected thread version; edits/tombstones also compare the expected comment
version. Reply targets are scoped to the same thread. Tombstoned messages cannot
be edited back into existence. Discussion actions preserve proposal content,
proposal versions and official-review decisions; existing closed/merged content
does not become editable through this service.

Receipts bind the actor, request, proposal, thread/revision and optional comment
version. Request hashes bind the complete exact intent, including its target.
Exact successful replay checks current write access but consumes no new admission
or provider read, even after later thread/proposal changes. Failed/abandoned
source admissions stay charged and bind subsequent attempts to the same intent.
Audit failure rolls back thread/revision/event creation while retaining the
earlier admission. Reusing that admission after recovery yields one durable
receipt. Database proposal locks serialize competing expected-version updates.

UI, unchanged-document resolution and check/provider synchronization still need
integration. These services do not authorize a merge on their own.

## Current discussion reads

`PgDiscussions::threads` and `comments` use repeatable-read transactions with
current membership/user/repository locks. Readers can read; write capabilities
come from current role, message ownership and independent-blocker policy. They
are hints: every mutation checks policy again. Changed target configuration
preserves history while disabling unavailable actions. Revoked membership or a
disabled account never receives a body.

Thread pages contain 20 roots, newest creation first, with a proposal-scoped UUID
cursor. Unresolved blocker counts include every thread in the proposal, including
older pages, historical snapshots and tombstoned messages. Current comment pages
contain 20 messages in their original creation order. Edits do not reorder them;
tombstones retain identity/authorship/time and return no body. Prior text remains
in the immutable ledger and is not exposed by these current-message endpoints.
Resolution identity/time comes from the last resolution transition, never a later
reply's author/time.

Comment pagination requires the expected thread version with its cursor. Any
intervening reply/edit/state change rejects continued paging as stale; clients
must refresh rather than combine bodies from different versions. Cursors from
another thread or proposal are rejected.

`outdated_context` compares original head/target and snapshot-recorded base with
current recorded database refs, plus repository/branch binding changes. A
metadata-only edit does not change source context. Reads make no provider call
and do not claim live Git freshness. Original refs, source-side metadata, quote
and ranges remain intact when context changes; no source URL or new source
capability is minted. Outdated context never resolves a thread or clears a
blocking requirement. Three indexes in migration 021 support per-thread latest
revision, original-message order and resolution-transition lookups, including
the write guard's serialized version check.

The read HTTP routes are:

- `GET /api/v1/proposals/{proposal}/threads?after={thread}`
- `GET /api/v1/proposals/{proposal}/threads/{thread}?after={comment}&expected_thread_version={version}`

Both require the current session and `X-Expected-Actor`, reject unknown query
parameters, and return `Cache-Control: no-store`. Invalid cursors/queries return
400; lost scope returns 404; changed paging context returns 409. OpenAPI and the
generated client describe the response data, capabilities and cursor contract.

## Discussion write HTTP

- `POST /api/v1/proposals/{proposal}/threads` accepts `OpenThreadRequest`.
- `POST /api/v1/proposals/{proposal}/threads/{thread}/actions` accepts
  `ThreadMutationRequest` for reply, edit, tombstone, resolve, reopen or changing
  blocking status.

Both require the current session, captured actor, expected Origin and CSRF token;
unknown query/body fields are rejected and bodies are limited to 128 KiB. A first
action returns 201 and an exact successful replay returns its original receipt
with 200. A receipt does not claim that the thread is still at that version:
refresh current history before another action. Stale context/versions and
idempotency conflicts return 409; denied role/ownership/independence returns 403.

`open_thread` commits admission before source work and uses the shared review
file-reader namespace. Provider failures never write to Git or close the proposal.
Unverifiable source cannot become an anchor. Current authority is checked again
before storing the thread and before returning a late failure/replay. If another
identical request has already committed while a read fails, the exact receipt is
returned rather than losing that successful result.

New file-anchor creation respects the durable cooldown keyed by exact repository
connection, shared with official reviews through `provider_backoff`. A provider
429 persists both cooldown and a content-free `DISCUSSION_SOURCE_STOPPED` audit
before returning Retry-After. Delays have a minimum of one second, a default of
60 seconds, and a u32 upper bound; remaining delays are rounded up. Other actors
cannot bypass the saved delay, and checking it does not prolong it. General and
snapshot-wide discussions, replies, other actions and stored history remain
available without provider IO. Failed/abandoned admissions stay charged against
the existing actor/organization quotas.

Failure audits use the immutable admission identity and contain actor, proposal,
snapshot/request IDs and a bounded reason code, with no body, quote or provider
error text. Access loss during IO is recorded against its original authorized
admission without publishing any source. Cooldown/audit failure returns an
unavailable response instead of claiming the rate limit was durably recorded.
Unknown replies remain safe to replay with the same immutable intent; history
refresh never resubmits an action.

## Active merge barrier

Migration 022 and the mutation service share the proposal lock with merge
authorization. While state is `MERGING`, opening a blocking thread, reopening a
resolved blocker or marking an unresolved nonblocking thread blocking is rejected.
Commenting and transitions that do not introduce an unresolved blocker remain
available. Stored current-role capabilities reflect that barrier, including
`can_open_blocking_thread`; an existing action receipt can still replay without
introducing a new change. This prepares the discussion side of merge
serialization; the merge/check workflow itself remains a separate unfinished
integration.

## Discussion interface

The shared review page shows proposal-wide discussion history even before the
first review snapshot exists. Readers can inspect current messages and original
context. Current capabilities govern opening, replying, editing one's own message,
tombstoning, resolving, reopening and changing blocking status. Source quotes and
messages render as plain text. Outdated context, independent requirements,
tombstones and the last resolution actor remain visible.

New drafts capture the proposal version or selected immutable snapshot. Text-file
anchors capture the selected file and before/after side, with a document, stable
section ID or up to 500 original source lines. Only selection intent is submitted;
the server constructs the source proof. Message limits count Unicode scalars,
preserve whitespace and line endings, and reject blank/NUL bodies. Replies and
edits capture thread and message versions; adopting refreshed context requires an
explicit action while preserving the draft text.

Each write freezes one request ID and body. Network uncertainty, malformed receipts,
denials and rate limits preserve that exact attempt for explicit retry. Retry-After
never triggers automatic POSTs. History refresh uses GET only, and a verified
receipt clears the draft before loading current history. Known validation/context
failures preserve the text for correction. Abandoning an uncertain attempt requires
confirmation and warns the user to inspect history.

Route changes, file selection, reload, logout and page close protect unsent or
uncertain discussion work. Changing the actor, logout epoch or captured context
cancels outstanding work and removes private text. Scoped access loss clears the
surrounding review evidence. Metadata-only history refresh keeps discussion drafts.
The layout wraps controls and text, has visible keyboard focus and native labelled
inputs. Physical keyboard, RTL and 320px browser verification remains pending
because no browser connection is available in this workspace session.

## Complete-book source proofs

`ReviewBookReader` can now derive document, stable-section or bounded-line
anchors from the sealed complete head-book manifest, including unchanged
documents. Its authorization namespace is separate from changed-file reads;
book entries never become synthetic diff entries. Entry IDs bind repository,
frozen head and the exact source descriptor with the same identity encoding
used by publication.

Current membership, proposal scope and frozen repository connection are checked
before provider IO and again after connecting, reading and processing source.
The reader fetches only the manifest path at the sealed head. It verifies byte
count, Git blob identity and SHA-256, checks the stable document identity against
actual front matter even for line selection, and captures original source ranges
and bounded quotes. CRLF source remains byte-for-byte intact. Legacy snapshots
without complete-book evidence return not-ready rather than borrowing current
source or reconstructing a document from the diff.

`VerifiedBookAnchor` has no deserialization, serialization or public constructor.
It retains both current authority context and immutable source/snapshot identity;
later awaits require a fresh recheck before the mutation transaction's own locked
authority check. Read authority does not grant comment permission. Complete-book
and changed-file readers share the existing two-source capacity and deadline;
CPU work keeps its permit through cancellation. Provider backoff remains explicit
for the mutation orchestrator. These proofs are not yet accepted by discussion
writes or exposed through document browsing/UI; that integration remains pending.
