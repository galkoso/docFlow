# Product questions

Product questions are proposal-level business decisions, separate from formal
snapshot review approvals. Opening a question with an answer produces PROPOSED,
including when the caller is qualified in Product or the answer came from an
import. OPEN has no answer; PROPOSED has an unapproved answer; DECIDED records an
explicit Product decision. A blocking OPEN or PROPOSED question prevents merge.

An active organization member with comment authority can ask or propose an
answer. Deciding an answer requires current Reviewer/Maintainer/Admin authority
and Product qualification. Admin or Maintainer alone does not imply product
expertise. This decision does not substitute for the independent official
Product/Development approvals required on the snapshot.

The creator or qualified Product reviewer can reopen a proposed/decided question.
Reopening clears the current answer/decider while prior immutable revisions will
retain the earlier decision. A decided answer cannot silently be replaced with a
suggestion: reopen first, or record a new explicit Product decision. Only Product
authority can remove blocking status; adding it requires the creator or Product.

Closed, merged and provisioning proposals reject new mutations. During MERGING,
an action cannot introduce a new unresolved blocker. Nonblocking questions and
actions that do not introduce blockers remain possible. Text preserves original
UTF-8/CRLF bytes, rejects NUL/blank content and is limited to 10,000 Unicode scalar
values per question/answer.

Migration 030 stores immutable question origins and revisions, with exact
organization/proposal scope, recorded repository/branch/head/target and proposal
version. These are recorded database context, not fresh provider proofs. Human
and imported source labels are retained; the imported source-reference UUID is
only a provenance locator until the importer verifies it against an ImportSession.
It never grants approval. Answer revisions cannot silently rewrite question text.

The database independently checks current membership, Product qualification,
allowed state transitions, optimistic revision versions and the merge barrier.
The proposal row serializes revisions against merge transitions. Opening is
complete only with its matching first revision. Every revision must commit with
an exact append-only audit record and content-free outbox event; incomplete writes
roll back together. Origin/revision mutation and event replacement/deletion are
rejected. A nonce is unique per organization. Successful revisions are limited to
60 per actor and 600 per organization in ten minutes, with serialized admission.

`PgProductQuestions` now authorizes reads and writes from current organization
membership. Mutations lock the proposal, evaluate the domain transition, and
commit the revision/audit/event atomically. Exact actor/resource/body nonce
retries return the original receipt without applying another revision or charging
quota. Changed intents fail. Current action authority is checked before replay;
an old receipt can be observed after closure, while new mutations remain forbidden.
A receipt is not a current-state response: read the question after a write.

All proposal readers can inspect the current state and append-only history.
Question pages use stable ID cursors and return at most 20 entries. Revision
history uses ascending version cursors; appending later revisions does not rewrite
already displayed pages. Blocker totals cover every question, independently of
pagination. Current permissions and original recorded context accompany each
question. Lost creator access or an outdated origin does not resolve a blocker.

The authenticated, no-store API exposes GET/POST `/api/v1/proposals/{proposal}/questions`,
GET `.../questions/{question}` and POST `.../questions/{question}/actions`.
Creation accepts only a nonce, expected proposal version, text, blocking flag and
optional proposed answer. The server records HUMAN source and the session actor;
clients cannot supply a source, state, creator or decider. Mutations carry their
nonce, expected revision version and propose/decide/reopen/set_blocking intent.
POSTs require Origin/XSRF and actor binding. Create bodies are limited to 256KiB,
action bodies to 128KiB, with the domain's separate Unicode text bounds. Known
conflicts/authority failures are distinct from retryable storage failures; quota
and unavailable responses include bounded Retry-After guidance.

Stored readiness now reads the latest revision of every scoped question. An empty
complete set passes this gate; an unresolved blocking question remains pending;
missing state is unknown. A proposed answer, including an imported suggestion,
does not resolve it. This integration performs no Git IO or official voting.
The typed client preserves captured actor/resource IDs and request bodies.

The review page now exposes proposal-level question creation and explicit
propose/decide/reopen/blocking actions, including before the first shared snapshot.
Imported suggestions remain visibly unapproved. Original context, current state,
Product decider and immutable history are displayed as escaped text. Permission
hints guide controls; server authority remains mandatory for every mutation.

Drafts capture the proposal/question version. A conflict retains the text and
requires fresh reads and explicit adoption of the current version. Uncertain
writes retain the original immutable body and nonce for manual retry; a later
history read never confirms that request. Retry-After delays the retry control
without automatically sending writes. Navigation, reload and logout guard drafts
and uncertain attempts. Account/scope changes cancel reads and hide private data.

Stored readiness loads on request and refreshes after validated question,
discussion or review receipts. It binds to the selected actor/snapshot and clearly
states that provider refs remain unverified. Neither this view nor a Product
question decision grants formal approval or merge authority. Automated component
and integration tests pass; physical keyboard, visual RTL and 320px acceptance
remain pending because no browser was available.

Verified bundle-import writer/provenance, fresh provider verification,
required-check publication and merge integration remain open.
