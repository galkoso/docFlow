# Implementation and acceptance ledger

Source of scope: `specification.he.md`, sections 1–26, including AC-01–20 and
all edge cases in section 22. The supplied requirements remain authoritative.

## Stage 0 — foundations and feasibility

- [x] Preserve the original specification without changes.
- [ ] Angular strict standalone application and Rust workspace; locked dependencies.
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
