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
