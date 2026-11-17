# Commit-pinned proposal comparisons

The raw engine, complete inventory preparation and authorized comparison loader
and durable metadata jobs/pages are implemented, with authenticated HTTP progress
and paged inventories, plus authenticated bounded raw/readable file windows.
Browser UI, media views and review anchors remain required work.

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
is wired into API application state and invoked by the durable comparison worker.

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
backend store/worker checks; HTTP coverage is described below and browser acceptance
remains open.

## Implemented: authenticated HTTP contract

`GET /api/v1/proposals/:id/diff?request_id=<uuid>` reserves a derived comparison;
optional `head` and `target` select exact commit IDs. Capture the request UUID,
selection and `X-Expected-Actor` before sending, and reuse them on an uncertain
result. An exact replay returns the original snapshot, including after a later
proposal head. This GET requires a captured actor header to prevent cross-site
requests from enqueuing work. The authenticated session and current access remain
authoritative; the header grants no access.

Queued/loading responses are 202 with a canonical `Location` containing
`?snapshot=<uuid>` and `Retry-After: 2`. Poll that snapshot URL. READY returns 200
with frozen summary and a metadata page; FAILED returns 200 with explicit terminal
state/error and no summary or page. A successful status read alone does not mean
a successful comparison. Offset defaults to zero and limit to 50 (1–100 allowed).
Subsequent pages use only snapshot/offset/limit, never a new request or ref
selection. Pages retain snapshot/content identity and make no provider reads.

Responses include current proposal head/version/state separately from the frozen
summary, plus explicit historical-head/context-changed flags and UTC completion
time. Closed proposals can retain readable historical comparisons. Access and
binding are rechecked after page retrieval; private requests are not shared with
another member, including an administrator. Membership revocation blocks reads;
removing the user's last membership invalidates the session. All responses use
`Cache-Control: no-store`. Invalid or ambiguous query combinations return 400;
changed selection under the same key returns 409. Admission returns 429 with
`Retry-After: 60`, while exact replay remains exempt.

`GET /api/v1/jobs/:job` requires the same captured actor for comparison jobs and
returns only a typed snapshot/content pointer and canonical progress path. It
never exposes source bodies, raw job payloads or provider connection settings.
OpenAPI and the Angular `ComparisonApi` expose these contracts without automatic
retries. Six PostgreSQL HTTP scenarios cover queued-to-ready pages, private jobs,
malformed requests, shared-cookie identity changes, revocation/reconfiguration,
historical results, replay/quotas and explicit setup failure. Two client scenarios
cover captured request identity across deferred subscription/replay and paging
without reselecting refs. These checks do not complete file-view or review acceptance.

## Implemented: source-verified individual text-file loading

`ComparisonFileReader` resolves a file only through a READY, successful private
comparison and its immutable file row. The server owns the before/after paths,
merge-base/head commits, connection settings, sizes, blob IDs and SHA-256 hashes;
clients supply only snapshot and file identities. It reads at most the selected
pair, without ref selection, ancestry requests, full-tree/book reloads, main
cache writes or Git mutations. Historical/closed results remain pinned to their
published source even when current proposal metadata has advanced.

Documents and catalogs up to 2MiB per side are supported. Missing sides and
present empty files stay distinct. Malformed YAML/Markdown, line endings, Unicode
and HTML remain exact raw source. Actual byte length, Git-framed blob identity
and SHA-256 must all match before an aligned result can be returned. Binary
assets and other file kinds are rejected explicitly by this text capability;
image views still require their own verified media path.

Current private scope and frozen provider binding are rechecked after connecting,
after each source fetch and after CPU verification/alignment. An explicit recheck
is available after later rendering awaits. Source/context types are not response
DTOs and have no serialization/debug derivation. Two loads run at a time with no
unbounded queue; the runtime uses a 60-second deadline (30–120 allowed). Bounded
blocking CPU owns the capacity permit through request cancellation. Provider
backoff is preserved without automatic retries. No source bodies are persisted.

Eight application scenarios cover exact historical renames, empty/add/delete,
forged provider/stored evidence, invalid/oversized/non-text inputs, access changes
at async boundaries, backoff, capacity/cancellation and timeout recovery. Two
PostgreSQL scenarios prove READY/private scope, exact pinned bodies, later closure
and configuration changes, plus access revocation during a provider fetch. The
runtime source configuration wires this capability alongside the inventory
worker. The raw HTTP/window and bounded payload cache contracts are described
below; readable HTTP serving is described later. This loader alone completes no diff UI or
review acceptance requirement.

## Implemented: bounded raw-file HTTP windows

`GET /api/v1/proposals/:id/diff/files/:file?snapshot=<uuid>` returns verified
raw Markdown/catalog source for one file from the published inventory. The file
identity comes from that inventory, rather than a browser path. Current private
scope and `X-Expected-Actor` are required. Rendering runs on bounded blocking CPU;
a further authorization/evidence check occurs after window construction.

The response retains proposal/snapshot/content/file identities, exact merge-base
and head, the complete file change descriptor, both source digests and original
byte/line totals. Source bodies are represented by raw segments with equal,
delete or insert kind and half-open byte coordinates. HTML remains JSON text;
consumers must render it as source, never executable markup. No source
normalization or Markdown reserialization occurs.

`chunk` and `offset` identify a UTF-8 byte position in one alignment chunk. Use
`raw.next` for the next request against the same snapshot/file. Initial values
are zero. Every window defaults to 32KiB of source text across both sides;
`max_bytes` accepts 1–64KiB. Equal text counts twice. At most 128 segments are
materialized. JSON escaping/metadata add bounded overhead beyond the text budget.
Windows can end inside a long line and resume at the exact next UTF-8 scalar;
line and byte-column coordinates make continuation explicit. A grapheme cluster
can span windows and byte columns are not display/UTF-16 columns. CRLF, combining
marks, emoji and missing final newlines remain reconstructable without loss.

Empty files and absent sides remain distinct in the file descriptor. An empty or
terminal window has no next cursor. Large/coarse replacements retain every byte
and keep `detailed=false` explicit. Invalid/duplicate/unknown query parameters or
nonexistent chunk/byte positions return 400; pending comparison rows return 409.
Assets/other non-text kinds return 422 without attempting a text/media fetch.
Provider backoff returns 503 and preserves `Retry-After` when supplied; missing
or forged source and revoked access never expose a raw response. Reads do not
retry automatically and all responses are `no-store`.

Six engine scenarios verify exact multi-window reconstruction, 2MiB lines,
coarse fallback, Unicode/CRLF/YAML/HTML, segment budgets, malformed evidence/cursors
and line continuation. A boundary regression also ensures a final common suffix
cannot overflow the alignment chunk cap. Four PostgreSQL HTTP scenarios cover
pinned multi-window reconstruction after later head/closure, private scope and
query bounds, explicit pending/media states, forged/missing source, revocation
and provider backoff. The Angular typed client captures window coordinates and
actor/snapshot identity before subscription; a client scenario covers exact
manual replay after another actor polls. These checks do not establish browser
rendering, readable AST views or review anchors.

## Implemented: bounded private file-payload cache

Each file reader keeps verified immutable source pairs and their raw alignment
in memory. The cache admits at most 32 entries under a 32MiB accounted-allocation
budget, including source capacities, alignment storage and evidence metadata.
Oversized entries are still read/verified for the requesting user but are not
retained. LRU eviction applies to both byte and entry pressure. Entries expire
five minutes after insertion; access does not extend that lifetime. Expired
entries are ineligible immediately and are reclaimed lazily on cache operations.
This accounting bounds retained cache allocations, not total process RSS or
in-flight readers. Restart loses the cache and fetches the same pinned pair again.

Cache evidence binds actor, organization, proposal, branch, frozen repository
connection, snapshot/content identity, exact base/head and the complete file
change descriptor. It grants no access. Every warm request resolves current
private READY scope before lookup and rechecks it afterward. API rendering also
retains its final recheck. Current proposal metadata is attached freshly, so
later heads or closed/merged state can reuse historical payloads without
retargeting their base/head. Removed access or a changed repository connection
blocks warm reads. Provider unavailability does not invalidate already verified
immutable bytes within their cache lifetime.

Only fully verified results that pass final authorization are admitted. Failed,
uncertain or revoked reads cannot populate the cache. Payloads are shared through
immutable references rather than cloning source for each window; an in-flight
reader can finish after eviction, subject to its own authorization recheck.
Cold work remains limited to two loads with no unbounded queue. Its permit now
also covers the final authorization and cache admission, and remains owned by
the CPU task through cancellation. Concurrent misses may repeat bounded read
work; no unbounded per-key waiter queue is introduced. No source is written to
disk, PostgreSQL, jobs, audit or the approved-book cache.

Nine additional application scenarios cover private evidence isolation, fresh
historical context, warm access races, absolute expiry, entry/byte eviction,
oversized bypass and failure admission. The HTTP reconstruction scenario now
proves many windows and historical replay fetch the pair only once. A warm HTTP
scenario checks provider-independent reuse, actor ownership, repository changes,
later closure and revoked membership. A PostgreSQL scenario also proves a new
reader refetches identical frozen bodies after restart. Operational retention
and cleanup beyond this bounded ephemeral cache remain part of hardening.

## Implemented: lossless bounded readable projection

`diff::readable::prepare` derives a document's readable units from the existing
lossless top-level editor boundaries and inert GFM renderer. Contiguous source
ranges cover every byte, including exact YAML metadata, stable-marker prefixes,
reference definitions, unsupported HTML/comments and trailing whitespace. Each
unit retains raw SHA-256, source/body ranges, kind, protected reason and nested
section IDs. A heading receives a stable identity only when its marker is unique
within that document. No parser failure becomes an empty or unchanged result.

Renderable units also carry a coordinate-independent fingerprint of their inert
render events and code-block information. Formatting, link targets/titles,
list/table/task metadata, exact visible text and code-language changes remain
part of that identity. Equivalent Markdown spellings can share a render identity
while their raw hashes still differ. Protected metadata/source has no claimed
render equivalence. This digest describes the derived view; it is not proof of
business meaning, a review anchor or approval.

Preparation limits source to 2MiB, parser events to 32,000, nesting to 64 and
expanded event strings to 8MiB before constructing full models. Heading lookups
use sorted source ranges and indexed occurrence counts. The existing reference
expansion probe is shared with the readable path, so the parser's exhaustion
fallback to literal reference text cannot silently change the readable result.
Definition rescanning remains bounded and reports its existing explicit limit.
The renderer emits known inert events, never executable source HTML.

Seven engine scenarios cover full source reconstruction/coordinates, GFM lists,
quotes/tables/code, Hebrew/emoji, protected definitions/HTML, semantic versus raw
identity, duplicate/missing markers, empty and unterminated/CRLF source, nested
code-language changes and adversarial limits. Projection types deliberately are
not serializable whole-response DTOs. This is pure processing without scope or
provider I/O. Pairing, bounded windows and authenticated serving are described
below. Browser UI and review acceptance remain required.

## Implemented: conservative before/after readable pairing

`ReadablePair::prepare` projects both optional file sides and retains every unit
exactly once in its derived rows, including unchanged/protected source. Absent
files and present files whose projection fails remain distinct. An unavailable
side retains its source hash/byte count and error; `ready=false` explicitly
requires raw fallback. Empty rows from a failed projection cannot mean no change.
Identical complete sources pair directly, including repeated text and duplicate
markers, and never manufacture changes from identity ambiguity.

Matching first uses unique stable heading IDs, then unique exact source and
unique render fingerprints. Metadata and trailing source have structural slots.
Remaining unique units can pair by ordinal within the same known heading scope
when both remaining counts agree; this inference is explicitly labelled
`section_position`. Unidentified headings disable positional inference. Changed
trusted heading IDs are not silently replaced by same-label matches. Ambiguous
duplicate units in changed sources remain unpaired additions/deletions. These
are conservative derived alignments, not proof of an author's intended move.

Rows distinguish raw change, render change (or unknown), and absolute ordinal
change. Render-equivalent Markdown changes retain raw differences. Reordering
can remain visible with unchanged unit content; insertion shifts are also
absolute position changes. Row identities bind a version tag, both complete
source hashes, unit indexes, ranges and raw hashes. Serving/review must still
bind file/snapshot, section context and current scope independently. No row is
an approval or a complete review anchor.

Hash/position grouping uses indexed linear passes over the bounded projections;
there is no block-pair alignment matrix. Output order and identifiers are
deterministic. Eight scenarios cover moved/edited stable sections, raw/render
and metadata differences, ambiguous/missing/changed identities, insert-plus-edit
cardinality, add/delete/parse failure, identical duplicate/protected source and
whole-pair identity. Every short binary block sequence through length three is
paired against every other sequence, checking complete one-to-one coverage,
determinism, unique rows and visible reordering. Pair objects remain private
processing models rather than serializable whole-file responses. Authenticated
bounded windows and authenticated serving are described below; UI remains required.

## Implemented: bounded readable row and event windows

Readable pairs expose metadata pages of 1–100 rows, with complete side state,
source hashes, unit totals and explicit raw-required errors. Rows include bounded
unit summaries and their before/after matching evidence. Each row is capped at
4KiB encoded JSON and a complete page at 512KiB. Large nested section/code lists
are represented by complete counts, rather than copying them into every row.
These are pure DTO builders; authenticated serving is described below.

A readable unit can materialize a stream of inert event fragments, capped at
128 fragments and 1–64KiB decoded text per window. The complete encoded JSON
response is capped at 384KiB, including escaping and container metadata. A
counting serializer enforces encoded bounds without first allocating a giant
JSON string. Text fragments end on UTF-8 scalar boundaries and retain exact
decoded-event byte coordinates plus adjusted UTF-16 text offsets. Source ranges
remain the original parser ranges, so entities/escapes are never presented as
one-to-one source byte mappings. Raw source/hash identity remains separate.

Each window carries its initial and final stack of known Open events, outermost
first. Consumers can reopen initial containers, render only known inert events
and close the final stack in reverse for a standalone virtual page. Concatenating
fragments reconstructs the canonical event stream without reopening duplicates.
Nested lists, tables, quotes and long code lines remain accessible. Ordered lists
include current/next item numbers for reopened containers, preserving numbering
inside items and between sibling items, including nested ordered lists. Apply
`start_ordered_lists` by stack depth when reopening; stack events retain their
original AST attributes. `end_ordered_lists` reports the resulting context. Code-language
metadata accompanies code opens and resumed code context. Stack and code metadata
have bounded budgets in addition to the overall JSON limit.

Protected metadata/source explicitly requires raw view. Indivisible oversized
attributes (such as a large link title/destination or image alt) also require raw
fallback; they are never silently shortened or split into duplicate elements.
Unavailable/absent projections and invalid coordinates retain explicit errors.
No window grants access or review approval. Future HTTP responses must bind the
file/snapshot and recheck current scope after materialization.

Seven scenarios reconstruct GFM, decoded entities, Unicode and container state;
check giant code lines/control-character escaping, complete row paging and raw
fallback; validate code-language resumption, terminal/malformed cursors, metadata
bounds and damaged evidence. The existing projection/pair tests continue to
cover source preservation and matching semantics. Application integration,
authenticated routes and typed client are described below; browser rendering
remains required.

## Implemented: authenticated readable HTTP contract

`GET /api/v1/proposals/:id/diff/files/:file/readable?snapshot=<uuid>` serves
bounded readable metadata pages (`offset=0`, `limit=50` by default; 1–100 rows).
The snapshot/file identities come from the durable inventory. The response binds
proposal, snapshot/content identity, complete file descriptor, exact merge-base
and head, source hashes and unit summaries. `page.ready=false` with a side state
`RAW_REQUIRED` explicitly selects raw fallback when projection cannot complete;
empty rows in that state never mean no change or approval.

`GET /api/v1/proposals/:id/diff/files/:file/readable/units/:unit` requires the
same snapshot and a `side=before|after`. Optional `event`, decoded-text `offset`
and `max_bytes` use the event-window protocol above; initial coordinates are zero
and text budget defaults to 32KiB. The response is explicitly `READY` with a
window, `ABSENT` with no unit/window, or `RAW_REQUIRED` with an error code and no
window. Protected metadata/source and oversized indivisible attributes return
raw-required state with their bounded unit summary; unavailable projections
retain their precise parser/limit code. Invalid coordinates/combinations return
400. Current access remains required for fallback/absence responses too.

Both routes authenticate and require the captured `X-Expected-Actor`. A private
READY file row resolves exact paths/refs and frozen connection settings; the
file reader verifies length, Git blob and SHA-256 evidence. Readable-side source
hashes must match that verified pair. Source caching avoids repeated Git reads,
including later historical closure. Current scope is checked again after AST
processing and response construction, before any body is exposed. Other actors,
revoked membership and changed repository connections cannot use warmed data.
Pending comparisons return 409 and opaque assets return 422 without media reads.

AST projection/pairing and window construction run on the shared two-slot
blocking-CPU pool. There is no unbounded waiter queue. A 30-second processing
deadline returns explicit 503 timeout; cancelled/timed-out tasks retain their
permit until CPU exits. Busy/source failures preserve explicit errors and
provider backoff. Each request currently recomputes its bounded AST pair from
cached verified text; an AST payload cache/performance acceptance remains work.
No raw source, AST or response is persisted or seeded into main indexes/caches.
Responses are `no-store`; source HTML remains inert data and raw fallback stays
available through the existing exact raw-file route.

OpenAPI and the Angular typed client include both routes. Client methods capture
actor, snapshot, side and cursor before subscription and never retry/retarget
implicitly. Seven additional PostgreSQL HTTP scenarios cover exact code-window
reconstruction and metadata paging after closure, protected/limit/attribute
fallback, creation/deletion, query/privacy/binding checks, final post-CPU
revocation, CPU saturation and provider failures. Existing pending/media tests
also cover the readable routes. Two client scenarios cover metadata paging and
frozen manual replay after another actor polls. Browser diff UI, media views,
formal review anchors and review/merge acceptance remain required.

## Remaining workflow

Readable AST comparisons still need browser/controller integration and performance
acceptance; media views still need verified pinned materialization. Both require access
rechecks after rendering. Retention/operational
pruning and cold-load performance still need hardening. Raw windows reuse their
private bounded verified-payload cache; each window still validates and
materializes its source coordinates on bounded CPU.
Creation/deletion/renames, catalog moves and image changes need explicit evidence.
Raw and AST-readable file views must page large content without hiding changes,
preserve complete pinned identities, and bind hunks to file/section/content hashes
and snapshot. The UI needs RTL before/after, narrow-screen layout, reliable word
highlights, unchanged-context controls, protected source and no-change states.
Discussions, submission, approval and merge stay bound to persisted review
snapshots rather than unsaved editor buffers or line numbers alone.
