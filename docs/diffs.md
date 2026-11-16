# Commit-pinned proposal comparisons

The raw comparison engine and provider merge-base read are implemented. The
proposal diff API, UI, semantic alignment and review anchors remain required work.

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

## Remaining workflow

The application must capture authorized proposal head and current target,
resolve their merge base, enumerate complete trees, resolve stable document IDs
and catalog ordering, and recheck authorization before publishing results.
Creation/deletion/renames, catalog moves and image changes need explicit evidence.
Raw and AST-readable file views must page large content without hiding changes,
preserve complete pinned identities, and bind hunks to file/section/content hashes
and snapshot. The UI needs RTL before/after, narrow-screen layout, reliable word
highlights, unchanged-context controls, protected source and no-change states.
Discussions, submission, approval and merge stay bound to persisted review
snapshots rather than unsaved editor buffers or line numbers alone.
