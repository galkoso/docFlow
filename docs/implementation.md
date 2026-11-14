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
- Browser verification attempt: the Browser runtime reported no available browser
  and discovery returned an empty list. Desktop/320px visual verification remains
  open; the Angular development server starts successfully on loopback port 4200.
