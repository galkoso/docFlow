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

- Durable expiry correction: a regression reproduced an expired buffer becoming
  visible under a longer policy before cleanup. Reads/saves now persist shorter
  expiry, and startup applies the configured cap to unread buffers before serving
  traffic. Increasing or removing retention cannot extend those recorded expiry
  times. Cleanup also applies the cap before its deletion batch. The regression
  and a new startup/unread-buffer scenario pass alongside all eight recovery API
  scenarios; workspace Clippy passes. A configured recovery policy now requires
  PostgreSQL availability at startup, documented in README. No content, buffer
  versions or migration checksums are rewritten by expiry maintenance.

- Editor source boundary: document-engine now exposes a bounded lossless block
  model and assembler. Retained blocks preserve raw Markdown, unknown YAML,
  heading markers, code fences, link definitions and image paths. Unsupported
  source becomes protected atoms; rich edits cannot silently replace/delete it.
  Full ordered requests require explicit block and section deletions, enforce
  source-hash identity, preserve renamed/moved heading IDs, reject duplicate IDs
  including nested headings, and reparse to detect consumed neighbor boundaries.
  Removed IDs are returned for the later repository reference validator. Eight
  engine scenarios cover the six repository fixtures, all supported block kinds,
  nested IDs, protected source, partial/stale/duplicate requests, moves/deletions,
  UTF-8/Hebrew preservation and the 10,000-block/2MiB limits. This is the server
  source-preservation foundation; rich browser editing, repository reference
  validation and atomic Git persistence remain required. The protocol and its
  limits are recorded in editor-source-model.md.
  All 34 document-engine tests and workspace Clippy pass.

- Editor model/preview API: proposal/document-scoped GET returns exact source
  blocks plus current identity/version/capabilities; pinned historical or closed
  models are readonly. CSRF-protected POST validates current edit assignment and
  head/proposal/document versions, assembles explicit changes and returns raw
  draft source, SHA-256, inert rendering, findings and removed IDs. It rechecks
  access after provider/CPU waits and never invents a Git snapshot for preview.
  Five HTTP/PostgreSQL scenarios cover unchanged/changed source, protected HTML,
  assignment grant/revocation, Reader/closed/historical behavior, cross-org scope,
  stale hashes/versions, CSRF and permission loss during provider awaits. They
  also reject catalog/front-matter document identity mismatch before editing.
  assert proposal head/version and operation journal remain unchanged by preview.
  Browser rich editing, source repair for malformed documents, recovery autosave
  integration and atomic Git saves remain required subsequent work.
  Full backend regression checks passed before the identity guard; all five
  editor scenarios and Clippy pass afterward. OpenAPI/client types are updated;
  Angular production build and all 51 frontend tests pass.

- Browser rich-document core: pinned ProseMirror/Markdown dependencies and typed
  editor/recovery API methods support the source model. The schema handles
  CommonMark, simple GFM tables/alignment/escaped pipes, strike, task states and
  nested stable heading markers; known schema DOM and private image URLs remain
  inert. Unrepresentable/non-round-trippable source is protected. Retained nodes
  preserve original bytes; changed nodes emit explicit edits/deletions. Identity
  normalization preserves originals when copied before their source, gives new
  headings fresh IDs and clears duplicate source keys on split. Protected source
  cannot disappear in rich transactions. Undo/redo restores original Markdown.
  A deterministic Rust fixture exporter and backend CI drift check connect
  browser tests to actual server models/canonical edits. Six new client scenarios
  cover seven fixture documents, no-op/changed/undo/redo source, identity, protected
  deletion, GFM details and inert DOM/private images. Tests caught and fixed an
  attribute mutation in semantic comparison; the comparison now builds fresh
  objects. Production build and all 57 client tests pass; fixture regeneration
  matches and workspace Clippy passes. Page/toolbar, session recovery/autosave,
  source repair and Git commits remain required; this core is not a complete
  user-facing editor yet. Browser rendering/IME/RTL acceptance remains open.

- Rich editor component: standalone controls now drive ProseMirror formatting,
  H1–H4/code blocks, lists/tasks/quotes, simple table insertion/row/column actions,
  links, image descriptions and Undo/Redo. Heading level changes retain each
  selected block's ID, including nested headings. Source atoms use inert details/
  text views. HTML paste conversion is announced; unsupported complex-table paste
  falls back to text and protected-source paste is blocked explicitly. Permission
  changes preserve the local document while transaction guards reject readonly
  mutations, including keyboard history. Link/image fields can be flushed by the
  future page save boundary and remain available if permissions change. Four new
  component scenarios exercise actual editor DOM/commands, nested identity,
  undo/redo, permission changes, invalid links, inert protected source and simple
  table shape changes. Geometry is mocked in jsdom; these are not browser layout,
  RTL/IME or accessibility acceptance. Production build and all 61 frontend tests
  pass. This reusable component is not yet connected to a proposal route;
  page/session/recovery integration, source repair and Git save remain required.

- Authorized source/recovery preview: source GET returns exact pinned bytes and
  syntax/model diagnostics even for invalid front matter or mismatched catalog
  identity. It permits repair while restricting paths to catalogued docs/*.md;
  broken catalog entries cannot expose other repository files. Source POST binds
  complete user-entered Markdown to its original SHA-256 and proposal version,
  returning inert rendering, findings and an optional rich model. Unfinished
  recovery source remains intact. Assigned editors can compare closed/historical
  bases with editing disabled; no Git, buffer or review write occurs. Access is
  rechecked after provider/CPU waits. Both preview paths bound returned findings
  at 1,000 with truthful totals/truncation, and removed-ID comparisons are nullable
  when parsing fails. Four additional HTTP/PostgreSQL scenarios cover repair,
  unfinished/mismatched input, historical/closed comparison, roles/CSRF/stale
  versions, foreign scope, provider-wait revocation, diagnostic bounds and an
  attempted catalog escape. Full backend checks passed before the nullable-ID
  refinement; the final API regression suite and workspace Clippy pass afterward.
  OpenAPI and typed source client methods are updated. Page/recovery integration
  and actual Git saves remain required subsequent work.
  The regenerated client builds and all 61 frontend tests pass.

- Recovery request identity: every private draft read/save/discard now requires
  the UUID captured with the editor intent in `X-Expected-Actor`. Authentication
  rejects malformed, duplicate or mismatched supplied headers across endpoints;
  cookies and current permissions remain the sole authority. Editor API methods
  take the captured actor explicitly, so a frozen retry cannot silently adopt a
  new account from shared cookies or refreshed profile state. A PostgreSQL HTTP
  scenario exercises all three recovery verbs with missing/invalid/duplicate/
  foreign actor bindings, checks no buffer or audit write, and confirms correctly
  bound requests work. All nine recovery scenarios, workspace Clippy, and all
  62 frontend tests pass. OpenAPI and generated client document the required
  recovery header. Editor page/autosave integration remains subsequent work.
  The full backend workspace regression suite and Angular production build pass.

- Recovery autosave controller: component-owned memory binds every buffer and
  immutable save intent to actor/proposal/document/base. Two idle seconds queue
  one write; newer typing waits for its acknowledgement and returned CAS version.
  Twenty-second timeouts, interrupted requests, service errors and invalid
  acknowledgements retain the exact original request for explicit retry only.
  Later conflicts cannot erase earlier uncertainty. Reads never replace local
  input or acknowledge unknown writes. Existing/changed buffers require an
  explicit restore/local choice; adopting another base is a separate action.
  Same-actor session recovery rechecks context without automatic resend; account
  changes or successful logout purge source, remote data and frozen requests.
  Policy/permissions, byte bounds, Undo back to Git source, and closed-branch
  recovery remain explicit. Twelve HTTP-controller scenarios pass; the frontend
  suite passed all 74 tests before page integration. This controller does not
  create Git commits; editor route integration follows as a separate stage.

- Proposal editor route: chapter/section links open an actor/document/SHA-scoped
  rich or full-source editor, diagnostics and inert server previews. The page
  connects the two-second autosave controller, explicit recovery comparisons,
  different-base adoption, manual exact-intent retries and local recovery files.
  It truthfully distinguishes uncommitted source from server recovery and Git.
  Reads/previews validate identity/base/version and discard stale results.
  Session refresh hides private content, retains the same user's rich history
  and unfinished link/image fields, then rechecks current source and permissions.
  Fresh view/permission closures replace old ones on reattachment. Account
  changes and successful logout clear all private state. Logout guards and
  document/base navigation guards protect unsaved work, including when Angular
  reuses the route; section-only navigation retains the same buffer. Rich
  serialization failures hold Markdown autosave; download includes actual node
  state, protected source model, selection and unfinished fields as explicitly
  uncommitted JSON. Thirteen recovery-controller, eight page/router and session
  guard scenarios are covered; production build and all 84 frontend tests pass.
  Tests use jsdom with mocked text geometry. Browser discovery still returns no
  connected browser, so layout/RTL/IME/accessibility acceptance is not claimed.
  Atomic Git saving, incoming-reference blockers, review submission and the
  remaining full-specification workflow are still required.

- Git save recovery evidence: the provider can now read exact commit parents,
  tree, bounded message and timestamp, checking all returned object identities
  and rejecting malformed/oversized evidence. An operation matcher requires the
  exact single parent and one final server-owned actor/operation trailer block;
  ambiguous/forged fields, extra lines and nil identities fail. The adapter
  rejects reserved fields in user messages before any write. Existing metadata
  reads retain their smaller contract. Three additional scenarios cover trailer
  framing, malformed provider objects and injection rejection; all 14 adapter
  tests and workspace Clippy pass. Other test providers explicitly reject the
  new capability instead of fabricating evidence. `docs/commit-saves.md` records
  the boundary: identity matching alone cannot verify planned file contents,
  reachability or authorization. The actual save planner, durable journal/worker,
  authenticated API and editor Git-save interaction remain required next work.
  The full backend workspace regression suite also passes after this change.

- Candidate-book save validation: the document engine checks the complete new
  catalog and source set, global nested/empty heading identities, orphan markers,
  exact document/section ownership, incoming links from untouched chapters and
  referenced asset existence. URI decoding is strict, once only, and constrained
  to canonical repository paths. No external URL is fetched. Draft warnings keep
  protected source and missing alt visible; review/merge must apply stricter
  gates. Referenced images still require pinned hash/MIME/pixel verification by
  the eventual save worker. Diagnostic output retains 1,000 findings with full
  blocker/count/truncation metadata and bounded paths; internal identity/asset
  maps are not serialized. Catalog diagnostics stream instead of accumulating
  unbounded repeated findings. Tests exposed the parser's reference-expansion
  fallback; a bounded callback probe now blocks incomplete checks without
  narrowing first-definition-wins CommonMark semantics. Whole-book destination
  and definition-scan work are also bounded. Fifteen new engine scenarios cover
  actual six-document fixtures, cross-chapter deletions, URI attacks, diagnostic
  bounds, parser exhaustion and adversarial duplicate definitions. All 49 engine
  tests and workspace Clippy pass. Git save journaling, byte verification and
  publication remain separate required work.
  Full backend workspace regression checks pass; the editor contract fixture
  regenerates without drift.

- Existing-document save plans: an encapsulated intent binds actor/proposal/repo,
  expected head/version, message, exact base hashes and raw Markdown. Fingerprint
  hashing streams through a 64KiB buffer instead of allocating escaped payloads.
  Preparation resolves only catalogued docs, verifies original hashes, applies
  all changes together and validates the complete candidate book. Only changed
  files normalize to LF/NFC; unknown source/YAML remains intact, and raw or
  canonical no-ops produce no mutations. Input and normalized output both obey
  50-file/10MiB and 2MiB-file limits. Plans retain read-only scope/fingerprint,
  canonical source/hashes, normalization flags and nullable removed-ID evidence.
  Eleven new application scenarios cover atomic cross-document repair, no-ops,
  stale scope/hash checks, malformed/oversized input, normalization expansion,
  raw repair and exact hash bytes across buffered Unicode/escaping boundaries.
  Full backend tests passed before the plan accessor refinement; final application
  tests and workspace Clippy pass afterward. Worker authorization/provenance,
  referenced media verification, journal/reconciliation, API and editor Git-save
  integration remain required. These plans do not execute or claim Git saves.

- Durable proposal saves: migration 007 reserves one active Git mutation per
  proposal, immutable actor/provider/head/version-bound private intents and a
  source-free sealed manifest. A lease-fenced worker validates the pinned complete
  candidate, Git blob identities, modes and referenced media before one atomic
  write. No-ops leave Git/version/approval state unchanged. Started operations
  recover only through observation; exact content/tree/identity/reachability
  evidence is required before transactional publication, DRAFT reset and review
  invalidation outbox. Definite outcomes purge private input; unresolved UNKNOWN
  retains its reservation and private input pending operational resolution.
  Twenty new PostgreSQL scenarios cover concurrency/idempotency, revoked access,
  malformed jobs, immutable evidence, lease expiry, crash and database failure
  after Git, provider backoff during verification, forged evidence, external
  descendants and image bytes. Full backend tests and Clippy pass. Runtime starts
  one worker with a configurable timeout. Authenticated submission, editor Git
  interaction, formal review consumers, operational resolution/retention and real
  provider/browser acceptance remain subsequent work.

- Authenticated Git-save API: POST commits binds the captured actor, exact
  idempotency key and expected head/version to server-resolved repository scope.
  Accepted tickets claim queueing only. Exact retries survive head advancement
  and closure without creating another operation; fresh stale/closed work fails.
  GET jobs exposes actor-private typed outcomes and bounded diagnostics, requires
  current edit access and never includes raw source/provider configuration.
  Inconsistent success publications fail closed; UNKNOWN remains explicit.
  Intent hashing uses a bounded CPU slot. Six PostgreSQL HTTP scenarios and one
  Angular exact-retry identity scenario supplement the worker evidence. Full
  backend tests/Clippy, production Angular build and all 85 frontend tests pass.
  OpenAPI and generated types are updated. The editor Git control, local-edit
  rebasing after a verified save and broader review workflows remain open.

- Client Git-save lifecycle: a component-owned controller freezes the original
  actor/key/request, accepts one job, and observes without resending writes.
  Uncertain responses and exhausted UNKNOWN jobs keep the mutation pending;
  explicit exact retries, current-session recovery and private-state cleanup are
  covered. Bounded typed outcome checks include no-op/version/document identity
  invariants. Completed work stays pending until the editor installs a verified
  pinned baseline. Fourteen new controller scenarios pass (99 frontend tests
  total). Page controls, active-field flushing and preservation/rebasing of later
  edits remain the next integration stage.

- Editor Git saving: page controls flush active fields, submit captured source
  against exact head/version/hash, display bounded whole-book blockers and show
  SHA/time only for verified publication. A matching pinned read is required
  before advancing the local baseline; unavailable/mismatched reads keep the
  operation pending and fence new writes. Promotion retains later raw typing,
  rich history/fields and held unrepresentable state, with explicit canonical
  LF/NFC serialization after verification. Recovery keeps older frozen writes
  and CAS evidence across that base change. The actual router updates the commit
  URL without dropping local edits; other document/base transitions remain
  guarded. Eleven added page/router scenarios and three recovery scenarios pass
  (113 frontend tests); Angular production build passes. Browser connection is
  unavailable, so visual/RTL/IME/accessibility and real-provider acceptance remain
  open. Multi-document dirty-buffer coordination, new-document/import workflows,
  diff, review, merge and operational reconciliation are still required.

- Diff foundations: the provider exposes an exact merge-base capability; GitHub
  validates returned graph identities/relationship metadata and avoids the
  first-page patch list. Ancestor checks share that boundary; other test providers
  reject unsupported merge-base work explicitly. A byte-preserving raw line
  engine returns complete before/after ranges and hashes. Exact line interning,
  a two-million-cell matrix budget, line limits and bounded output prevent
  adversarial alignment expansion; coarse fallback is explicit and loses no
  source. Four engine scenarios include exhaustive short-sequence comparison
  against a brute-force minimality oracle and full reconstruction/coordinates;
  one adapter scenario covers graph consistency and backoff. Pinned proposal
  application/API/UI, semantic/move/image/catalog comparison and review anchors
  remain required. `docs/diffs.md` records the implemented boundary and remaining
  workflow; this does not complete DIFF-01–08 or any review acceptance scenario.
  Full backend workspace tests and Clippy pass after these changes. OpenAPI and
  the cross-language editor fixture regenerate without drift.

- Comparison inventory preparation: immutable complete tree/source snapshots
  verify Git-framed blob identity, declared/actual bounds and path uniqueness.
  Unique actual document IDs pair rename-plus-edit; exact-path and unique-blob
  fallbacks record their evidence method. Metadata distinguishes content, mode
  and absolute catalog-position changes; all assets/other files remain visible.
  Malformed, orphaned and ambiguous source/catalogs retain raw changes plus
  bounded full-count diagnostics. Eight application scenarios cover these
  boundaries, determinism, quotas and mixed repositories. The blob helper is now
  shared with save verification. This pure planner grants no authority and does
  not fetch or serve content: authorization, merge-base/target capture, bounded
  loading, API/UI and semantic/review anchors remain required next work.
  Full backend workspace regressions passed before the final diagnostic/quota
  refinements; final application tests (32) and workspace Clippy pass afterward.
  OpenAPI and the cross-language editor fixture regenerate without drift.

- Authorized comparison loader: current server scope binds actor/org/proposal,
  repository configuration, recorded head/base and metadata version/state.
  Frozen numeric GitHub connections prevent reconfiguration from retargeting a
  read. The loader proves historical head/main selections, reads the merge-base
  and head snapshots, checks bounds before I/O and validates source on bounded
  CPU. Rechecks after provider/CPU work block revoked or changed context. Late
  ref movement is reported without mixing source; removal of selected main
  lineage fails closed. One load/timeout bounds work, and cancellation retains
  the CPU permit until work ends. Eight application and three PostgreSQL
  scenarios cover these boundaries. Runtime application state shares the actual
  configured GitHub source; no HTTP diff route is claimed yet. Paged serving,
  heavy-load jobs/cache, semantic views and review anchors remain next work.
  Full workspace regression tests passed before the final readiness refinement;
  final application tests (40), targeted PostgreSQL checks and workspace Clippy
  pass afterward. OpenAPI/editor fixtures have no drift; the restarted local API
  reports ready with the new loader initialized.

- Durable comparison jobs and pages: migration 008 freezes actor/proposal/provider
  context and read request identity, with exact-key replay and serialized admission
  limits. A lease-fenced worker rechecks current scope, honors backoff and atomically
  publishes metadata plus contiguous immutable file rows. Pending/ready/failed
  state and bounded progress remain explicit; stale owners and partial DB writes
  cannot expose READY. Every private page rechecks current access/binding and reads
  persisted metadata without Git, including after restart. No source bodies or
  binaries are stored, and this read work never reserves/mutates Git. Identity
  extraction now parses front matter without expanding Markdown bodies. Twelve
  PostgreSQL scenarios cover multi-batch/empty publication, crash rollback,
  idempotency/quotas, immutable evidence, access changes, backoff and restart
  paging. The runtime starts one comparison worker. HTTP routes, file-level raw/
  readable views, body caching and review workflows remain subsequent work.
  Full workspace regressions pass; the final lease-accessor refinement also
  passes all twelve new PostgreSQL scenarios and workspace Clippy. OpenAPI and
  editor fixtures regenerate without drift. Migration 008 applied locally and
  the restarted API reports ready with one comparison worker running.


- Authenticated comparison HTTP contract: captured actor/request identity queues
  derived reads, with canonical snapshot polling and immutable inventory pages.
  Every page and generic job pointer rechecks current private scope and provider
  binding. Historical summaries retain their original head/version alongside
  current proposal state and explicit change flags. Terminal failure stays
  explicit without a fake ready page. OpenAPI and an Angular typed client cover
  the contract without automatic retries. Six PostgreSQL HTTP scenarios and two
  client scenarios cover identity, replay/quotas, paging, revocation, configuration,
  historical closure and missing setup. File bodies, readable/raw diff UI, review
  anchors and remaining review/merge lifecycle are still required.
  Full backend workspace tests and Clippy pass; the frontend production build
  and all 115 tests pass. Both CI workflows for the preceding worker commit
  completed successfully. The new contract is generated from the server schema.


- Individual comparison text-file loading: private READY rows resolve frozen
  base/head paths and provider binding, without accepting browser paths/refs.
  Exact length, Git-framed blob ID and SHA-256 are verified before bounded raw
  alignment. Empty files and missing sides stay distinct; historical closed
  proposals retain their published pair. Rechecks after connection, each fetch,
  CPU and optional later rendering reject changed scope. Two loads and a fixed
  runtime 60-second deadline bound work; cancellation retains the CPU permit
  until it exits. Eight application and two PostgreSQL scenarios cover these
  boundaries, corruption, revocation, backoff and recovery. No source is persisted
  or serialized implicitly. Runtime configuration shares the frozen provider
  capability. File HTTP/windowing, body caching, readable/asset views, UI and
  review/merge workflows remain required.
  Full backend workspace tests and Clippy pass after this stage. Both CI
  workflows for the preceding HTTP contract commit also completed successfully.


- Bounded raw comparison windows: authenticated file routes use only private
  published file identities and verified pinned sources, rechecking access after
  off-executor materialization. Windows carry both digests, snapshot/content/file
  identities and full coordinates; 1–64KiB text and 128 segments bound responses.
  UTF-8-safe continuation keeps huge lines, empty files and coarse replacements
  fully reconstructable. A final common suffix can no longer overflow the
  alignment chunk cap. Six engine scenarios and four PostgreSQL HTTP scenarios
  cover exact reconstruction, invalid/ambiguous cursors, later head/closure,
  source corruption/revocation, pending/non-text states and provider backoff.
  OpenAPI and the typed Angular client expose the raw contract, with a frozen
  cursor/actor replay test. Source HTML remains JSON text, never rendered markup.
  Body caching, readable/media views, UI, anchors and remaining review/merge
  lifecycle are still required.
  Full backend workspace tests and Clippy pass, as do the frontend production
  build and all 116 tests. The editor fixture has no drift. The restarted API
  reports ready and the new route requires authentication. Both CI workflows
  for the preceding file-reader commit completed successfully.


- Private comparison payload caching: verified immutable source/alignment pairs
  share memory across windows, with 32 entries, a 32MiB accounted allocation
  budget, absolute five-minute eligibility and LRU eviction. Current private
  READY scope is resolved before every hit and rechecked afterward; frozen
  evidence includes actor/org/proposal/connection/snapshot/refs/file metadata.
  Later heads and final states reuse historical bytes with fresh current context.
  Failed final authorization cannot admit source. Expired payloads are reclaimed
  lazily; oversized payloads bypass retention, and process restart refetches the
  same pinned pair. Cold capacity now covers final authorization/admission too.
  Nine application scenarios cover isolation, races, expiry, failure admission
  and entry/byte pressure. HTTP multi-window reconstruction fetches once; a new
  warm-read scenario checks provider independence and revocation/reconfiguration.
  PostgreSQL coverage includes cold-reader restart. No source is persisted or
  exposed outside current scope. Readable/media views, UI, anchors and remaining
  review/merge lifecycle plus operational hardening remain required.
  Full backend workspace tests and Clippy pass, including all 57 application
  scenarios and eleven comparison HTTP scenarios. OpenAPI and editor fixtures
  regenerate without drift. Both CI workflows for the preceding raw-window
  commit completed successfully.


- Bounded readable projection: existing lossless editor blocks and inert GFM
  events now produce contiguous units covering every source byte, including
  metadata, protected definitions/HTML and whitespace. Raw and render identities
  stay distinct; only unique document-local heading markers are trusted. Render
  identity retains formatting, destinations, GFM metadata and nested code info.
  Event/expanded-text/depth budgets precede model construction; the existing
  reference expansion probe now also prevents read-side parser exhaustion from
  masquerading as literal reference text. Sorted range/occurrence lookup avoids
  block-by-heading quadratic scans. Seven engine scenarios cover these source,
  identity, safety and work-limit boundaries. Projection models grant no scope
  or approval and cannot be serialized as whole response DTOs. Pairing, bounded
  readable HTTP/UI, media, review anchors and remaining lifecycle remain work.
  Full backend workspace tests and Clippy pass, including all 66 engine tests.
  OpenAPI and editor fixtures regenerate without drift. Both CI workflows for
  the preceding private-cache commit completed successfully.


- Before/after readable pairing: unique stable headings, exact source and render
  identity precede explicitly labelled section-position inference. Structural
  metadata/trailing units and unpaired duplicates retain complete source coverage.
  Missing files and failed projections stay distinct; unavailable sides require
  raw fallback, while identical complete sources never invent duplicate changes.
  Raw/render/absolute position flags remain separate. Versioned row identities
  bind both full source hashes and unit ranges, with outer file/snapshot/scope
  still required before any review use. Indexed grouping avoids a pair matrix.
  Eight scenarios include every short repeated binary block sequence, verifying
  deterministic one-to-one coverage and visible reorderings alongside identity,
  movement, cardinality and failure cases. Whole pairs are not response DTOs.
  Bounded authenticated readable serving/UI, media, review anchors and remaining
  review/merge lifecycle are still required.
  Full backend workspace tests and Clippy pass, including all 74 engine tests.
  Both CI workflows for the preceding readable-projection commit completed
  successfully.
  OpenAPI and editor fixtures regenerate without drift.


- Bounded readable DTOs: metadata pages expose 1–100 rows with explicit ready,
  absent and raw-required side states; per-row and full-page encoded limits
  prevent unbounded metadata responses. Event windows retain decoded UTF-8/UTF-16
  coordinates, original source ranges, raw/render hashes, code info and initial/
  final inert container stacks. Text/event/JSON budgets preserve huge code lines
  and control-character text without truncation. A counting serializer rejects
  oversized encoded output without allocating a giant JSON string. Protected
  source and oversized indivisible attributes require explicit raw fallback.
  Seven scenarios reconstruct event streams and stack context, complete row pages,
  Unicode/entities, code-language continuation and adversarial bounds/errors.
  Ordered-list restart context preserves current/next and nested item numbers.
  DTO builders perform no I/O or authorization. Application/HTTP integration,
  client/UI, media, review anchors and remaining lifecycle remain required.
  Full backend workspace tests pass, including all 81 engine tests. Clippy and
  formatting pass; OpenAPI and editor fixtures regenerate without drift. The
  previously network-blocked pairing commit was pushed successfully and both
  CI workflows completed successfully.


- Authenticated readable HTTP contract: actor-bound snapshot/file routes expose
  paged unit metadata and decoded event windows, with explicit READY/ABSENT/
  RAW_REQUIRED outcomes. Exact source hashes are checked against the verified
  file pair; scope is rechecked after bounded CPU before any response body.
  Two-slot processing, a 30-second deadline and permit ownership through
  cancellation bound work. Parser/attribute/protected-source fallback cannot
  masquerade as no change. Current private access and provider binding remain
  authoritative for warmed data and historical closure. OpenAPI and the typed
  Angular client capture exact side/cursor/snapshot intent without auto retries.
  Seven HTTP scenarios cover reconstruction, paging, absence/fallback, query/
  scope changes, post-CPU revocation, saturation/backoff and corrupted evidence;
  pending/media tests also cover both routes. Two client scenarios cover captured
  paging and manual replay. AST work is recomputed from bounded cached text;
  AST caching/performance, browser UI, media, review anchors and remaining
  review/merge lifecycle still require implementation/acceptance.
  Workspace regressions pass; the final error-typing refinement passes all 18
  comparison HTTP tests and workspace Clippy. Formatting, frontend production
  build and all 118 frontend tests pass. OpenAPI regenerates consistently and
  the editor fixture has no drift. The restarted API reports ready and both new
  routes require authentication. Both CI workflows for the preceding bounded
  readable-window commit completed successfully.


- Angular comparison lifecycle controller: component-owned intent freezes actor,
  proposal, request UUID and selection. Accepted snapshots are observed/paged
  without new preparation. Response evidence, immutable summary/known file
  descriptors and terminal outcomes are validated before installation. Explicit
  navigation cancels stale generations; polling/timeouts stop at bounded limits
  without false failure. Same-actor authentication can resume accepted observation,
  unaccepted intent requires manual replay, and identity change/logout purges
  private metadata and selection. Context conflicts never silently retarget.
  No-change is distinct from approval. Sixteen lifecycle scenarios cover capture,
  malformed/mixed receipts, paging/navigation, expiry/retry, scope changes and
  reactive getters. Production build and all 134 frontend tests pass. Both CI
  workflows for the preceding readable HTTP commit completed successfully.
  Route/page, body viewers, browser acceptance, media and the remaining review/
  merge lifecycle remain required; the full goal remains unfinished.

- Raw diff OpenAPI contract correction: use the distinct `RawDiffKind` schema
  for equal/delete/insert operations instead of colliding with the inventory's
  document/asset/catalog/other `Kind`. Wire JSON is unchanged. Regenerated the
  checked-in OpenAPI and Angular client. Both API contract tests pass, including
  a regression assertion for the two distinct enum references.

- Routed comparison and raw source viewer: `/changes/:id/diff` restores the
  captured preparation intent or immutable snapshot/page from its URL; proposal
  detail links to it. Latest preparation removes stale snapshot/page selection.
  Hebrew RTL desktop columns place before on the right; narrow layouts carry
  real DOM side labels. Branch and pinned source/target refs, findings, historical
  context, no-change state and file evidence remain visible. Source is escaped
  text with explicit addition/removal markers and optional equal context.
  One 32 KiB raw window lives in page memory; history keeps coordinates only.
  Actor/snapshot/content/file evidence, hashes, UTF-8 ranges, chunk progression,
  response budgets and cross-window continuity are checked before display.
  Scope loss cancels requests and destroys source; retries keep the same cursor.
  Eight raw-window and seven routed-page tests cover navigation, retry, Unicode,
  malformed evidence, empty versus absent files, session changes and inert HTML.
  Production build and all 149 frontend tests pass. Browser discovery returned
  no connected browsers, so visual/keyboard acceptance remains unverified.
  Readable UI, inline word changes, media, selection anchors and the full review/
  merge workflow remain required; this stage does not complete the full goal.

- Readable comparison UI: the default file view pages paired unit metadata and
  loads only the selected before/after units through independent bounded event
  controllers. The existing inert GFM renderer preserves headings, formatting,
  links, tables, lists, code and pinned image context. Continuation reopens known
  container events with ordered-list item numbers, closes the page's remaining
  stack, retains code language metadata and never injects Markdown as HTML.
  Whole-side and per-unit raw fallbacks remain explicit, including an oversized
  atomic event encountered on a later page. Missing sides are distinct from
  empty or unavailable source. Mode/file/actor changes cancel private requests
  and destroy body windows; source is never persisted in browser storage.
  Validators bind metadata/events to captured actor scope, file/snapshot hashes,
  immutable unit summaries, exact cursors, container/list continuity and budgets.
  Added a deterministic Rust-generated readable contract with Hebrew, nested
  ordered lists, long code, tables and protected HTML; CI checks fixture drift.
  Twenty-one new scenarios bring the frontend suite to 170 passing tests; the
  production build, all 81 document-engine tests and engine Clippy pass, and
  readable fixture regeneration matches. Both CI workflows for the preceding
  raw viewer stage completed successfully. Browser visual/keyboard acceptance,
  inline word highlights, dedicated changed-media view, selection anchors and
  the remaining review/merge/export/import work remain open.

- Readable inline word emphasis: complete paired units now get exact visible-word
  comparison with no Unicode/case normalization. Structural block boundaries
  prevent matching across incompatible GFM layouts; formatted leaves retain their
  original block/UTF-16 coordinates. A bounded LCS checks uniqueness of matched
  token identities, suppresses ambiguous repeated-word alignments and caps text,
  tokens, matrix cells and output ranges. Grapheme boundaries preserve niqqud,
  astral/ZWJ emoji and decomposed text. Shared whitespace is not falsely marked
  when punctuation changes. Partial windows keep unit-level change markers.
  The inert renderer overlays semantic ins/del elements with search marks while
  preserving visible text. Readable units use explicit +/- labels and red/green
  context without relying on color alone. Reactive captured-scope ownership also
  gates body/word display when actor, file or selected unit changes.
  Seven algorithm tests include exhaustive optimal alignments for short repeated
  sequences; renderer and view tests cover formatting/search overlap, real Rust
  fixtures, partial-window suppression and identity/selection clearing. Production
  build and all 180 frontend tests pass. Both CI workflows for the preceding
  readable-view stage completed successfully. Changed-media viewing, restorable
  selection anchors, browser acceptance and the full review/merge/bundle lifecycle
  remain open; this is not a completion claim for the full specification.

- Comparison image API: a captured-actor request selects a published private
  file/snapshot and before/after side, never a browser path or Git ref. The shared
  file-reader capacity/deadline covers bounded binary fetch, exact length/Git OID
  and optional SHA checks, content-addressed path/MIME validation, metadata-free
  PNG decoding and display hashing off the executor. The CPU closure owns its
  permit through cancellation. Current authorization is rechecked before/after
  fetch, after decode and immediately before HTTP publication. Missing sides,
  unsupported/executable files, excessive sizes, malformed evidence and provider
  backoff stay explicit; no originals or decoded-image cache are returned.
  A bounded base64url ImageEvidence header binds the PNG body to actor, frozen
  source refs/file/side, original hash/OID/bytes and derivative hash/dimensions.
  OpenAPI and typed blob client are regenerated consistently. Five image reader
  scenarios and four HTTP scenarios cover real fixture pixels, private trailer
  stripping, forged source, absence, revocation and pinned closed/history reads.
  The full backend suite and workspace Clippy pass; local API restarts ready,
  and an anonymous request to the new route returns 401.

- Dedicated image comparison UI: asset entries now render explicit before/right
  and after/left previews with missing-side states instead of a fake text diff.
  Other opaque file kinds report unavailable preview. Each side captures its
  actor/file/snapshot, validates the image evidence header and source descriptor,
  checks PNG MIME/body bounds and hashes the derivative before creating a private
  object URL. The image becomes visible only after browser dimensions match the
  verified response. Retry preserves the exact selection; error/replace/logout/
  navigation/destruction revoke URLs and cancel requests. Late async hashes
  cannot install previews under a new scope. Local invalid/oversized media does
  not trigger a provider read; error JSON returned as Blob is handled explicitly.
  Seven state, three rendered-view, one routed-page and one typed-client scenario
  bring the full frontend suite to 192 passing tests. Production build passes,
  frontend serves locally on 4200, and generated API output has no drift.
  Browser connection remains unavailable, so physical image/layout/keyboard
  acceptance remains open. Restorable selection anchors and all remaining
  review/merge/bundle/production requirements are still part of the active goal.

- Immutable comparison locators: snapshot inventory reads can select the bounded
  page containing a file ID, excluding an ordinal query. PostgreSQL resolves its
  immutable ordinal under current actor-private scope, then the normal page read
  and final status read recheck access/context. This needs no provider IO and
  survives process restart and later proposal closure/head changes. Readable row
  queries similarly locate a paired-unit ID within the verified exact source
  pair on the bounded CPU path, excluding an offset query. Unknown IDs return
  explicit 404; IDs cannot follow a different file or full source pair. Syntax,
  duplicate and mixed-query rejection remain strict. Engine, PostgreSQL and HTTP
  tests cover later pages, private scope, stale identities and no-Git location.
  The full backend suite and workspace Clippy pass. OpenAPI and the typed client
  are regenerated; existing readable fixtures retain their exact contract.

- Restorable comparison selection: Angular URLs now retain snapshot, immutable
  file ID, readable paired-unit ID and readable/raw view. Restoration locates
  exact identities directly instead of scanning or trusting stale page offsets.
  File/unit receipt validation requires the requested identity in the bounded
  returned page; refresh/retry preserves actor and snapshot without preparation.
  Browser back can reuse validated files already in memory; selecting another
  file cancels an obsolete in-flight page. Raw-view URLs select the whole file,
  while unit locators require readable view; applying a unit locator to a media
  file is explicit and cannot fetch an unrelated body. Unknown IDs can return to
  the same snapshot's file/unit list without silently selecting latest refs.
  Latest preparation clears old file/unit locators, mode switches cancel bodies,
  and private selection links contain no actor, source text or approval authority.
  Links target the current actor's private comparison, not a shared formal review.
  Twelve new frontend scenarios bring the suite to 204 passing tests. Production
  build passes, and OpenAPI/readable fixture checks show no drift. Both CI
  workflows for the preceding image-view stage passed. Raw chunk/window position
  links, persisted formal review anchors/decisions, browser/human acceptance and
  all remaining merge/bundle/production requirements remain open.

- Review preparation core (stage 3): a non-deserializable server context and the
  verified complete comparison produce a bounded immutable review plan. The
  candidate validates the full after-book, preserves every changed file, requires
  current saved head/target/version and an editable authorized actor, and rejects
  empty/historical/final/moved-branch or stale-target input. Active qualified
  non-author reviewer assignments must contain distinct product/development
  reviewers, plus design when UI impact is yes. Undecided UI and an outdated base
  remain explicit; no approval outcome is inferred from preparation.
  Canonical fingerprints bind frozen source refs, repository connection, proposal
  metadata, UI impact/reason, policy/configuration versions, assignments and the
  complete change/media manifest. Actor/list ordering/state-only version changes
  do not manufacture a different shared content identity. Metadata fingerprinting
  streams through a bounded buffer rather than encoding whole source bodies.
  Referenced and newly added media become deduplicated verification requirements;
  the pure plan neither fetches binaries nor fabricates decoder receipts. Source
  warnings (including missing alt) and full truncated-diagnostic counts remain
  visible for later review/merge gates. Twelve new cases use the real verified
  comparison loader; all 74 application tests and workspace Clippy pass. Both CI
  workflows for the preceding selection-link stage completed successfully.
  Durable submission/snapshot publication, media verification receipts, PR/check
  orchestration, review anchors/decisions/discussions and review UI still require
  implementation; this pure plan is not an exposed or completed review workflow.

- Durable review configuration: migration 009 stores immutable UI-impact and
  reviewer-assignment revisions plus actor-bound replay receipts. Authenticated
  GET/PUT routes and a generated typed Angular client enforce captured actor,
  CSRF, current organization/edit authority, bounded input, saved head and both
  proposal/configuration versions. Selected reviewers must be active, qualified
  for the domain, and independent of the author. Partial configuration remains
  editable without claiming submission readiness. Reads recompute eligibility;
  membership revocation preserves historical selections. Changed settings advance
  both versions, demote APPROVED to IN_REVIEW, and atomically append audit plus a
  review-invalidation outbox event. Canonical no-ops preserve state and versions.
  Exact retries retain their original receipt after later edits, closure or an
  unresolved Git mutation while still requiring current edit authority.
  A concurrent-write regression identified and fixed a PostgreSQL statement-
  snapshot race: revision lookup now follows the proposal lock. Database guards
  reject history rewrites, late appends, unselected/incomplete revisions and
  manifests bypassed by forcing deferred checks early. Nine new PostgreSQL/HTTP
  scenarios and two application cases pass; the full backend suite and workspace
  Clippy pass. OpenAPI/client regeneration, production frontend build and all 206
  frontend tests pass, including two new typed-client cases. Both CI workflows
  for the previous review-plan commit passed. See `review-configuration.md` for
  the exact contract. Picker/review UI, formal submission/snapshots, decisions,
  discussions and PR/check/merge orchestration remain open.

- Reviewer selection source: an actor-bound read-only endpoint lists current
  eligible reviewers by domain, excluding authors, inactive/unqualified users
  and other organizations. UUID keyset pagination returns at most 50 bounded
  names with explicit continuation, and reauthorizes every page. It makes no Git
  calls and does not change configuration or confer edit authority. Two new
  PostgreSQL/HTTP cases cover filters, strict queries, scope/revocation and all
  55 users across multiple pages; all 11 configuration API scenarios pass.
  Workspace Clippy, generated OpenAPI/client, production build and all 207
  frontend tests pass. A typed-client continuation case preserves actor/domain
  identity. This supplies the future picker; the review UI and formal workflow
  remain open.

- Review configuration UI: `/changes/:id/review` now displays proposal/branch/
  target/state/version context and edits UI impact/reason plus paged named
  reviewer assignments and required flags. It retains selected/revoked reviewers,
  preserves names across pages, explains independence/design requirements and
  permits partial metadata without claiming formal submission or approval.
  Configuration responses include the header context without Git IO. Private
  page state validates response identities, versions, assignments, statuses and
  continuation cursors. An uncertain write retains its exact key/actor/body for
  manual retry, freezes edits and never automatically resubmits. Definite
  validation failures allow correction; stale versions require explicit reload.
  Reason canonicalization matches Rust Unicode White_Space, including U+0085
  and excluding U+FEFF. Router/logout/reload/unload guards protect local work;
  refresh hides it, and actor changes, logout or denied access purge it.
  Ten state, five rendered/router and eighteen evidence cases bring the frontend
  suite to 240 passing tests; production build, the API suite and workspace
  Clippy pass. OpenAPI/client are regenerated. Both CI workflows for the prior
  reviewer-list commit passed. Browser discovery again returned no connections;
  physical keyboard/layout/RTL/narrow-screen acceptance remains open, as do
  formal snapshots, submission/media workers, decisions/discussions, PR/checks,
  merge/base/rollback, bundles and the remaining operational/pilot requirements.

- Authoritative review submission context: a non-deserializable expectation and
  server scope pair current review metadata with the verified comparison's
  repository identity. PostgreSQL enforces active scoped actor/edit authority,
  editable state, saved head and all three versions, a valid active owner and no
  unresolved Git mutation. Immutable revision lookup follows the proposal lock;
  reviewer memberships are fresh, revoked members stay absent/inactive, and
  qualification ordering/duplicates have stable semantic equality. Seven new
  database scenarios cover authority, tenant scope, version gates, revocation,
  owner eligibility, repository configuration and observed concurrent lock waits.
  All 76 application and seven domain tests plus workspace Clippy pass. Both CI
  workflows for the prior configuration-page commit passed. This scope reader
  makes no provider call and publishes no snapshot. See `reviews.md`; verified
  preparation/media orchestration and durable submission/publication remain open.

- Verified review preparation orchestration: `ReviewPreparer` pairs the current
  server scope with a full saved-head/target comparison, checks edit/reviewer
  authority after loading and CPU planning, and observes exact branch/main refs
  through the frozen provider binding before returning a private candidate.
  Authorization changes take precedence over private validation diagnostics.
  A later recheck observes authority/refs without reloading source. Two preparation
  slots and a deadline bound work; CPU closures retain capacity through caller
  cancellation. There is no automatic retry or provider write. Eight new cases
  use the real comparison loader and include malformed scope, revocation at each
  boundary, changed refs, diagnostic suppression, later rechecks, cancellation
  and a virtual-clock deadline/recovery check. All 84 application tests, the full
  backend suite and workspace Clippy pass. Both CI workflows for the preceding
  scope-resolver commit passed. No API/client change is exposed by this stage.
  Media receipts, submission leases, durable shared snapshots and their API/UI,
  PR/checks, decisions/discussions and the remaining full specification are open.

- Review media evidence: candidates can now be consumed into a sealed
  `VerifiedReview` after every required image has passed saved-head byte-count/
  Git-object/content-address checks and bounded MIME/pixel decoding. Referenced
  images are deduplicated and newly added unreferenced images are included.
  Original/derivative bytes are discarded; immutable receipts retain only hashes,
  dimensions and source metadata. The logical identity binds the plan and media
  validation version; a separate digest binds canonical evidence and display
  hashes. Current scope is checked before/after IO and CPU, and final exact refs
  are observed before any complete evidence is returned. Errors identify the
  failing asset and preserve provider backoff without automatic retry. Missing-alt
  and other content findings remain visible for later gates. Twelve new real-
  loader scenarios cover valid/empty scans, reuse, unreferenced media, forged or
  oversized bytes, content hashes, MIME/decoder failure, revoked authority, moved
  refs, policy/actor identity, timeout cleanup and missing-alt preservation. All
  96 application tests and workspace Clippy pass. Both CI workflows for the prior
  preparation commit passed. Durable submission leases, checkpointing, shared
  snapshots, their HTTP/UI flow and all remaining review/merge/bundle/production
  requirements remain open. This step exposes no new API route or approval.

- Durable review submission journal: migration 010 freezes small normalized
  expectations and the server authority digest, with scoped actor-bound request
  replay, private status and atomic operation/job/request/audit reservation.
  New work uses the existing per-proposal mutation gate; identical retries can
  observe their original ticket after later changes while requiring current edit
  authority. Worker leases are bound to attempt as well as owner and expiry,
  including same-worker reclaim. A leased resolver validates that fence before
  excluding exactly its own pending operation, then rechecks current metadata
  and the lease clock. Authority/heartbeat fail if roles change or a blocked read
  outlives its lease. Scoped preparers retain the shared IO/CPU limit. Read-only
  backoff/exhaustion and definite failure are journaled; provider-started/unknown
  work is never retried or unlocked by these paths. Database guards freeze all
  identity rows and prevent orphan jobs; generic helpers cannot claim/complete
  submissions. Eleven new PostgreSQL scenarios bring the context/journal suite
  to 18 passing cases; two new fingerprint cases bring application tests to 98.
  The full backend suite and workspace Clippy pass. Both CI workflows for the
  preceding media-evidence commit passed. No submission endpoint, published
  snapshot or approval is claimed yet. Snapshot publication, worker/API/UI and
  PR/check integration plus all remaining full-spec requirements remain open.

- Atomic shared review snapshots: migration 011 and `PgReviewSnapshots` publish
  sealed evidence only after a lease-bound authority/ref recheck and a second
  transactional authority check. One commit includes the complete immutable
  file/media manifests, frozen review metadata, submission receipt, active
  proposal pointer/version/state, job/operation success, audit and PR-sync outbox
  event. A one-time seal and deferred guards reject incomplete/orphan evidence;
  a final attempt/owner/clock fence rolls back late publication. Only the active
  identical fingerprint can be reused, preserving approved state/version. Head,
  target, base, review metadata and provider-binding changes invalidate that
  pointer; restoring old inputs never reactivates historical approval. Policy
  versions cannot regress. Lost replies and concurrent callers replay the same
  result with current edit authority and no repeated Git work. Organization
  Readers can page through shared metadata/files with current membership checks,
  independently of provider availability; job status remains actor-private.
  Nine new PostgreSQL tests cover real verified text/image pipelines, exact
  manifests and receipts, pagination, active reuse/history, private/shared
  access and revocation, stale scope/refs/attempts, concurrent replay, injected
  outbox failure, expiry after writes, incomplete sealing, null media receipts,
  missing publication outcomes, immutable rows and truncation. The review suite
  now passes 27 tests; the full backend suite and workspace Clippy pass. Both
  workflows for the preceding submission-journal commit passed. Public HTTP and
  worker wiring, shared diff/image bodies, PR/check processing, decisions,
  discussions, approvals, merge, bundles and production acceptance remain open.

- Durable review snapshot worker: `ReviewWorker` now drives pending submission
  jobs through lease-bound comparison preparation, complete media verification
  and atomic shared publication. Preparation uses authority-checked 60-second
  leases renewed every 15 seconds; heartbeat waits and publication share the
  processing deadline. Publication has no competing heartbeat and retains its
  own final fence. Safe transient reads use journaled bounded backoff and provider
  retry-after; invalid content/media, denied/stale authority and moved refs fail
  without persisting private diagnostics. Database publication uncertainty leaves
  work for receipt observation or expiry/reclaim. Replaced attempts cannot fail
  newer owners. Shutdown cancels pending reads; expired read-only work restarts,
  without repeating Git writes. The API starts one review worker with a default
  900-second timeout (30–900 allowed), sharing the configured frozen provider
  factory and preparation capacity. Eleven new PostgreSQL worker scenarios cover
  actual text/image completion, idle behavior, backoff, retry exhaustion, authority
  revocation before/during reads, stale attempts, shutdown/reclaim, processing
  deadline and publication rollback/recovery. A separate application-state test
  verifies configured-source wiring and a missing-provider failure. The context/
  journal/snapshot/worker suite passes 38 tests; the full backend suite passes
  412 tests with two existing performance measurements excluded. Workspace Clippy
  passes. Both workflows for the preceding snapshot-publication commit passed.
  Public submission/status/snapshot endpoints and their frontend flow, PR/check
  processing, checkpointed scans and all remaining full-spec work remain open.

- Authenticated review submission/shared snapshot HTTP: `POST /proposals/:id/submit`
  queues or exactly replays a small actor-bound intent with CSRF/origin, current
  edit authority, strict input and expected-version checks. Generic job status
  now returns typed, backed snapshot publication to the original actor only and
  rejects ambiguous query parameters. Organization members, including Readers,
  can inspect shared typed snapshot metadata, stable cursor history, complete
  paginated file descriptors and media receipts without Git IO. History remains
  available after closure/provider changes; current-metadata flags do not imply
  fresh refs or approval. Owned read schemas do not construct sealed evidence;
  private source bodies, provider IDs and authority/job payloads stay excluded.
  Migration 012 indexes organization-serialized admission: actor/organization
  unresolved limits are 2/12 and ten-minute new-request limits 10/60. Terminal
  requests still count, exact replay bypasses limits, and rejected admission
  creates no partial work. Eleven new HTTP integration scenarios cover the real
  queued-to-published text/image flow, private/shared scope and revocation,
  strict/malformed/oversized requests, all CAS versions, busy/idempotency/closure,
  immutable history during new publication, unbacked success, concurrent actor
  admission and both organization quotas. All 423 backend tests and workspace
  Clippy pass; frontend build and all 240 tests pass with the updated OpenAPI and
  generated client. Two existing performance measurements remain excluded. Both
  workflows for the preceding worker commit passed. Frontend submission/shared
  review, shared raw/readable/image bodies, PR/checks, decisions/discussions,
  approvals and the remaining full specification are still open.
