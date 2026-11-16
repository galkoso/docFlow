# Commit-pinned proposal comparisons

The raw engine, complete inventory preparation and authorized comparison loader
and durable metadata jobs/pages are implemented. The HTTP diff routes, UI,
file-level views and review anchors remain required work.

## Provider graph boundary

`GitProvider::merge_base(left, right)` takes exact immutable commit IDs and
returns the provider-selected common ancestor. Equal IDs require no network
request. GitHub uses the compare endpoint's graph metadata, with page 2 and one
commit per page so first-page patches and their file-count limit are irrelevant.
Returned base/common IDs must parse, base must match the requested left input,
and ahead/behind/diverged relationships must agree with the common ancestor.
Malformed or contradictory responses fail explicitly. The request is bounded,
read-only and has no automatic retry; provider backoff is preserved.

`is_ancestor` uses that same checked relationship. Other deterministic test
providers explicitly reject the new capability until their diff scenarios are
implemented. The returned merge base is a comparison boundary, not permission to
merge, proof of current branch tips, or a claim that there is only one best
common ancestor. Review and merge workflows must bind head, target, policy and
current authorization independently.

GitHub documents that changed files appear only on the first comparison page
and are capped at 300; SpecSpace will enumerate complete pinned trees for its
own diff instead of using that list. See [GitHub's compare endpoint](https://docs.github.com/en/rest/commits/commits#compare-two-commits).

## Exact raw line comparison

`document_engine::diff::compare` accepts two UTF-8 sources up to 2MiB each. It
returns SHA-256 digests, byte/line totals and equal/delete/insert chunks with
half-open byte ranges and one-based line positions/counts. Chunks reconstruct
both inputs exactly. No YAML, Markdown, Unicode, line-ending or trailing-newline
normalization takes place. Empty files and full-file replacements are explicit;
raw HTML is source data and is never executed by the engine.

Complete lines are interned once as exact strings before alignment, so matrix
work compares integer identities rather than repeatedly comparing large lines.
Common edges are retained. Detailed alignment uses at most two million matrix
cells (8MB of u32 data), at most 100,000 lines per side and 1,024 output chunks.
If an alignment or output budget is exceeded, `detailed=false` identifies a coarse
replacement that still covers every byte of both files. The work limit cannot
turn a partial comparison into an apparent unchanged result. Output holds ranges
rather than copies of the source text. API windows must retain those digests and
snapshot identities when materializing/displaying the ranges.

Four engine tests cover exact reconstruction, Hebrew marks/emoji/CRLF/YAML/HTML,
missing final newlines, empty/complete replacements, work/output/line limits and
oversized input. Every pair of binary line sequences of length zero through five
is checked against an independent brute-force common-subsequence oracle, as well
as reconstruction/position invariants. One additional provider scenario covers
exact graph identity, relationship consistency, malformed responses and backoff.
These tests do not prove proposal scope, semantic diff, browser rendering or
review/merge acceptance.

## Implemented: complete inventory preparation

The pure application comparison planner prepares immutable before/after
snapshots from complete provider trees and all pinned document/catalog source.
It verifies each supplied text's declared size and framed Git blob object ID,
rejects missing/extra/duplicate sources and case-folded paths, and bounds trees
at 20,000 files and source books at 1,000 documents / 20MiB (2MiB per text).
The framed SHA-1/SHA-256 blob identity helper is shared with save verification;
existing save proof behavior and its golden object hashes remain covered.

Inventories include every changed path, including uncatalogued documents,
assets, policy/configuration and other files. Unique actual source document IDs
pair rename-plus-edit across paths; matching paths and conservative unique exact
blobs provide fallbacks. Each pairing records its evidence method. Ambiguous
identical blobs remain additions/deletions. Before/after identities, paths, sizes,
modes, digests and catalog positions stay distinct; content, mode and catalog
position changes have separate flags. Absolute position differences are reported
as position changes, without claiming to know which move the author intended.

Malformed source, invalid/missing catalogs, identity mismatches, missing catalog
files, duplicate IDs and documents without positions remain visible with bounded
diagnostics and complete finding counts. No parse error can silently erase a raw
file change. Metadata-only image inventory is not MIME/pixel verification. Stable
inventory/file IDs bind repository and both immutable commits; they are inputs
for later review anchors, not approvals. Deterministic ordering and mixed-repo
rejection are covered alongside renames/reordering, ambiguous duplicates, raw
repairs, changed identity, file modes, forged text evidence and quota/path checks
in eight application scenarios.

This preparation performs no I/O or authorization. The serving workflow still
must prove the selected merge base/head, capture current target and proposal
context, fetch complete trees and source, bound CPU work and recheck permissions.
No comparison route or UI is claimed by the pure planner. The serving snapshot
must additionally bind organization/proposal, current target, metadata version
and provider configuration; planner IDs alone cannot authorize a resource or
count as review evidence. Large inventory responses still need pagination.

## Implemented: authorized loader and bound connections

`ComparisonReader` resolves current server-owned actor/organization/proposal,
recorded head/base, metadata version/state and repository connection. It rejects
provisioning or invalid context before connecting. `PgComparisonAuthorization`
requires an active user and current organization membership; Readers can read
without an edit assignment. Closed/merged proposals remain readable. The context
and provider settings are private server types, never browser-deserialized or
serialized as a response.

`PgGitHubSource::provider_bound` matches frozen numeric repository/installation
settings, then connects using those exact values. It does not perform a second
unbound lookup that could choose another connection during reconfiguration. The
normal book reader and comparison reader share configured GitHub connection
caching; an unbound test book provider is never implicitly a comparison provider.
Current authorization/configuration/version is rechecked after connection,
between snapshot loads, every eight text reads, after CPU work and before return.
The caller can recheck a loaded result again after later rendering awaits.

The loader captures the current main target and proves any selected historical
head against the recorded proposal head, and any historical target against main.
It resolves their merge base and loads the complete base/head trees and required
text, checking declared limits before text I/O and actual sizes before retaining
source. Preparation and blob/identity validation run on bounded blocking CPU.
Main-only changes never enter the before side. Identical base/head commits share
one prepared snapshot. This loader never writes Git or seeds main indexes/caches.

After loading, main and branch observations are refreshed. Ordinary ref movement
is reported separately while the comparison's head/target/base stay pinned; a
force-push removing the selected target's main lineage rejects publication.
Missing proposal branches remain explicit alongside recorded historical content.
A comparison-view ID additionally binds organization/proposal, frozen connection,
metadata version/state/branch and exact selected refs. It is not a review approval.

One load runs per reader; extra loads receive Busy instead of queuing without a
bound. A 30–900-second timeout is enforced (runtime configuration uses 900).
Cancellation releases I/O capacity; CPU closures own the permit until their work
finishes even if the caller disconnects. No read is automatically retried.
Eight application scenarios cover merge-base correctness, main-only changes,
lineage checks, empty-history sharing, revocation/reconfiguration/version races,
ref movement, forged source/size limits, scope/readiness and cancellation/timeout.
Three PostgreSQL scenarios cover real tenant/membership/account checks, Reader
access, closed/provisioning context and bound settings/setup failures. The loader
is wired into API application state but has no HTTP endpoint yet.

## Implemented: durable progress and metadata pages

Migration 008 adds actor-private comparison requests and ordered derived file
rows. Reservation uses an immutable request fingerprint and exact replay key;
identical concurrent requests return one ticket. A new request captures current
server context and freezes its resolved head. An explicit target remains fixed;
an omitted target means main observed by the successful load and is not
advertised as a selected SHA before READY. Context changes before publication
fail the job instead of retargeting its intent. Exact replay still returns the
original ticket after later metadata changes, subject to current access/binding.

One read-only worker claims expiring leases, rechecks context, honors provider
backoff and publishes all metadata/file rows in one transaction. Restart or an
expired lease can repeat reads safely. Stale owners cannot publish, fail or renew
work. Loading heartbeats recheck access. Publication has a separately refreshed
lease and a bounded transaction, avoiding a heartbeat blocking on its own row
locks. Timeout/shutdown drops unfinished work; later claims recover by reading.
Malformed/exhausted jobs terminate explicitly and are audited. No Git mutation,
Git-operation journal reservation or approved-book index write occurs.

Parent triggers preserve the original request and terminal descriptor. Child
rows can be inserted/deleted only while publication is pending; READY requires
complete contiguous ordinals, exact counts and matching scope/head/version.
Terminal file rows cannot grow, change or disappear. Metadata is capped at 4MiB,
each file record at 8KiB and the complete inventory at 40,000 changes. Neither
Markdown nor binaries are stored. Inventory identity lookup reads validated front
matter only, avoiding unnecessary Markdown body expansion.

Status distinguishes queued, loading, publishing, retry wait, ready and failed.
Current actor/membership/provider binding is checked on every status/page read;
removing a Reader's membership is not blocked by the request's audit/history.
A published historical comparison remains readable after a new head/closure,
with current head/version returned separately from its frozen descriptor.
One-to-100-file pages retain the snapshot/content identity and do no Git reads,
even after a process restart. Per-actor admission serializes quota checks: two
pending jobs and ten new requests per minute; exact replay is exempt.

The API runtime starts one worker. `COMPARISON_WORKER_TIMEOUT_SECONDS` defaults to
900 and accepts 30–900 seconds; apply migration 008 before starting it. Twelve
PostgreSQL scenarios cover a 620-file multi-batch publication, empty comparisons,
restart/paging, duplicate requests and quotas, stale owners, backoff, mid-batch
rollback/reclaim, malformed/exhausted jobs, revocation and frozen scope, source
privacy, incomplete database publication and immutable evidence. These are
backend store/worker checks; no HTTP or browser acceptance is claimed yet.

## Remaining workflow

The HTTP workflow must expose reservation/progress and durable paged metadata,
using the stored snapshot identity for every subsequent page. File-level raw
and readable views still need pinned body reads/verification, bounded windows
and rechecks after rendering work. They must use stored paths/ref identities
rather than reloading the whole book for each page. Runtime body caching,
retention/operational pruning and cold-load performance still need hardening.
Creation/deletion/renames, catalog moves and image changes need explicit evidence.
Raw and AST-readable file views must page large content without hiding changes,
preserve complete pinned identities, and bind hunks to file/section/content hashes
and snapshot. The UI needs RTL before/after, narrow-screen layout, reliable word
highlights, unchanged-context controls, protected source and no-change states.
Discussions, submission, approval and merge stay bound to persisted review
snapshots rather than unsaved editor buffers or line numbers alone.
