# Formal review preparation

Review configuration is editable at `/changes/:id/review`; its contract is in
[`review-configuration.md`](review-configuration.md). Server-side preparation,
the submission journal, durable worker and atomic shared snapshot persistence
are implemented.
The public submit/status/snapshot API, decisions, discussions,
and provider PR/check orchestration remain open.

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
store methods are not yet exposed through public HTTP endpoints; shared raw/
readable/image body reads and snapshot history navigation remain open.

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
currently restart rather than resume. Public submission/status/snapshot endpoints
and the corresponding frontend flow remain separate remaining work.
