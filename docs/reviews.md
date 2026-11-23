# Formal review preparation

Review configuration is editable at `/changes/:id/review`; its contract is in
[`review-configuration.md`](review-configuration.md). Server-side preparation,
the submission journal, durable worker and atomic shared snapshot persistence
and authenticated submission/status/shared snapshot endpoints are implemented.
The submission/shared-snapshot frontend is available at
`/changes/:id/review/snapshots`. Shared diff bodies, decisions, discussions and
provider PR/check orchestration remain open.

## Current server context

`ReviewExpectation` contains only the expected saved head, selected target, and
proposal/configuration/policy versions. `ReviewAuthorization::resolve` loads
`ReviewScope`, pairing the exact comparison repository binding and refs with
current review metadata and memberships. These types cannot be deserialized from
an HTTP request.

`PgReviewAuthorization` resolves the actor's active organization membership and
current edit authority. Readers and unassigned non-Admins cannot submit. Admins
remain confined to their organization. The proposal must be editable, all saved
head/version expectations must match, its owner must remain an active non-Reader,
and no unresolved content/submission/merge mutation may be pending. Repository
provider, numeric connection identity and main-branch configuration are validated
through the same resolver used by verified comparisons.

The transaction locks the proposal before reading its immutable configuration
revision. A waiting read sees the revision committed by the writer, while an old
expectation becomes stale. Reviewer memberships are loaded fresh: absent members
remain absent, disabled users are inactive, and role/domain changes are retained
for the preparation rules to reject. Historical assignments are not erased.
Domain order and duplicate qualifications are canonicalized so equivalent arrays
compare equally. Display-name changes are outside authority comparisons.

Scope equality supports rechecks across asynchronous work. The resolver makes no
Git calls and publishes no jobs or snapshots. An expected target is still a
selection: the comparison loader and `PreparedReview` must prove its lineage and
current-main match. The full preparation path must also validate all source and
media, recheck authorization/refs, and fence its eventual publication transaction.
An earlier successful scope read alone cannot authorize later publication.

## Bounded comparison and plan orchestration

`ReviewPreparer` resolves current review scope before loading any Git content,
loads the exact saved head/selected target through `ComparisonReader`, and
requires its frozen comparison context to match the resolved review scope.
Full edit/reviewer metadata is checked again after loading, including failed
comparison reads.

`PreparedReview` runs on the blocking pool with the verified complete comparison.
The two-slot preparation limit remains held through that CPU work when its caller
cancels or times out. A deadline bounds each preparation or later recheck. There
is no automatic retry, Git write, or branch/PR creation in this path.

After processing, authority is checked before either the plan or its validation
diagnostics can be returned. A fresh provider connection binds the same numeric
repository/install identity; branch and main refs must still match the expected
head/target, and authority is checked after connecting and reading refs. A later
`recheck` repeats authority/ref observations without reloading source.

The non-deserializable `ReviewCandidate` retains the source comparison and full
plan. Its media requirements become verified receipts through the next step.

## Sealed media verification

`ReviewPreparer::verify_media` consumes the candidate and returns an opaque
`VerifiedReview` only after every required image passes. It checks current scope
and exact provider binding/refs before scanning, reads one image at a time at
the saved head, and rechecks authority after each binary read and CPU decode.
Final authority/ref checks follow the complete scan and digest calculation.

Each source must match the planned byte count and Git blob OID, fit the 5 MiB
per-file limit, and match its content-addressed SHA-256 path. The existing media
engine verifies PNG/JPEG/WebP content and extension, bounds dimensions/allocation,
applies orientation, and creates a metadata-free PNG display derivative.
Verification drops original and display bytes. Receipts retain only path,
blob OID, source length/hash, display hash, and display dimensions. They cannot
be deserialized or constructed by a caller. Referenced assets are deduplicated;
unreferenced newly added images are also verified. Text-only candidates have an
explicit empty media set. Other unchanged opaque files are not read.

The scan shares the preparer's two-slot limit and deadline. Its permit stays in
CPU closures through cancellation. A scan returns provider errors with their
backoff; the durable journal decides whether a later attempt is safe. Invalid media identifies the failing asset path
with a bounded error code; authority changes take precedence over diagnostics.
A failure never returns partial evidence. Large scans currently restart rather
than resume; checkpointed media work remains open.

`VerifiedReview::fingerprint` hashes the planned content identity and media
validation version. It is independent of actor, state-only changes, or incidental
PNG encoding details. A separate `evidence_digest` also binds the canonical
receipt set, including generated display hashes. Decoder-policy changes must
bump `MEDIA_VALIDATION_VERSION` and therefore change review identity. Content
findings, including missing alt text, remain attached to the candidate for later
review/merge gates; decoding an image does not satisfy those gates.

## Durable submission journal

Prepared review plans now include mandatory source requirements. A stable section
requirement retains exact before/after source descriptors and original byte/line
ranges; sections with unchanged raw source do not become new requirements.
Front matter, unsectioned text, paths/modes/catalog positions, section ordering,
catalog files and media changes remain explicit review work. Added/deleted
sections distinguish absent section sides from existing file sides.

Unstructured or excessive sections use an explicit whole-file requirement,
retaining all original evidence. A bounded global manifest reserves at least one
requirement for every changed file; exceeding the section budget replaces that
file's section set with complete whole-file review work instead of dropping it.
Plan fingerprint version 3 includes the exact requirement set. Matching unit IDs
do not transfer visits or approvals across snapshot/policy contexts. These data
are preparation evidence, not a visit receipt or an approval.

Migration 025 seals the complete ordered requirement set in the same fenced
publication transaction. Header IDs/counts, per-file coverage, source descriptors,
stable identities and byte/line bounds must agree before a snapshot becomes
ready. Rows cannot be changed, extended, deleted or truncated after publication;
an outbox failure rolls them back with the snapshot. Existing snapshots retain
their original evidence as requirement version 0. They cannot be upgraded in place
or reused as a complete requirement publication; resubmission creates new sealed
evidence. This manifest does not assert visits, approvals or merge readiness.
The actor-bound `GET /api/v1/proposals/{proposal}/review/snapshots/{snapshot}/units`
returns the sealed ordered requirements in pages of up to 100 (default 50).
Offsets are bounded to 0–40,000, query fields are strict, and every read checks
current membership. Pages echo actor/proposal/snapshot/fingerprint/head identity,
remain pinned if the proposal advances, and contain descriptors and ranges only.
Legacy snapshots return `available: false` with no requirements. These reads need
no provider IO and do not record a visit or an approval. The typed Angular client
requests pages of 20. The review screen loads this list on demand, displays the
original before/after ranges and explicit section/file requirements, and opens
document links at the snapshot head or merge base. Missing sides and legacy
unavailability stay explicit. The `requirements` URL parameter restores paging
without changing the selected comparison or discussion draft. Actor/context
changes cancel reads and hide stale data; denied access purges the parent review
scope. This list exposes no visit checkbox, approval count or completion claim.

The internal mandatory-unit source reader re-derives each requirement from the
verified immutable file pair before exposing any text. This includes complete
whole-file fallbacks; a correctly hashed but truncated range cannot replace the
sealed work. It returns at most 64 KiB per UTF-8 window, preserves original CRLF
and protected source, and distinguishes an absent side from an empty span. The
opaque proof retains actor/proposal/snapshot/fingerprint, exact source and window
bounds; it can be rechecked after later awaits. It shares the file reader's
verified cache, source capacity and deadline. Binary requirements need separate
display verification. This internal proof is not a visit receipt or an approval;
Browser source controls verify these identities and window hashes before
rendering text or recording an acknowledgement.
PostgreSQL unit authorization resolves only the selected ready snapshot's sealed
manifest under current organization access and the exact repository/installation
binding. It preserves the snapshot's immutable head/base/fingerprint if proposal
metadata advances, while a proof captured under an earlier current scope fails
recheck. Legacy requirement version 0 returns unavailable authority; metadata
alone cannot be promoted to a source proof. This read path adds no migration.

Migration 026 adds separate immutable source attempts, deliveries and explicit
window acknowledgements. The internal service checks an independent qualified
assignment and current snapshot before source IO, charges failed attempts too
(600 per reviewer / 6,000 per organization in ten minutes), and respects the shared
provider cooldown. No transaction spans provider IO. A final locked check prevents
role, snapshot or repository changes from publishing stale delivery evidence.
Attempts expire after three minutes; deliveries allow acknowledgement for thirty
minutes. Stored delivery data includes exact source/range/window hashes, never
source bodies. Only an explicit actor/domain/snapshot-bound acknowledgement creates
a visit, atomically with audit and event records. An exact recorded retry remains
replayable after delivery expiry, subject to current authority; nonce reuse for
another intent is rejected. Neither a delivery nor one window acknowledgement
asserts whole-unit coverage or approval by itself. Server coverage assembles the
acknowledged ranges as described below; section approval remains a separate action.

The guarded HTTP boundary exposes
`GET /api/v1/proposals/{proposal}/review/snapshots/{snapshot}/units/{unit}/source`
with strict required `expected_fingerprint`, `domain`, `side` and `offset` query
fields. It checks the session and captured actor, returns a bounded verified
window/delivery with no-store headers, and never acknowledges it automatically.
`POST /api/v1/proposals/{proposal}/review/snapshots/{snapshot}/visits` requires
Origin, CSRF and actor binding plus a strict 4 KiB intent containing the delivery,
domain, fingerprint and immutable request nonce. New receipts return 201; exact
replays return 200. Unacknowledged expiry returns 410, scope/nonce conflicts return
409, and quota/provider cooldown responses include Retry-After. Provider failures
never return partial source or invented receipts. OpenAPI and the typed Angular
client describe source reads and explicit acknowledgements as separate calls.
The required-item panel now offers a captured source pane for the review domains
reported by the validated official-review read. It displays raw source inertly,
uses verified next/previous windows, and records only an explicit “read this
window” action. Scope changes and lost access hide/cancel private state. Pending
or uncertain acknowledgements retain their original nonce and delivery for
explicit retry, including after client-side delivery expiry. Retry-After causes
a visible wait, never an automatic POST. Reload, logout, route exit and unload
guards protect uncertain writes; list refresh/paging does not retarget a captured
source window. A separate explicit read below exposes server coverage; source
window acknowledgement alone supplies no whole-unit completion or approval.

`GET /api/v1/proposals/{proposal}/review/snapshots/{snapshot}/coverage` returns
the qualified caller's own domain-specific coverage, with captured fingerprint,
actor and snapshot binding. Query fields are strict; offsets are bounded to
0–40,000 and pages to 1–100 (default 20). The server unions acknowledged original
byte ranges per required side. Overlap, duplicate visits and empty tail reads
cannot hide a gap; an empty required span still needs an explicit acknowledgement.
A unit is viewed only when every present side is viewed. Binary/media sides
without a supported acknowledgement remain unsupported and unviewed. Summary
counts include the entire sealed requirement set, independently of the returned
page. Delivery without acknowledgement contributes nothing. Reads need no provider
IO and write no review evidence; migration 027 adds an index for this scoped read.
Historical counts remain pinned and are marked not current after a new snapshot
or policy context. Matching unit IDs do not transfer visits across snapshots or
domains. Legacy manifests return unavailable coverage, never synthesized completion.
The required-item panel now offers an explicit read of the caller's coverage in
the selected eligible domain. It shows complete, partial and unviewed source
sides, unsupported work and whole-unit completion, with totals for the complete
snapshot rather than just the page. Context and manifest evidence are validated
before display. Historical results are labelled; unavailable legacy evidence
never implies completion. Loading the panel or changing domains records nothing.
Only a verified acknowledgement receipt triggers a fresh coverage GET; counts
are never incremented optimistically. Refreshes preserve captured source windows
and uncertain acknowledgement nonces. Access denial clears the source and parent
requirement evidence, while account, snapshot, domain and page changes discard
stale responses. Transport failures offer explicit retry without timer retries.

Official-review history also reads acknowledged coverage in its existing repeatable
read transaction, using the same original-range union as the coverage endpoint.
`unviewed_domains` lists eligible caller domains with incomplete or unavailable
coverage. `unviewed_content_warning` is the aggregate warning; it also stays true
when the caller has no eligible domain. Legacy evidence, unread gaps, missing
present-side acknowledgements and unsupported media keep the warning active.
The approve-all form uses the selected domain's warning and labels complete read
evidence as of the last history read. Refreshing decision history obtains fresh
coverage without changing a draft or submitting a decision. Read completion does
not enable an otherwise unavailable approval. If an uncertain decision retains a
domain that is no longer eligible, the form shows its read evidence as unknown
and preserves the original request. Explanation, current authority and
provider freshness are checked independently on submission; read evidence neither
creates a section approval nor publishes content. Section decisions and
policy-check assembly remain separate integration stages.

Migration 028 moves the original-range union into `review_unit_coverage`, shared
by coverage reads, approve-all warning reads and the individual approval guard.
An optional exact unit selector bounds an individual check without changing the
global-count behavior of paged reads. `review_unit_approvals` stores immutable
explicit actions against a sealed unit and snapshot/fingerprint, caller and
domain. The database requires current qualified assignment and complete
acknowledged coverage of every present source side; absent sides need no visit,
while unread gaps and unsupported binary/media work cannot pass this guard.
Approvals require matching audit and outbox evidence in the same transaction.
Approval rows and event scope cannot be rewritten or removed.

The internal `PgUnitApprovals` service provides explicit approval and scoped
paged status. It locks current authority, validates the nonce and captured scope,
serializes organization admission and enforces successful-write quotas of 600 per
reviewer and 6,000 per organization in ten minutes. An identical nonce replays the
original receipt; a changed intent cannot retarget it. A separate explicit action
adds history but still counts only one approved unit. Exact historical replay is
observation of the stored receipt and still requires current eligibility; a new
action requires the current snapshot context. Historical counts remain labelled
not current, and matching unit IDs in a new snapshot inherit no approvals. Legacy
manifests remain unavailable. The service reads no provider data, changes no refs
or proposal state, and creates no official snapshot decision. Review-panel
controls and assembly of final approval requirements remain unfinished.

`GET /api/v1/proposals/{proposal}/review/snapshots/{snapshot}/unit-approvals`
exposes the caller's scoped status with strict fingerprint/domain queries and
bounded paging (default 20, maximum 100; offsets 0–40,000). `POST` to that path
accepts only the captured request nonce, fingerprint, domain and unit ID, with a
4 KiB limit and session actor/Origin/CSRF checks. It returns 201 for a new explicit
approval and 200 for exact receipt replay. Unread requirements and unavailable
legacy evidence are distinct 409 outcomes; conflicting nonce reuse also returns
409. Successful-write quotas return 429 with a 600-second Retry-After, while
storage failures return a bounded retryable 503 and roll back all approval
evidence. Both operations return no-store responses. Generated client methods
keep status GETs separate from frozen explicit POST intents.

Migration 010 stores immutable submission expectations, the actor/proposal/repo
binding, and a canonical authority digest. `PgReviewSubmissions::reserve` checks
current edit authority, then replays an identical actor-bound idempotency key or
reserves one `SUBMIT` operation and `SUBMIT_REVIEW` job in a transaction with its
audit event. The request hash binds organization, actor, proposal, exact head/
target and all three expected versions. Hashes normalize SHA case. Job payloads
contain only the operation ID; proposal text, media and authority data are not
copied into jobs or status responses.

Replay still requires current edit authority but remains observable after later
head/version changes or closure. A reused key with different expectations fails.
New requests require current saved head/versions and no pending mutation.
Database triggers freeze request, operation and job identity, reject orphan jobs,
and preserve history against updates/deletion/truncation. The existing one-active-
mutation index serializes submission with saves, imports and merges.

Claims and heartbeats bind worker, job, operation and attempt number to an
unexpired lease. Expired read-only work can be reclaimed; an old attempt is
rejected even when the same worker ID claims it again. `authorization(lease)`
returns a resolver which excludes only that exact validated pending operation
from the busy check, loads current actor/reviewer metadata, compares the frozen
authority digest, and rechecks the lease clock after authorization. A blocked
membership read cannot return authority after its lease expires. Scope/job lock
order is consistent, and provider calls never hold these transactions.

`ReviewPreparer::with_authorization` installs that resolver while sharing the
root preparer's IO/CPU limit and provider factory. Revocation and metadata changes
stop preparation/heartbeats. A still-owned read-only lease can record a definite
failure and release its reservation after revocation. Transient failures retain
bounded retry delays. Exhausted read-only work fails once and releases its
mutation slot. Once an operation is provider-started or unknown, this journal
neither reclaims it nor marks it failed; status requires reconciliation. Generic
job helpers cannot lease or complete submission jobs.

Status is private to the original actor with current edit access. It returns only
validated state/phase/error fields and rejects inconsistent or unbacked success.
Success requires a persisted publication receipt and matching snapshot/job/
operation outcome. Its phase is `SNAPSHOT_READY`; it does not imply a PR exists,
checks passed, or any reviewer approved. Fresh provider observations cannot lock
Git against later external changes, so approval and merge gates must also recheck
their own exact refs.

## Atomic shared snapshot publication

Migration 011 and `PgReviewSnapshots` persist only sealed `VerifiedReview`
evidence. A candidate must match the submission's frozen authority digest.
Publication renews the read-only lease, rechecks current edit/reviewer authority
and the exact provider refs through a lease-bound preparer, then opens a short
transaction. Provider calls never hold that transaction. It locks the proposal
before the job/operation, resolves full authority again, and requires the same
scope as the verified candidate. A final worker/attempt/clock fence protects
completion after the evidence and outbox writes.

Snapshots contain frozen proposal/repository metadata, the full comparison file
manifest, reviewer assignments, validation findings and media receipts. They do
not contain raw Markdown or source/display image bytes. File descriptors retain
their original IDs and optional hashes; separate media receipts record the
verified image hashes and dimensions. Files and receipts are inserted in bounded
batches with contiguous ordinals. A one-time seal compares both complete ordered
manifests. Deferred constraints reject unsealed, incomplete or orphan publication
transactions. Published headers, children and submission receipts are immutable,
including against deletion, late insertion and truncation.

New publications also seal the complete head-book document manifest in catalog
order, including documents unchanged by the proposal. Each entry retains only
its source descriptor and a deterministic identity bound to repository, head and
that descriptor. Plan fingerprint version 2 includes the complete manifest;
changed-file counts and the diff still represent only actual changes.

Migration 023 stores this manifest in the same fenced publication transaction.
Its guards enforce unique document IDs, paths and entry identities, contiguous
catalog positions, supported paths, exact descriptor fields, hashes, sizes and
the 1,000-document/20-MiB limits. Sealing also checks each changed after-document
against the book manifest. A missing manifest prevents publication, and sealed
manifests cannot be extended, updated, deleted or truncated. An outbox failure
rolls the manifest back with all other publication evidence.

Pre-023 snapshots retain their original evidence and manifest version 0. They
remain readable, but a complete-book read explicitly returns no manifest;
it never reconstructs one from the diff or a new Git read. They cannot be reused
as a new complete-book publication. Current membership and proposal/snapshot
scope protect stored manifest reads without provider IO. Verified document,
section and line source proofs now support complete-book discussion creation,
including unchanged documents; see `docs/discussions.md`. Paged document browsing
and composer selection use this sealed evidence, as described below.

`GET /api/v1/proposals/{proposal}/review/snapshots/{snapshot}/documents` now
returns sealed head-book descriptors in catalog order, in pages of 1–100
documents. It requires a current session and captured actor, rejects unknown or
invalid paging queries, and returns no-store responses. The response echoes
actor, proposal, snapshot, fingerprint and frozen head; changing current proposal
refs does not change these pages. Reads do not contact Git. Legacy snapshots
return `available: false` with no entries, distinct from an available empty book.
The generated client requests bounded pages for the composer picker. The shared
review discussion panel loads this list on demand and offers pinned document
reading links for all members. Writers can select unchanged documents and keep
the exact entry/snapshot while composing; document paging does not retarget the
draft. Actor/context loss clears private list/draft state, while route, reload,
logout and unload guards protect unsent or uncertain work.

An unchanged **active** snapshot can be reused in review/approved/changes-requested
states. Reuse preserves proposal version and state, including `APPROVED`; it
keeps the original immutable evidence digest. Otherwise a new snapshot becomes
active, the selected target is recorded, proposal version increments and state
becomes `IN_REVIEW`. A historical fingerprint is never globally reused. Changing
head, target, base, review configuration or proposal review metadata clears the
active pointer; retained `APPROVED` state is demoted. A provider-binding change
also clears pointers, increments affected proposal versions and demotes approval.
Restoring an old head or binding therefore cannot reactivate historical approval.
Policy versions cannot move backward; current metadata checks compare policy.

The snapshot, immutable submission result, active proposal pointer, successful
job/operation, audit event and `REVIEW_SNAPSHOT_PUBLISHED` outbox event commit
together. The event explicitly requires PR synchronization and grants no
approvals. PR synchronization is still pending implementation. A lost reply or
concurrent publication can replay the same durable outcome with current original-
actor edit access, without a live lease or new Git reads, even after later head/
version changes or closure. A rolled-back attempt leaves no partial evidence.

Organization members, including Readers, can read shared snapshot metadata and
complete file descriptors in pages of 1–100. Every read checks current membership,
active user status and proposal/repository scope. History remains readable after
closure, content changes and provider reconfiguration, without provider calls.
`current_metadata` compares the active pointer and current database refs, versions
and binding; it is neither a fresh Git observation nor approval/merge readiness.
Job status remains private to the original actor with current edit access. These
store methods also support the authenticated endpoints below. Shared raw/
readable/image body reads remain open.

## Durable snapshot worker

`ReviewWorker` claims only the journal's pending, read-only submission jobs. It
installs lease-specific authority on the shared preparer, loads the exact saved
comparison, verifies all required media, then invokes atomic snapshot publication.
The root preparer's capacity and frozen provider factory are shared with its
lease-bound clones. Configuring a comparison source also configures review
preparation; an unbound book source never substitutes for this capability.

The API starts one worker and shuts it down with the other background tasks.
`REVIEW_WORKER_TIMEOUT_SECONDS` defaults to 900 and permits 30–900 seconds. The
preparation phase uses a 60-second lease with 15-second authority-checked renewals.
Renewal waits share the processing deadline. Publication runs without a competing
heartbeat, renews its own lease, and fences its final transaction. The overall
processing deadline also bounds publication. Failure cleanup uses the journal's
short database transaction limits. Shutdown cancels pending reads and leaves a
running lease available for recovery after expiry. Detached CPU work retains
its preparation permit until it finishes and cannot publish by itself.

Transient read failures and processing timeouts receive bounded exponential
backoff; provider retry-after values up to one day are honored. Longer requested
delays stop automatic retries and leave a failed job for an explicit new request.
Eight attempts exhaust the journal's retry budget and release its mutation slot.
Invalid content/media, moved refs, stale metadata and revoked authority fail
definitively. Persisted job results contain bounded error codes, without private
validation findings, paths or source text. A replaced/expired attempt cannot fail
or publish over its new owner. Database publication uncertainty leaves work for
receipt observation or expired-lease recovery; it does not force a failed outcome
over a possibly committed result. Started/unknown provider operations remain
outside the worker's claim and failure paths.

This worker performs only Git reads. It does not create PRs, publish checks,
grant approval or merge. A crash before snapshot commit can repeat reads; a crash
after commit leaves the successful job and immutable receipt observable. Scans
currently restart rather than resume.

## Authenticated HTTP contract

`POST /api/v1/proposals/:proposal/submit` accepts only the five fields in
`ReviewSubmissionRequest`: exact saved head, selected target, and expected
proposal/configuration/policy versions. It requires the captured `X-Expected-Actor`,
the session's CSRF token, same-origin `Origin`, and one valid `Idempotency-Key`.
The body limit is 8 KiB. Unknown/duplicate fields, unknown query parameters,
malformed JSON and invalid SHA/version values fail without queueing work.
Current edit authority and server scope are checked before reservation. A reused
key with changed expectations conflicts; identical replay returns its original
ticket even after later changes or closure, while requiring current edit access.

Acceptance returns **202**, proposal/operation/job IDs, `Location: /api/v1/jobs/:id`
and `Retry-After: 2`. It claims acceptance only. Job reads for `SUBMIT_REVIEW`
require the original actor's current edit access and captured actor header. Other
members, including another Admin, cannot inspect this private job. Existing job
routes now reject unknown query parameters. Review status uses the journal's
validated phases and includes a typed `review` publication only for a backed
`SNAPSHOT_READY` outcome. It never returns arbitrary job payload/result JSON.
Snapshot readiness does not claim PR synchronization, approval or merge readiness.

All shared routes require a session, captured actor header, active user and current
organization membership, including for Readers. They are read-only and make no
provider calls:

| GET route under `/api/v1/proposals/:proposal/review/snapshots` | Response |
|---|---|
| Base path | Shared history, active snapshot pointer and current database proposal state/version |
| `/:snapshot` | Typed immutable metadata, validation findings, assignments and evidence identity |
| `/:snapshot/files` | Complete file descriptors, preserving original IDs and hashes |
| `/:snapshot/media` | Verified source/display hashes, dimensions and byte-count receipts |

History defaults to 20 entries and accepts limits 1–50. `after` is the immutable
snapshot ID returned as `next_after`; ordering uses descending creation time and
UUID. New publications do not shift or duplicate older cursor pages. A cursor
from another proposal/organization cannot select history there. File/media pages
default to 50 entries and accept 1–100, with `offset` bounded at 40,000/20,000
respectively. Responses include exact snapshot identity, total and `next_offset`.
Unknown, duplicated or out-of-range query fields fail. Empty history/media and
pages beyond the end are explicit empty results.

Metadata uses an owned, versioned read schema, separate from non-constructible
publication evidence. The snapshot UI-impact enum preserves the frozen uppercase
`YES`/`NO`/`UNDECIDED` representation. Header/summary counts and validation
completeness must agree before serving typed metadata. Snapshots retain original
creator and UTC timestamps. History stays readable after closure, head advance
and provider reconfiguration; `current_metadata` describes current database
metadata only. Source Markdown, image bytes, provider connection IDs and private
authority/request digests are not included. All routes, including errors and jobs,
use `Cache-Control: no-store`.

Migration 012 adds indexes for bounded public admission. New reservations serialize
within their organization: each actor may hold two unresolved submissions and
create ten new requests per ten minutes; the organization may hold twelve and
create sixty respectively. Terminal jobs still count in the recent-request
window. Started/unknown operations retain their unresolved slot. Exact replay is
checked before quotas and remains available at a limit. Rejected admission writes
no job/operation and returns **429** with `REVIEW_SUBMISSION_LIMIT` and
`Retry-After: 60`. The existing per-proposal mutation gate still prevents concurrent
save/submit/import/merge reservations.

OpenAPI and the generated TypeScript client describe these routes and read models.
PR/check processing, discussion/decision APIs and approvals remain open.

## Shared snapshot body authority

The shared file authorization adapter resolves only sealed `READY` review
snapshots and their immutable file manifests. It checks current active user and
organization membership, including Readers, on every authorization boundary.
An actor-private comparison snapshot or another organization's locator cannot
substitute for a shared snapshot. Provider repository identity, installation and
proposal branch must still match the frozen binding before any cached or uncached
body is returned. Metadata history remains available independently of that body
binding. Later head, target, policy or closure changes do not replace historical
source commits.

The existing verified file reader can fork authorization with a separate cache
while sharing its two provider/CPU read permits. It rechecks current authority
around IO, CPU processing and cache use. Shared after-image reads additionally
require the published validation version, source/display hashes and dimensions to
match the newly verified, metadata-free PNG derivative. A policy-version mismatch
fails before fetching after-image bytes. Before images retain exact pinned-source
verification.

Authenticated shared reads use
`/api/v1/proposals/{proposal}/review/snapshots/{snapshot}/files/{file}` with
`/raw`, `/readable`, `/readable/units/{unit}` or `/image?side=before|after`.
These routes use shared authority explicitly and never fall back to private
comparison snapshots. The snapshot is fixed in the path; alternative snapshot,
head, ref or path query fields and duplicate parameters are rejected. Captured
actor headers are required. Raw windows, readable metadata/events and PNG body
evidence reuse the verified private-route renderers with the shared reader and
their existing bounds, safe fallback states, capacity and provider backoff.
Current authority is checked again after rendering before publishing any response.
Image bodies contain only verified PNG pixels, with `X-Comparison-Image` binding
the actor, frozen source, side and display hash. Responses are `no-store`.
OpenAPI and the generated client describe all four shared operations.

Referenced after-images, including unchanged assets absent from the changed-file
inventory, use
`/api/v1/proposals/{proposal}/review/snapshots/{snapshot}/media/{asset}/image`.
The filename must be a permitted content-addressed image and match an immutable
receipt in that exact shared snapshot. There are no query parameters or arbitrary
ref/path overrides. The current actor, membership and frozen repository binding
are checked around the existing bounded image pipeline and again before response
publication. Source bytes/OID, validation version, derivative hash and dimensions
must match the sealed receipt. Historical head/target changes and closure do not
reselect source. The reader shares existing image capacity and returns PNG pixels
only, with `X-Review-Media` carrying base64url `ReviewMediaEvidence`: receipt path,
validation version and nested actor/snapshot/source/display identities. Its media
file identity is SHA-256 of `specspace-review-media-v1`, a NUL byte and the full
asset path; it is distinct from a changed-file identity. Internal one-sided image
source descriptors do not add fabricated changes to the snapshot inventory.
Shared inline after-images use this route as described below.

Referenced before-images use
`/api/v1/proposals/{proposal}/review/snapshots/{snapshot}/files/{file}/before-media/{asset}/image`.
The captured actor is required and query parameters cannot override refs, paths or
sides. The file must be a document in that exact shared snapshot. The reader first
verifies its complete frozen before-source, then uses the bounded canonical
readable parser to prove an image event in an unprotected unit names the requested
asset. Code examples, HTML, ordinary links and after-only references grant no
authority. The canonical source supplies the path used for provider IO.

The frozen merge-base tree must contain exactly one regular image entry at that
path, with supported blob identity and bounded size. Tree reads are capped at
20,000 entries and share the existing two reader slots with source parsing and
image verification. OID, length, content-addressed SHA-256, actual image type and
safe re-encoded pixels are checked before display. Current authority is rechecked
after reference parsing, connection, tree IO/selection, binary IO and image CPU,
and once more before the HTTP response. CPU work retains its permit through
timeout/cancellation. No ref is reselected and no before-image receipt is invented.

The PNG body carries `X-Review-Before-Media`, whose `ReviewBeforeMediaEvidence`
binds the original before-document path/OID/hash, changed-file identity, snapshot,
merge base, asset source and display proof. Its validation version describes the
current image engine; it is distinct from a sealed after receipt. Missing before
side/reference/asset, unavailable canonical proof and tree limits return explicit
**422** local-image fallback codes. Access denials retain their scoped **401/404**
behavior. The shared readable frontend uses the document-bound endpoint for
inline before-images as described below.

## Submission and shared snapshot frontend

`/changes/:id/review/snapshots` is linked from proposal details and reviewer
configuration. Opening it performs only shared database reads. History now carries
the current proposal number/title/branch/target, so even empty or historical views
retain branch identity without Git IO. The page displays frozen head/target/base,
description, UI-impact decision, content findings, complete paged file descriptors
and verified image metadata. It links to commit-pinned book views and shared raw,
readable and image-body comparisons. Snapshot availability
is explicitly separate from PR/checks, approval and publication through merge.

Editors explicitly check the saved version before submitting. Preflight combines
captured-actor configuration and provider-observed proposal detail and rejects
mixed versions, moved/unavailable refs, revoked/ineligible reviewers, missing
independent product/development reviewers and required design eligibility. The
detail endpoint honors an optional captured-actor header before provider work;
legacy detail reads without it remain supported. The subsequent explicit send
freezes head/target, all three versions, actor and idempotency key. Further edits
or route/query changes cannot replace that intent while its outcome is uncertain.
Worker-side source/media/authority checks remain authoritative.

Polling observes the accepted private job only. Timeouts and lost replies preserve
the original mutation for explicit exact retry; timers never POST. A later error
does not erase earlier uncertainty. Definite admission failure permits a new
preflight; provider reconciliation retains its pending reservation. Publication
responses must match captured proposal/operation/job, identity digests and applied
version before the page reports snapshot completion. A job restored from `job=`
uses authenticated observation only and has no reconstructed body or retry key.
Its result is validated against the requested job/proposal before shared reading.

The URL preserves proposal, selected `snapshot`, file/media offsets, history cursor,
selected `file`, raw/readable `mode`, paired readable `unit` and accepted private job ID.
File selection requires an explicit snapshot and a file from the displayed manifest
page; changing file pages or snapshots clears file/unit selection. Unknown/duplicate
parameters and out-of-range offsets
are rejected. Shared history preserves microsecond UTC ordering. Header, history,
file/media counts, identities, content findings and receipt hashes/dimensions must
agree before a complete page is installed. There is no partial mixed-snapshot
display. Pending requests use timeouts and generation checks and are cancelled on
scope change or destruction. Same-actor session recovery resumes safe job reads;
unknown POST results require explicit retry. Account switches and logout purge
private intent and displayed evidence. A denied private observation also clears
displayed evidence; explicit reload drops the denied job URL and reads shared
history afresh. Mutation event handlers check the current profile synchronously,
including queued clicks before reactive cleanup. Route exit, logout and unload
guard pending work. Private intent is kept in page memory only.

Shared body components provide a page-local transport for the existing bounded
raw/readable/image controllers. Private comparison pages retain their original
transport and authority. Shared responses must match actor, proposal, snapshot,
content identity, pinned head/base and full file evidence before display. Raw
source remains escaped text with explicit before/after and addition/removal labels;
readable units retain safe AST events, raw fallback and bounded pagination. Image
proof and body hashes are checked before an object URL is created; replacement,
identity loss and destruction revoke it. Input changes hide prior-file raw output
immediately, before the new read completes. All source stays in page memory.

A body access denial clears the displayed snapshot and requires an explicit reload;
source backoff retains safe retry for the same immutable selection. Source requests
are cancelled on selection, route, identity or component changes. Shared readable
after-images, including unchanged referenced assets, load through the sealed
media endpoint. The before side receives a separate context derived from the
validated before-document path/OID/hash and changed-file identity. It uses the
document-bound before-media endpoint and verifies those additional proof fields;
a before response cannot substitute for an after receipt or vice versa. Its
supported current engine version is independent of the historical after-receipt
version. Missing before source/reference returns a local 422 fallback without
purging verified sibling content. Changed image files remain available in the
before/after image comparison.

Inline image requests begin near the viewport (300px margin), once per selection,
or from an explicit labelled load button. Browsers without viewport observation
retain that button. Transient errors require explicit retry; no timer retries
provider reads. The captured actor, snapshot, content identity, head/base, path,
validation version, bounded source/display metadata and after side must match.
The client checks the after-media domain identity or the exact before-document
identity, then the PNG body hash before creating a URL. It rechecks live identity
after each hash. Decoded dimensions must
also match before pixels become visible. Account/selection changes and destruction
cancel requests and revoke URLs; media access denial clears sibling source views
through the page's existing access-denial boundary. Private book/editor image
routing remains unchanged.

Native labelled controls, RTL/LTR isolation, wrapping layouts and 44px button
heights support keyboard and narrow-screen use. Component/router tests verify
guards, labels, escaped server strings and account isolation. Physical keyboard,
visual RTL and 320px acceptance remain open: browser discovery returned no available
browser, so no visual acceptance is claimed by the automated DOM tests.
