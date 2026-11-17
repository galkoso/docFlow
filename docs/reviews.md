# Formal review preparation

Review configuration is editable at `/changes/:id/review`; its contract is in
[`review-configuration.md`](review-configuration.md). Formal submission, shared
snapshot publication, decisions, discussions, and provider PR/check orchestration
are still being implemented.

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
CPU closures through cancellation. Provider errors preserve their backoff and
are not retried automatically. Invalid media identifies the failing asset path
with a bounded error code; authority changes take precedence over diagnostics.
A failure never returns partial evidence. Large scans currently restart rather
than resume; checkpointed media work remains part of durable-worker development.

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
This journal does not yet publish snapshots or complete submissions. The shared
snapshot store, publication transaction, worker loop, public submit/status API,
and PR/check orchestration remain open. Fresh provider observations cannot lock
Git against later external changes, so approval and merge gates must also recheck
their own exact refs.
