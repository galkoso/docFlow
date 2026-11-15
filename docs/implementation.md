# Implementation and acceptance ledger

Source of scope: `specification.he.md`, sections 1–26, including AC-01–20 and
all edge cases in section 22. The supplied requirements remain authoritative.

## Stage 0 — foundations and feasibility

- [x] Preserve the original specification without changes.
- [x] Angular strict standalone application and Rust workspace; locked dependencies.
- [ ] Local PostgreSQL and private S3-compatible storage; API/worker process roles.
- [ ] Editor feasibility: Hebrew, tables, images, stable IDs, protected unsupported blocks.
- [ ] GitHub App feasibility: atomic commits, branches, PRs, required-check enforcement.
- [ ] 20MiB book search including content outside the rendered DOM.

Development uses synthetic fixtures and the specification's default decisions.
Production identity, GitHub installation and hosting must be configured before
real data is connected (DEC-01–15); this does not block local implementation.

## Stage 1 — knowledge source and branches

- [ ] OIDC sessions, server resource authorization, organizations and roles.
- [ ] Git provider contract and GitHub App implementation.
- [ ] PostgreSQL schema, migrations, operation journal, audit and outbox.
- [ ] Commit-pinned catalog, continuous book and historical views (AC-01).
- [ ] Full-snapshot literal search, normalization and UTF-16 source maps (AC-02).
- [ ] Proposal list/create/workspace, branch validation and idempotency (AC-03).

## Stage 2 — editing and review

- [ ] Rich editor, source preservation, buffers and dirty-navigation handling (AC-04,13).
- [ ] Atomic commits with expected head/version and no-op detection.
- [ ] Raw/readable diff, snapshot discussions and outdated anchors (AC-14).
- [ ] Independent qualified reviewers, section reviews, approval invalidation (AC-05,10,19).
- [ ] PR/check integration, base updates, three-way conflicts and serialized merge (AC-11,12).
- [ ] Close/reopen, inverse restoration proposals, provider synchronization (AC-15,16).
- [ ] Delivery evidence and PLANNED/IMPLEMENTED/RELEASED policy (AC-20).
- [ ] Keyboard, RTL, narrow-screen and state-isolation verification (AC-18).

## Stage 3 — external AI bundles

- [ ] Versioned export instructions, manifests, full/restricted context and retention (AC-07).
- [ ] Private staging, ZIP/schema/path/hash/ID/catalog/link validation (AC-17).
- [ ] Result preview, partial selections, product decisions and atomic apply (AC-08).
- [ ] Export-snapshot three-way merge, stale preview protection and dedupe (AC-09).
- [ ] Manual and external-model paths work within the same proposal (AC-06).
- [ ] Folder upload alternative, repair report, expiry/revocation and download authorization.

## Stage 4 — hardening and pilot

- [ ] Complete section 23 technical tests and section 22 edge-case coverage.
- [ ] Load measurements: 20MiB, 1,000 documents, six concurrent editors.
- [ ] Failure drills: provider races/timeouts, crash recovery, outbox and webhook replay.
- [ ] Backup/restore exercise, retention, telemetry, resource quotas and deployment docs.
- [ ] Security review: XSS, CSRF, resource access, archives, secrets and provider privileges.
- [ ] Human pilot with product, development and design reviewers.

## Evidence

Checkboxes represent verified work only.

- Rust domain foundation: `cargo test --workspace` passes three behavioral tests
  for branch validation, case-insensitive reservation keys and path traversal
  prevention. `cargo fmt` and `cargo clippy --workspace --all-targets -- -D warnings`
  pass. This establishes identifier rules only; Git/database uniqueness enforcement
  and resource authorization remain open.
- Angular foundation: production build and navigation accessibility unit check pass
  with strict TypeScript/templates. Four lazy routes, Hebrew/RTL shell and initial
  setup states are implemented. No connected book, authentication or proposal
  workflows are claimed by this check; browser/narrow-screen verification is pending.
- Axum API foundation: liveness, database-unavailable readiness and structured 404
  tests pass. API binds to loopback by default, uses a bounded SQLx pool, disables
  response caching and shuts down on Ctrl+C. Readiness currently checks PostgreSQL
  only; provider/storage checks remain pending with their respective adapters.
- PostgreSQL core: six database integration tests pass for concurrent job leasing,
  lease fencing, expired-side-effect handling, append-only audit, case-insensitive
  branch reservations (including closed proposals), tenant foreign keys and
  operation idempotency uniqueness. Core migrations include memberships, teams,
  repositories, proposals, buffers, operations, jobs, outbox and webhook dedupe.
  Review/bundle/search persistence, authorization enforcement and the recovery
  reconciler are still pending; uniqueness alone is not an idempotent use case.
- Review/authorization domain: four additional behavioral tests pass for resource
  scope, role/assignment gates, author exclusion, independent product/development
  reviewers, revoked qualification, UI impact and head/target/policy invalidation.
  Content edits demote APPROVED and reject final-state writes. These pure rules
  still need API/persistence integration; merge readiness is not implemented yet.
- Search engine: four tests pass for literal matching, block boundaries,
  Hebrew marks/bidi, NFC, whitespace, grapheme-safe UTF-16 highlights, pagination
  and full-snapshot counts. A release benchmark on this local machine measured
  engine-only p95 **0.453s**, 20 samples, exactly 20,971,520 bytes / 1,000 synthetic
  documents. Reproduce with `cargo run --release -p specspace-document-engine
  --example find_benchmark`. This excludes Markdown extraction, database, HTTP and
  DOM loading and therefore does not complete AC-02 or the end-to-end latency gate.
- Markdown extraction: three additional tests pass for exact source retention,
  stable section IDs across heading renames, code/list/table text, independent
  search boundaries, hidden metadata/destinations and located validation findings.
  Raw HTML is reported rather than rendered. This is source preservation during
  parsing, not yet proof of rich-editor round-trip or full catalog/link validation.
- Catalog foundation: three additional tests pass for ordered composition,
  uncatalogued files, duplicate document/section identities and a six-document
  Hebrew/English fixture searched as one book. Fixture search finds four instances
  across four documents, including pointed Hebrew. This verifies engine composition;
  Git-backed loading, browser rendering and internal-link validation remain open.
- Path canonicalization additionally rejects non-NFC filenames; a decomposed
  Unicode path is covered by the traversal test, preventing ambiguous normalization
  from silently targeting another file. Archive-level duplicate detection is pending.
- GitHub read/write adapter: four local HTTP contract tests pass for verified ref
  creation, one expected-head mutation for multiple files, stale-head refusal,
  unknown-result handling without retries, immutable text reads, truncation rejection
  and rate-limit timing. Provider API contracts were checked against GitHub's
  [commit](https://docs.github.com/en/graphql/reference/commits) and
  [Git](https://docs.github.com/en/graphql/reference/git) references.
  Live installation authentication, PR/check enforcement, tree loading and merge
  capability remain open. Contract tests do not prove the live GitHub feasibility gate.
- GitHub App token issuer: two additional tests pass using an ephemeral RSA key,
  cryptographic JWT verification and a local token endpoint. JWTs use RS256,
  bounded lifetime/clock skew and repository-scoped permission requests. Expired
  installation tokens cannot construct a provider client. No private key fixture
  or raw token is exposed through Debug/Serialize. Secret-provider/API wiring and
  live installation verification remain pending.
- GitHub Actions verified both backend and frontend workflows successful for
  commit `e5aac93` (runs 43619202550 and 43619202546), including PostgreSQL tests.
- Snapshot transport: provider tree enumeration now includes uncatalogued files
  and rejects truncated trees. Truncated GraphQL text falls back to a pinned Git
  blob read with size/encoding/identity checks. Five HTTP transport tests plus the
  two App authentication tests pass. Live provider and book-service wiring remain open.
- Book application service: three tests pass for a complete commit-pinned book,
  catalog ordering, all-document search, malformed-chapter placeholders, orphan
  diagnostics and permission revocation before publication. A seventh PostgreSQL
  test verifies current membership/disabled-user/tenant checks. Parsing runs in a
  bounded blocking pool; declared and actual text-size limits are checked.
  The full Rust suite now passes **37 tests**, including seven PostgreSQL tests,
  and workspace Clippy passes with warnings denied. HTTP/UI/OIDC wiring is pending.
- Server-session storage: two additional PostgreSQL tests pass for hashed bearer
  tokens, per-session CSRF secrets, expiry, logout isolation and immediate account
  disablement. OIDC flow/session tables are migrated in tests. Login callbacks,
  cookie attributes and HTTP CSRF enforcement are still pending.
- Login-flow persistence: a tenth PostgreSQL test verifies browser binding,
  concurrent single-use state consumption and expiry. An incorrect binding cannot
  consume another browser's valid flow. OIDC signature verification and callback
  routing still need integration with these records.
- OIDC identity verification: two tests pass against a local discovery/JWKS/token
  server with an ephemeral RSA key. Authorization Code + S256 PKCE succeeds;
  invalid signature, issuer, audience, nonce, expiry and access-token hash fail.
  Production requires HTTPS, local HTTP is loopback-only, redirects/retries are
  disabled and fresh JWKS are loaded per login operation. This module returns
  verified issuer/subject only; HTTP callbacks and session cookies remain open.
- Authentication HTTP stage: login/callback/profile/logout are wired to the OIDC
  verifier, single-use flow store and hashed sessions. Two unit checks verify
  host-scoped Secure/HttpOnly/SameSite cookies and duplicate-cookie refusal. Two
  PostgreSQL HTTP checks verify session-derived identity (ignoring injected roles),
  Origin+CSRF enforcement and logout isolation. API unit/integration checks and
  workspace Clippy pass. Callback-to-browser validation against a configured real
  identity provider, `/me` capability expansion and frontend sign-in remain open.
- API contract: Rust derives now generate `docs/openapi.json` for all current
  endpoints, profile capabilities and structured errors. Backend CI regenerates
  and compares the artifact. `/me` returns capabilities derived from current
  memberships; reader HTTP tests prove merge/admin capabilities remain false.
  The public auth-status endpoint reports whether login is configured. Future
  product endpoints and their request/response schemas remain to be implemented.
- Angular API types are generated from the Rust OpenAPI artifact. The generator
  requires TypeScript 5 while Angular 22 requires 6, so `tools/api-client` has its
  own pinned dependency tree and lockfile. Frontend CI regenerates the types and
  checks for drift. No peer-dependency override or Angular downgrade is used.
- Frontend session controls: production build and all five Angular tests pass.
  Tests cover unconfigured login, anonymous versus failed requests, failed logout,
  duplicate submission and cancellation of stale identity reads after logout.
  The UI uses generated response types and Angular's cookie/CSRF HTTP support.
  A live local proxy check returns JSON auth status (200), unauthenticated profile
  (401) and database readiness (200), rather than a fallback HTML page. Full browser
  sign-in and visual/accessibility verification remain pending.
- Historical-read foundation: GitHub commit metadata validates the requested SHA
  and normalizes commit dates to UTC. Ancestry checks reject divergent commits;
  eight provider tests now pass. A read-only live compare of two docFlow commits
  confirmed page 2 retains ancestry metadata without file patches, including when
  that page has no commits. This is transport evidence, not a live App permission gate.
- Pinned reader service: five application tests pass. Arbitrary branch commits
  cannot be loaded as main history; verified main snapshots carry Git commit dates.
  A four-snapshot, repository-keyed derived cache rechecks current authorization,
  bounds concurrent loads/search work and does not silently substitute a cached
  version when a current-main read fails. Durable indexing and worker integration
  remain open; cached data is disposable and Git remains authoritative.
- App connection scope: reusable App signing credentials produce installation-
  scoped clients only after GitHub repository metadata matches the configured
  numeric repository ID. Nine provider tests pass, including substituted metadata
  rejection. No token or private key is exposed through the connection interface.
- Server source configuration: secret references accept bounded `env:NAME` or
  absolute `file:/path` inputs, never URLs, and secret bytes are zeroized on drop.
  Repository IDs/installations come from DB records; short-lived connection caching
  is bounded and respects configuration changes/expiry. Three infrastructure unit
  tests and eleven PostgreSQL tests pass. Foreign repository IDs remain hidden
  before configuration lookup. API startup wiring is the next step.
- Reader transport data: search hits now have stable SHA-bound IDs and bounded
  grapheme-safe excerpts; parsed headings retain their levels for the table of
  contents. Eleven document-engine tests pass. The updated 20MiB / 1,000-document
  benchmark measured engine-only p95 **0.529s** over 20 samples on this machine.
  HTTP and browser latency are not included in that result.
- Book HTTP stage: repository discovery, ordered book metadata, per-document reads
  and full-snapshot search are now connected to current session/database access and
  server-configured GitHub App credentials. Three PostgreSQL HTTP scenarios prove
  cursor/commit isolation as main advances, cross-organization denial, account
  revocation against cached data, and explicit setup errors with no fixture fallback.
  API unit/integration checks and workspace Clippy pass. The OpenAPI contract and
  Angular types include these routes. Reader rendering, durable indexes, background
  jobs and live GitHub App validation remain open.
- Reader render contract: three additional document-engine tests prove canonical
  visible-text/UTF-16 parity across formatting, tables, code and nested lists;
  raw HTML becomes located placeholders and unsafe link/image destinations are
  omitted. Rendering has explicit depth/event limits and only canonical hashed
  PNG/JPEG/WebP asset references are accepted. All fourteen engine tests and
  workspace Clippy pass. HTTP delivery, Angular rendering and asset byte serving
  are the next integration steps; no visual acceptance is claimed yet.
- Canonical render delivery: document responses now include typed render events
  and explicit render errors. Rendering/JSON encoding runs in a bounded blocking
  pool, followed by a fresh permission check. Render-limit findings also prevent
  a misleading search-ready state. API/PostgreSQL tests verify event delivery and
  indexed text offsets, while application tests and workspace Clippy pass.
  Angular native rendering and authorized image bytes remain to be connected.
- Browser verification attempt: the Browser runtime reported no available browser
  and discovery returned an empty list. Desktop/320px visual verification remains
  open; the Angular development server starts successfully on loopback port 4200.
- Native Angular render stage: a bounded event tree produces semantic headings,
  lists, tables, code, links and task markers through Angular templates. Text is
  interpolated, raw HTML remains a located notice, and no image URL is fetched.
  Four renderer tests verify native table structure, executable-content rejection,
  cross-format UTF-16 highlights including Hebrew niqqud/emoji, and malformed/deep
  input failures. Production build and all nine frontend tests pass. Reader page
  wiring, catalog links, table alignment and authorized image delivery remain open.
- Connected reader checkpoint: repository discovery now opens the real ordered
  book, shows its commit/update metadata and source findings, and loads chapters
  using that exact SHA. A continuous document surface and section TOC replace the
  placeholder. Stable catalog links preserve repository/commit/document/section;
  source images still show an explicit unavailable placeholder. Private book state
  is component-scoped and cancelled/reset on account/version changes or denied
  chapter access. Five additional HTTP/DOM tests cover pinned requests, duplicate
  loads, stale responses, explicit inaccessible repositories, section-only route
  changes and logout with an outstanding chapter request. Build and fourteen
  frontend tests pass. Automatic viewport loading/eviction, full find controls,
  image delivery and visual desktop/mobile verification remain open.
- Bounded viewport reader: nearby chapters load through IntersectionObserver with
  at most two concurrent requests and eight retained document responses. Chapters
  outside the viewport are evicted while measured heights preserve their place in
  the continuous book; TOC targets take priority. Observers disconnect on teardown.
  Tests exercise request/cache limits, height retention, observer lifecycle and
  GFM table alignment. Production build and all sixteen frontend tests pass.
  Real-browser scroll/320px checks and worst-case large-chapter DOM performance
  remain open; no browser performance acceptance is claimed.
- Direct search positioning: the find API accepts a zero-based start_at as an
  alternative to its snapshot/query-bound cursor and returns the page's starting
  position. Conflicting cursor/position inputs fail explicitly after authorization.
  PostgreSQL HTTP coverage verifies last-result seeking against the old snapshot
  after main advances, stable match identity and conflicting-input rejection.
  API tests and workspace Clippy pass. This enables bounded backward wrap-around;
  the Angular find controller and all-visible-result highlighting remain open.
- Paged find controls: the reader now supports Ctrl/Cmd+F, Escape, Enter,
  Shift+Enter and arrow navigation, accessible full-snapshot counts and wrap
  announcements. Queries debounce for 200ms, cancel superseded requests, bind all
  pages to one commit and retain only three pages. Navigating to a match loads its
  chapter before scrolling; retained results are highlighted across formatted text.
  Five controller tests plus reader shortcut coverage prove cancellation, direct
  backward wrap, cache limits, denied access, version mismatch and code-point query
  limits. Production build and twenty-one frontend tests pass.
  FIND-05 remains partial: matches outside retained result pages are not yet marked
  even when their chapters are visible. All-visible highlighting, result-context
  navigation, durable indexing, large-hit/large-chapter browser performance and
  the six-user p95 target remain open; this checkpoint is not search acceptance.
- Binary provider stage: GitProvider now exposes explicitly bounded, commit-pinned
  raw blob reads. GitHub resolves the requested commit/path to an immutable blob,
  validates metadata size before the REST fetch, then verifies returned identity,
  encoding and decoded size. Binary data is not coerced through UTF-8. The new
  provider test covers non-UTF-8 bytes, size bounds and mismatched blob identity;
  all ten adapter tests and workspace Clippy pass. Image validation and serving
  are separate next steps; no public asset path is exposed by this checkpoint.
- Image display validation stage: canonical content-hashed PNG/JPEG/WebP inputs
  are checked against their bytes and actual codec, decoded under dimension/pixel
  and allocation limits, oriented from EXIF and resized to at most 2048px per side.
  A fresh PNG encoder copies pixels only, omitting source EXIF/ICC metadata; output
  writes are bounded to 16MiB. Three media tests verify supported formats, hash/
  extension mismatch, disallowed content, byte/dimension limits, resizing and a
  synthetic private EXIF marker's removal while preserving orientation. All
  seventeen engine tests and workspace Clippy pass. Decoder allocation limits are
  best-effort in the codec library; strict dimensions/pixel checks also apply.
  Reference: https://docs.rs/image/0.25.10/image/struct.Limits.html . The HTTP proxy
  and UI integration remain the next checkpoint; upload/original retention policy
  is still part of the later proposal/media workflow.
- Authorized image delivery: a session-protected, commit-required asset endpoint
  proves main-history provenance, validates canonical asset paths after repository
  authorization, bounds fetch/decode concurrency to two and rechecks current access
  after creating display pixels. Responses are PNG-only, no-store and nosniff;
  original bytes/provider URLs are never exposed. Missing blobs return 404, invalid
  content 422 and oversized assets 413 with limits. PostgreSQL HTTP coverage proves
  cross-organization denial before provider reads, unauthenticated denial, history
  isolation, old-SHA reads after main advances, corrupt/oversized/missing assets and
  revocation during processing. The Angular renderer generates only same-origin,
  SHA-pinned asset URLs, lazy-loads them and offers a retry notice on errors.
  The old synthetic PNG had an invalid IDAT CRC: the strict decoder rejected it.
  Its CRC and SHA-256 filename/references were repaired, with a fixture decode
  regression check. Full workspace tests, Clippy, production build and twenty-two
  frontend tests pass; OpenAPI and generated client types include the image route.
  Real-browser image/layout verification, upload validation/retention policy and
  pre-merge media/link checks remain open; FILE-01–04 are not fully accepted yet.
- Find focus correction: opening find remembers the initiating focus target;
  closing it restores that target without scrolling, with the main-content region
  as fallback. Focus references are dropped on account/version reset and after
  restoration. Reader DOM coverage now verifies shortcut opening, external-field
  exclusion, Escape closing and focus return. The production build and all
  twenty-two frontend tests pass. Browser discovery was retried after image
  integration and still returned no connected browser; visual gates remain open.
- Canonical visible-highlight engine: viewport intervals now select highlight
  ranges from the same normalized whole-block scan and stable identity function
  used by global find. Formatting/viewport boundaries cannot change matching.
  Requests are limited to 64 windows and 32,768 UTF-16 units; results are bounded,
  ordered and deduplicated across overlapping windows. Three new tests verify a
  match beyond result 1,500 against paged identity, Hebrew/emoji/whitespace/literal
  parity, semantic boundaries and work limits. All twenty-one engine tests and
  workspace Clippy pass. HTTP delivery and viewport rendering are the next steps;
  FIND-05 remains open until those layers are connected and verified.
- Authorized visible-highlight transport: a commit-required document endpoint
  accepts bounded base64url MatchWindow lists, uses the complete document's
  canonical blocks in a bounded processing pool and rechecks access before
  returning ranges. It shares global search readiness and never mixes versions.
  PostgreSQL coverage proves paged/visible identity, old/new commit isolation,
  access checks before malformed windows/provider reads, invalid document/window
  failures, limits and revocation. API/application tests and workspace Clippy pass.
  The OpenAPI contract describes windows and compact ranges. Viewport integration
  and browser performance/visual checks remain open.
- Viewport highlight controller: a disposable Angular controller batches at most
  32 text windows per request, keeps one request outstanding per rendered chapter,
  cancels on query/version changes, rejects mismatched ranges and retains at most
  128 offscreen windows in addition to currently visible ones. Text leaves split
  at grapheme boundaries into 1,024 UTF-16 windows; even a very large grapheme uses
  a bounded request interval while preserving its original display text. Five new
  tests prove batching, cancellation, cache eviction, mismatch/denial handling,
  empty-query clearing and Unicode-safe chunking. Build and all twenty-seven
  frontend tests pass. Renderer observation and visual mark integration follow.
- Visible mark integration: native text leaves are now observed as grapheme-safe
  windows. The renderer requests canonical ranges for all nearby text independently
  of retained navigation pages, merges stable IDs, and marks cross-format matches
  without re-normalizing text in JavaScript. Line-break matches have addressable,
  visible marks too. Parts are computed once per range/tree change rather than
  rescanning every text leaf on active-selection changes. Query clearing removes
  marks; denied highlight access clears the reader, and observation/request state
  is disposed with each chapter. A DOM/HTTP regression verifies nonpaged Hebrew
  matches across bold text, active styling, query clearing and observer teardown;
  another verifies line-break targets. Build and twenty-nine frontend tests pass.
  FIND-05's retained-page highlighting gap is implemented across engine/API/UI.
  Real-browser visual/scroll acceptance, dense-document DOM performance, durable
  indexes and the six-user p95 target remain open; full search acceptance is not
  claimed from DOM tests alone.
- Persistable canonical index text: normalized blocks now have a versioned compact
  source-map format with checksums, structural/allocation validation and explicit
  corruption failures. Grapheme mappings use regular runs with delta/varint storage;
  persisted matching shares direct-search match identities and excerpt construction.
  Four new tests cover pagination/Unicode parity, 256 deterministic mixed-Unicode
  cases, compact regular runs, candidate book order and corrupted/versioned data.
  All twenty-five engine tests and workspace Clippy pass.
  The release indexed benchmark (20MiB, 1,000 documents, six concurrent readers,
  120 samples) measured 0.005s engine p95, 0.512s construction and 23,200,000 bytes
  of encoded maps on this machine. This excludes PostgreSQL, HTTP and browser work
  and does not prove the complete performance gate. Run with `cargo run --release
  -p specspace-document-engine --example find_benchmark -- --indexed`.
  PostgreSQL persistence, worker scheduling and runtime use are still pending.
- PostgreSQL derived-index schema: snapshots now bind repository/SHA/normalizer
  version to a scoped job, readiness metadata and a complete ordered block set.
  Text payloads deduplicate only within an organization/version; their normalized
  text, original text and source maps are immutable. A READY transition validates
  contiguous ordering, counts and byte totals; block writes take a parent lock so
  publication cannot race a partial write. READY blocks/snapshot metadata cannot
  mutate; deleting a derived snapshot cascades its references for safe rebuilding.
  Three PostgreSQL scenarios prove atomic/immutable/rebuildable publication,
  organization/version isolation and concurrent publication fencing. Migration
  applied locally; all workspace tests and Clippy pass. The snapshot publisher,
  query store, index worker and runtime readiness wiring are still pending.
- Durable snapshot publisher/query store: prepared index snapshots bind source
  hashes, document order and complete canonical blocks before a database write.
  Authorized enqueue is idempotent per repository/SHA/version. Index-only leases
  support heartbeat, bounded expired-lease recovery and terminal malformed/exhausted
  jobs; generic Git-operation leasing excludes index jobs and never reclaims their
  unrelated side effects. Publication fences the current owner and pending job,
  verifies immutable shared payload reuse byte-for-byte, and commits READY plus
  job success together. Conflicting payloads roll back without a readiness marker.
  PostgreSQL literal `strpos` selects candidates from one repeatable-read snapshot;
  restored canonical maps produce counts/pages/identities with a 128MiB, 4,096-entry
  disposable payload cache and fresh ACL checks before/after each read. Allocation
  accounting includes buffer capacities. Four added PostgreSQL scenarios verify
  literal/Unicode parity, pinned old/new pages, sharing, revoked/foreign access,
  idempotent enqueue, heartbeat/stale-owner fencing, rollback and job termination.
  Two application tests cover prepared source identity and rejected incomplete data.
  All workspace tests and Clippy pass. The long-running indexing worker, retries,
  indexed highlights, runtime HTTP routing and DB/HTTP performance gates remain
  pending; the reader still uses its existing computation path at this checkpoint.
- PostgreSQL performance regression and bounded-memory fix: a manual release test
  now builds an exact 20MiB source book with 1,000 distinct chapters, publishes it
  into an isolated database and runs 120 varied-query reads across six distinct
  active users. Its initial cold read exposed a ~70s candidate-selection delay.
  Snapshot-key materialization plus a lateral exact-payload lookup removed repeated
  large-text predicate work without materializing the large strings themselves.
  Dense decoded maps also exceeded the intended cache working set; maps now retain
  their compact persisted bytes plus sparse checkpoints and a current-run cursor.
  The existing SSM1 format, checksums, canonical ranges and match identities remain
  compatible. A format/working-set regression test covers this representation.
  The final local run measured build 0.795s, publication 1.518s, cold store read
  0.872s and warm store p95 0.554s; encoded maps totaled 23,078,000 bytes. A traced
  follow-up read fetched zero payloads. The cache remains bounded at 128MiB.
  The manual test enforces cold-read <2s and warm-store p95 <1s; it is ignored by
  ordinary unit runs and was executed explicitly with `cargo test --release -p
  specspace-infrastructure --features postgres-tests --test search_postgres
  indexed_store_six_reader_20mib_benchmark -- --ignored --nocapture` and DATABASE_URL.
  Full workspace tests and Clippy pass. These are store-only feasibility results;
  worker/runtime wiring, HTTP timing and browser acceptance remain open.
- Durable indexing worker: a scoped Git source checks repository/organization
  identity, reuses only the approved reader cache and otherwise loads a pinned
  main-history book through the existing provider boundary. Reader seeding still
  requires fresh browser ACL checks on every later read. Workers claim index-only
  leases, heartbeat every 15s, bound job duration, classify transient/permanent
  errors and honor provider retry delays. Failure/requeue updates are lease-fenced
  and atomic with snapshot state; retries stop at the job's attempt limit.
  Shutdown cancels in-flight read/build work and leaves its lease recoverable.
  Four PostgreSQL scenarios prove delayed retry, terminal source errors, scoped
  cache reuse and shutdown/restart with exactly one publication. All eleven
  non-performance index scenarios and workspace Clippy pass. API startup, indexed
  reader routing and client readiness polling remain the next integration layer.

- API index-worker lifecycle: application state now shares one configured Git
  source and approved reader cache with durable indexing. API startup validates
  a 30–3600 second job timeout (900 default), starts exactly two workers, and
  cancels both on SIGINT, SIGTERM or server exit. Leases remain recoverable after
  cancellation. API unit/PostgreSQL integration tests and workspace Clippy pass.
  Search endpoints still use the prior computation path in this commit; durable
  query routing and client readiness polling follow separately.

- Persisted main-history proof: the reader can reopen an approved pinned SHA
  after cache loss without depending on a later main ref still containing it.
  Proof requires the exact repository/SHA/normalizer READY snapshot and a
  successful INDEX_SNAPSHOT job in the same organization. READY content from a
  proposal job is insufficient, including for durable find queries. Actual Git
  metadata/content validation and current browser ACL checks still apply. API
  sources share this proof with the reader. New cold-reader and PostgreSQL tests
  cover historical reads, unknown commits, revoked users and proposal exclusion.
  Full backend tests and Clippy pass (12 durable-index scenarios; the separate
  release performance benchmark remains intentionally ignored in ordinary runs).

- Durable reader query routing: opening a structurally valid book idempotently
  queues its pinned index; find reads complete persisted snapshots with current
  ACL checks. A first direct search proves main history and queues missing work.
  Pending work returns retryable 409 with job/SHA and Retry-After: 2; terminal
  failures remain explicit and never become zero results. Invalid books keep
  their existing readable findings and structural search error. Visible highlights
  retain the canonical bounded renderer path while full-book navigation uses SQL.
  Six API/PostgreSQL scenarios pass, including pending ticket reuse, publication,
  search after process/cache loss without a Git round-trip, foreign scope denial,
  and failed new snapshots alongside a working older version. API tests and
  workspace Clippy pass. Client polling is the next stage.

- Reader index readiness: find now recognizes retryable pending-index responses
  only for the selected SHA, shows preparation status and keeps the match count
  unknown. It polls every 2s in a visible tab / 10s while hidden, with one request
  at a time and at most 90 automatic retries. Query/version/access changes and
  disposal cancel work; terminal failures stop polling. A bounded-wait message
  offers explicit retry after that limit. Canonical visible highlights continue
  to work independently during preparation. Three new client scenarios cover
  pending-to-ready, background timing, cancellation, terminal/mismatched state
  and the retry bound. Angular production build and all 32 tests pass. Browser
  visual acceptance and end-to-end HTTP performance remain open.

- Authenticated HTTP performance gate and fix: a real loopback Axum test uses
  six distinct server sessions, PostgreSQL, a ten-connection pool, the actual
  reader/worker/renderer, exactly 20MiB in 1,000 chapters and 8,000 matches.
  Distinct visible payloads must exceed 19MiB so fixture deduplication cannot
  hide database work. The initial valid workload failed at 1.875s search p95;
  phase timings showed repeated SQL text scans and processing-slot queueing.
  Search now reads immutable ordered references, matches verified cached
  payloads in Rust and fetches only uncached matching payloads through scoped
  SQL lookups in the same repeatable-read transaction. The 128MiB / 4,096-entry
  cache and fresh ACL/provenance checks remain enforced. A new PostgreSQL test
  compares successive disjoint queries, literal metacharacters, nonmatches and
  paging against direct canonical search. Full backend tests and Clippy pass.
  Explicit release HTTP measurement: fresh no-match p95 0.112s, first positive
  query 0.655s, warm search p95 0.079s, chapter HTTP p95 0.035s and metadata HTTP
  p95 0.047s, with 120 samples per route and no warm Git content fetches. The
  earlier store-only benchmark also passes: cold 0.680s, warm p95 0.054s.
  Reproduction commands and scope are recorded in docs/reader-performance.md.
  In-app Browser still reports no available browsers; visual/accessibility
  acceptance and browser-visible navigation/first-content timing remain open.

- Proposal creation intent: typed, deny-unknown-field input validates Hebrew
  plain-text metadata, the immutable change/ branch, main-only source SHA,
  initial navigation mode, UI-impact reason and bounded typed HTTPS links.
  Idempotency keys are validated separately; a deterministic SHA-256 request
  fingerprint includes all intent and resolves an omitted owner to the actor.
  Two new application tests verify invalid-field reports, key limits, default
  owner equivalence and intent changes. Application/infrastructure tests and
  workspace Clippy pass. This is the creation model; API and Git provisioning
  are not exposed by this commit.

- Atomic proposal reservation: an additive migration records initial navigation
  mode and typed links, and enforces one CREATE journal/job per proposal.
  PgProposals rechecks active actor/owner eligibility and same-organization team
  scope, serializes idempotency keys and atomically writes PROVISIONING identity,
  permanent case-folded branch reservation, CREATE operation, queued provisioning
  job, audit and outbox event. Same-intent replay returns the original ticket;
  changed intent conflicts without additional work. No provider call occurs
  inside the transaction and no Git success is implied by reservation. Three
  PostgreSQL scenarios prove concurrent replay, collision rollback, permanent
  reservation after closure and scope/revocation checks. Application and all
  infrastructure tests, API compatibility tests and workspace Clippy pass; the
  migration is applied locally. Provisioning execution, API endpoints and UI
  remain the next layer.

- Provider mutation readback: an accepted branch creation followed by a failed
  ref read now reports an uncertain result rather than a safe-to-retry failure.
  Uncertain readbacks retain any provider Retry-After delay, including Commit
  verification, so reconciliation cannot retry earlier than the provider allows.
  A provider HTTP test proves one accepted create, a delayed uncertain result
  after rate-limited verification, and later observation of the existing ref.
  All eleven provider tests and workspace Clippy pass.

- Proposal provisioning journal and worker: CREATE jobs have an independent
  lease protocol, excluded from generic leasing. A short fenced transaction
  records the mutation boundary before provider I/O. Expired preflight work may
  restart; a started/uncertain operation can only observe its reserved ref.
  A matching ref completes atomically as DRAFT with identical base/head/target,
  a successful operation/job, audit and outbox event. Existing refs in fresh
  work are rejected, and divergent/absent uncertain refs are never rewritten.
  Attempts, heartbeat, timeout, delayed retry and terminal recovery status are
  explicit. Actor/owner/team eligibility is checked before mutation and before
  publishing; stale workers cannot settle work after lease loss. Job intent
  binds repository remote and installation identity, preventing retargeting
  after reconfiguration. No DB transaction spans a provider call. Fourteen
  PostgreSQL scenarios cover reservation, phase recovery, rate limits before
  and after writes, shutdown after accepted creation, missing/divergent refs,
  moved main, existing branches, revocation, malformed payloads, exhaustion and
  configuration changes. Full backend tests and workspace Clippy pass; the
  added configuration/terminal-state cases pass in the final focused run.
  API startup now runs one provisioning worker alongside two index workers;
  its 30–3600s timeout defaults to 120s and uses the same graceful shutdown.
  HTTP creation/list/job endpoints and the proposal UI remain the next stage.

- Proposal creation and job HTTP API: POST /api/v1/proposals now authenticates
  the current session, verifies same-origin CSRF, validates a single idempotency
  key and bounded typed JSON, checks create permission before provider access,
  returns an existing ticket before consulting mutable Git refs, and verifies
  the displayed main SHA and absent destination before atomic reservation.
  SOURCE_MOVED is an explicit 409 with expected/actual SHAs; changing the request
  under an existing key conflicts. A 202 returns the proposal/operation/job IDs
  and Location, without implying that Git creation has finished. GET /jobs/:id
  resolves organization/repository access through linked proposal or index
  records, exposes real phases and verified creation results, and never returns
  internal payloads or provider configuration. Unknown operations remain visibly
  in reconciliation, including while queued for delayed observation. Six new
  HTTP/PostgreSQL scenarios cover concurrent replay, source movement, validation,
  CSRF/roles/foreign scope/revocation, accepted-to-ready and uncertain recovery,
  plus index-job phase and scope. Full backend tests and Clippy pass. Shared
  creation models have optional OpenAPI support; the exported contract and
  generated TypeScript types are updated, and Angular builds successfully.
  Proposal browsing/detail/context endpoints and the creation UI follow next.

- Proposal browsing/detail APIs: scoped GET /proposals supports literal title/ref
  search, status/team/owner/repository filters and bounded keyset pagination.
  Cursors bind actor, filters, limit and exact timestamp/UUID order; foreign
  repositories are hidden. GET /proposals/:id exposes recorded versions, metadata,
  current capabilities and separate live Git observations. Missing/moved refs,
  provider failures and outdated targets do not overwrite recorded head/base or
  change workflow state; scope and capabilities reload after provider awaits.
  GET /proposals/options provides bounded current repository/team/eligible-owner
  choices without provider configuration. Equal base/head gives exact zero
  change counts; other counts remain unknown until the diff stage computes them.
  Three new HTTP scenarios cover tied-timestamp pagination, cursor/filter/actor
  mismatch, literal metacharacters, scope, collaborator permissions, provider
  changes, closed branches, revocation and eligible choices. All nine proposal
  API scenarios, the remaining API tests and workspace Clippy pass. OpenAPI and
  generated client types are updated; proposal list/detail UI follows separately.

- Connected proposal list and metadata workspace: the RTL list replaces the
  static placeholder with authorized server data, URL-restored literal search,
  state/repository/team/owner selectors, bounded cursor navigation, exact zero
  counts where known, explicit unknown counts and separate loading/error/empty
  states. Detail URLs restore identity, recorded branch/target/versions, metadata,
  links, source-version navigation and live synchronization notices. Pending
  provisioning polls scoped jobs every 2s / 10s in hidden tabs, stops on terminal
  outcomes or 90 retries, and reloads verified metadata after success. Private
  state and pending requests/timers are disposed on identity, route and access
  changes. A pending server detail keeps its original coherent metadata snapshot
  when no provider call is needed, avoiding a mixed provisioning/ready response.
  Six state tests and a URL/identity component test cover cancellation, failures,
  private-data clearing, polling success/termination/bounds and mismatched IDs.
  Angular build and all 39 client tests pass; nine proposal API tests and Clippy
  pass after the consistency guard. The in-app Browser still lists no browsers,
  so rendered visual/accessibility acceptance remains open. The creation form,
  proposal content reader/editor and editing/review operations are not complete
  in this commit and remain part of the full implementation.

- Exact creation previews: authenticated/CSRF-protected POST /proposals/context
  idempotently reserves a number from the proposal sequence and records the
  server-read main SHA for 30 minutes. Preparation creates no proposal, Git ref
  or job. Optional creation_context_id binds final creation to the same actor,
  repository, source and provider identity; one transaction consumes its exact
  number. Concurrent consumers cannot reuse it, while ordinary creation still
  shares the sequence safely. A composite FK binds context/number/author/scope.
  Expired, consumed or changed contexts fail explicitly; replay of an already
  accepted creation still returns its original ticket after expiry. Legacy
  requests omit the new field from canonical serialization, with a byte-level
  compatibility test preserving existing idempotency fingerprints. Three new
  PostgreSQL scenarios and one HTTP scenario cover allocation/consumption races,
  scope/source/configuration/expiry and source-move/replay behavior. Application,
  infrastructure and API tests pass; Clippy passes and migration 005 is applied
  locally. OpenAPI/client types are updated. The form UI follows in a separate
  commit.

- Proposal creation UI: /changes/new now prepares an exact numbered main-source
  preview, suggests an ASCII branch with a Hebrew-title fallback, preserves
  custom branches, captures owner/team/mode/UI-impact reasoning and typed HTTPS
  links, and submits through the existing idempotent CREATE API. Both the book
  and proposal list expose a creation action using repository-specific server
  capabilities. Source movement requires explicit preparation and a new click;
  text remains intact. Uncertain requests freeze their original body/key and
  are retried unchanged, with an explicit choice before abandoning them. Local
  human-entered recovery data remains only in memory for the same identity and
  is purged for a different identity; nothing resubmits on login. Navigation and
  unload guards protect unsent input, and an accepted ticket opens its pending
  proposal without claiming Git completion. Refreshed eligibility never silently
  replaces a previously chosen owner/team. Field errors have labels/associations;
  missing repository/team and authentication states remain explicit. Seven new
  client scenarios cover preview/branch behavior, unchanged retries, source
  movement, identity isolation, preparation retry keys, response mismatch and
  owner fidelity. Angular production build and all 46 client tests pass. The
  repository capability API is covered for Reader/Contributor; API tests and
  workspace Clippy pass. Browser visual/accessibility acceptance remains open.
  Proposal content reading/editing, review and the AI bundle workflow are still
  required subsequent stages of the full specification.

- Provisioning source metadata: new reservations persist the selected base and
  target SHA immediately, while head remains absent until Git is verified.
  Pending records from earlier versions read their immutable source from the
  CREATE journal. Integration assertions cover persisted values, legacy pending
  records, no provider reads during provisioning and the eventual verified head.
  All 17 infrastructure and 10 API proposal scenarios pass; workspace Clippy
  passes. This preserves source-version visibility without claiming completion.

- Private proposal reader API: proposal-scoped specification, document, literal
  find, visible-highlight and raster-display routes read the recorded head or a
  proven ancestor. They preserve closed/missing-ref history, reject arbitrary
  commits and return an explicit preparation conflict before a verified head
  exists. Current membership and recorded scope are checked before reads, after
  waits and before publishing CPU/provider results, including cache hits. A
  separate four-snapshot cache never feeds approved-book provenance or indexing;
  proposal search computes the complete pinned snapshot without enqueueing main
  jobs. Three HTTP/PostgreSQL scenarios cover cross-proposal cursor boundaries,
  private/main isolation, pinned content/history, changed anchors, pending/closed
  states, membership loss and image-read revocation. Full workspace tests and
  Clippy pass; OpenAPI and generated client types include all five routes.
  The continuous proposal UI follows separately. Proposal search performance at
  the maximum book size is not yet measured; the approved-book benchmark remains
  separate evidence.
  Unique OpenAPI operation IDs have a regression assertion after generated
  client compilation exposed a naming collision. Angular build and all 46
  existing frontend tests pass against the regenerated contract.

- Continuous proposal UI: /changes/:id/book reuses the bounded chapter reader
  through proposal-scoped endpoints and is linked from verified proposal detail.
  Number, title, branch, target and selected SHA remain visible; the page clearly
  identifies proposal content and closed/merged history. Internal document/section
  links preserve proposal identity and SHA. Search, visible highlights and hashed
  image derivatives use the same private scope; switching proposals at an equal
  repository/SHA cancels pending requests and clears derived data. Unknown access,
  unverified heads and inconsistent metadata produce explicit errors, with no
  approved-book fallback. Five new frontend scenarios cover chapter cancellation,
  pending/mismatched metadata, same-SHA search/highlight isolation and route/label/
  link/logout behavior, alongside private image-path assertions. Production build
  and all 51 frontend tests pass. CI for the preceding API commit is green in both
  workflows. In-app Browser availability was checked again and remains empty;
  rendered visual/accessibility acceptance is still open. Editing, draft recovery,
  atomic commits and subsequent review/import/merge stages remain required.

- User-private recovery backend: GET lists only the current user's metadata or
  retrieves one document's exact unfinished source; PUT uses an immutable save ID
  and expected buffer version, preserving accepted retries and rejecting other
  tabs' stale saves. Recovery remains independent of Git and records its claimed
  base alongside the current proposal head/version for comparison. Closed/merged
  proposals retain recovery storage for current authorized editors; revocation
  stops writes, while current members can read/discard their own existing buffers.
  Even administrators cannot read another user's buffers through these routes.
  Versioned DELETE protects recreated buffers from delayed discard requests.
  Migration 006 adds a global non-repeating version sequence, save fingerprints,
  timestamps and storage constraints. A configurable DRAFT_RETENTION_DAYS policy
  is returned in context; absent policy disables writes. Expired/shortened-policy
  content is hidden immediately and a bounded background task purges it. Per-user
  proposal quotas are 20MiB / 1,000 documents, each at most 2MiB. DB lock/statement
  waits are bounded; audit contains references/results without document content.
  Two application validation tests and seven HTTP/PostgreSQL scenarios cover
  byte preservation, privacy, concurrent saves, replay/mismatch, stale/final heads,
  current ACL, retention/cleanup, discard recreation, both quotas and CSRF. Full
  backend regression tests passed before the discard addition; the final API
  suite and workspace Clippy pass afterward. OpenAPI and client types are updated.
  Migration 006 is applied locally; Angular production build and all 51 existing
  frontend tests pass against the generated contract.
  The rich editor, recovery prompts/autosave UI and verified Git commits remain
  subsequent work; this backend does not satisfy the complete editing criterion.
