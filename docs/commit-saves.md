# Git save boundary

The editor currently provides local source and private recovery. The backend now
has a durable Git save worker and authenticated submission. Connecting the
editor interaction is the next integration stage.

## Implemented: pinned commit evidence

`GitProvider::commit_receipt` reads one exact immutable commit object. The GitHub
adapter checks the returned SHA against the requested SHA and returns its tree,
parents, complete message and UTC committer timestamp. Messages are bounded at
64KiB, parent lists at 16; malformed hashes/dates, repeated/self parents and NUL
messages are rejected. Ordinary reader metadata stays independent of these
extra reconciliation fields.

`CommitReceipt::matches_operation` checks one exact expected parent and a final
two-line `SpecSpace-Operation` / `SpecSpace-Actor` trailer block. Both UUIDs must
match the journal intent. Duplicate or reserved fields in earlier text, extra
trailer/footer lines, nil identities and self-parent results fail the match.
CRLF message framing is accepted. User commit messages cannot supply reserved
identity fields; the adapter supplies them from the server-owned request.

This match is evidence of operation identity only. It does not verify intended
file contents, current authorization, proposal version, branch reachability, or
whether the observed commit has been published into the recorded proposal state.
No worker may equate matching trailers or a changed branch head with a completed
save.

The existing adapter performs one atomic `createCommitOnBranch` request with the
exact expected head and all file changes. It does not retry uncertain mutations.
The new evidence read likewise performs no writes.

## Implemented: candidate Markdown validation

`save_validation::validate` checks a complete candidate book, including all
`docs/*.md` files outside the catalog. Catalog mismatches, duplicate identities,
heading identity/title problems, orphan section markers, disallowed link schemes, broken
document/section references and absent image paths block a draft plan. Section
identity checks include nested and empty headings; incoming links in untouched
chapters are checked against the complete new book. Local fragments, `ss:` IDs
and repository document paths resolve to exact document ownership. URI escapes
are decoded once with strict UTF-8 and canonical repository-path validation.
External URLs are never fetched.

Protected HTML/unsupported heading levels remain visible warnings rather than
being removed from source. Missing image alt is also a draft warning; it still
must block review/merge as required by policy. The checker returns the exact set
of referenced assets. Path existence is not media-byte verification: the worker
must read these assets at the same pinned commit and verify hash, MIME and pixels
before it can write a save. This checker does not authorize an operation or
declare a proposal ready for review/merge.

Reports keep at most 1,000 findings while retaining full reported counts and
blocker counts, so late blockers cannot be hidden by warnings. Paths longer than
240 characters are rejected with explicitly truncated diagnostic paths. Internal
section/asset maps are excluded from diagnostic JSON. Catalog findings can be
streamed without first materializing every repeated diagnostic.

Reference processing has explicit resource bounds. Resolved-destination work is
limited to 64MiB across validation passes, and definition scanning to 128MiB
across the book. The checker also detects exhaustion of pulldown-cmark 0.13's
reference expansion budget, which otherwise turns later reference uses into
plain text. A callback probe counts original URL/title costs using short
destinations. It preserves first-definition-wins semantics, escaped labels,
multiline titles and quoted definitions; duplicate-definition rescanning is
bounded. Exceeding a budget yields an incomplete blocking report, never a
truncated valid result. These limits do not change source bytes.

## Implemented: existing-document save planning

`ValidatedSave` captures actor/proposal/repository UUIDs, expected commit and
proposal version, message, and up to 50 documents with exact base SHA-256 hashes.
The complete raw intent is fingerprinted with the `specspace-save-v1` protocol.
A 64KiB buffered hash writer avoids allocating another fully JSON-escaped copy.
Nil scope IDs, malformed hashes/IDs, duplicate document IDs, NUL content, invalid
messages and input beyond 2MiB per file / 10MiB per request fail validation.

`prepare` accepts only the matching pinned repository/commit, resolves file paths
through a valid catalog, checks exact original hashes and applies all candidate
changes together. It normalizes changed files to LF/NFC without reserializing
Markdown or unknown YAML fields. Unchanged files retain their original bytes.
Raw no-ops and edits normalizing back to Git produce an empty mutation list;
normalized output is checked against the atomic byte limit again. The complete
candidate then passes through book validation, so a section deletion cannot
leave an incoming link in an untouched chapter. Repairs of invalid old source
report unknown removed identities rather than claiming none were removed.

The resulting plan exposes read-only access to its original scope, fingerprint,
head/version, canonical changed source, hashes and removed identities. Per-file
flags identify normalization and normalization-only byte changes; they do not
claim a complete semantic/formatting diff. `file_changes` produces one bounded
batch of puts for the provider. This existing-document path cannot add arbitrary
files, edit the catalog, or remove a source file. File creation/import workflows
remain separate work. A plan is not authorization and does not call Git or the
database; assets and current context still require worker verification.

## Implemented: durable save journal and worker

`PgSaves` reserves an immutable actor/proposal/repository-bound intent with its
expected head, metadata version, branch and GitHub installation configuration.
Concurrent identical idempotency keys return the original job; changed intents
are rejected. One active Git mutation reserves the proposal, including UNKNOWN
operations whose jobs have exhausted retries. Current active membership and
owner/EDIT assignment (or audited Admin access) are checked at reservation,
execution and publication. Reader access never permits saving.

Migration 007 stores the private raw request separately from metadata-only job,
audit and outbox payloads. Database triggers prevent changing the original scope,
request or sealed manifest, and prevent deleting evidence rows. Definite terminal
outcomes purge the raw request; unresolved UNKNOWN operations retain it until
operational resolution. An administrative resolution flow and a bounded retention
policy for that unresolved private input remain required operational work.

The worker loads the complete pinned tree and Markdown snapshot, checks actual
Git blob identities and declared sizes, prepares all documents together and
validates the candidate book. Referenced images must be existing regular files
with valid content-addressed names, MIME, hash and pixels. Unsupported modes
cannot be edited. The worker seals a source-free manifest in PostgreSQL before
one atomic Git write. No-op saves neither write Git nor change proposal version
or approval state. The runtime starts one worker with a default 900-second task
limit, configurable through `SAVE_WORKER_TIMEOUT_SECONDS` (30–3600 seconds).

Jobs have expiring leases, heartbeats and fenced publication. A started operation
never becomes a fresh write again. Recovery observes bounded commit ancestry and
verifies the operation's exact parent, actor, complete tree, paths, file modes,
changed bytes and Git blob hashes. The verified commit must remain reachable
from the current branch. Later external descendants are allowed, while the
recorded result remains this operation's own commit. An old or diverged branch,
wrong evidence, permission revocation and uncertain verification remain explicit;
none permits another mutation. Provider backoff is honored before and after the
write, preserving UNKNOWN state during post-write verification failures.

Only sealed verification evidence can publish success. Publication atomically
updates the recorded proposal head/version, resets the state to DRAFT, emits a
review-invalidation outbox event, completes the job and purges private input.
A failed publication rolls back and is recovered by observation without a second
commit. Formal review snapshots and approval invalidation consumers are still
part of the forthcoming review service.

Twenty PostgreSQL integration scenarios cover atomic two-document saving,
no-ops, idempotency/concurrency, current ACL/configuration changes, lease fencing,
crash/restart, post-Git database failure, provider backoff, unknown outcomes,
forged evidence/content/modes, external descendants, bounded diagnostics,
malformed jobs, immutable evidence and image verification. The complete backend
workspace suite and Clippy pass. These deterministic provider tests do not claim
real GitHub or browser acceptance.

## Implemented: authenticated submission and status

`POST /api/v1/proposals/:id/commits` requires session authentication, Origin/XSRF,
`X-Expected-Actor` and `Idempotency-Key`. The body contains expected head/version,
message and existing documents with exact base hashes and Markdown. Repository
and provider context come from current server records. Intent hashing runs in a
bounded CPU slot; reservation rechecks access after that work. Decoded content
size violations return 413, malformed JSON 400, invalid intents 422, stale context
or a conflicting mutation/key 409. Accepted requests return only a 202 ticket and
job Location, never a saved SHA.

Identical replays return the original job even after a head advance or closure,
provided current access still permits the intent. They never retarget to a new
base. `GET /api/v1/jobs/:id` requires the captured actor header for save jobs and
current EDIT/owner/Admin scope; only the submitting actor can read private save
results. It projects bounded typed diagnostics and verified outcome metadata,
without raw source or provider configuration. Success requires both the operation
and job publications; no-op outcomes carry no commit timestamp. UNKNOWN retains
an explicit reconciliation phase even after retry exhaustion.

Six PostgreSQL HTTP scenarios cover those boundaries, including account changes,
CSRF, private polling, changed retries, closed-state replay and status publication.
Synthetic outcome rows test HTTP projection; Git proof itself is exercised by
the worker's twenty scenarios. Generated OpenAPI/client types and explicit-actor
Angular methods expose the contract; a client test checks exact retry identity,
key and source after an unrelated poll uses another actor.

## Required next implementation

The editor's Git-save interaction must flush active fields, preserve later local
edits and frozen retries,
and show a verified SHA/time only after publication succeeds. Formal review,
operational reconciliation, unresolved-input retention and real-provider/browser
acceptance remain open.

Provider reference: [GitHub Git commit objects](https://docs.github.com/en/rest/git/commits?apiVersion=2026-03-10#get-a-commit).
