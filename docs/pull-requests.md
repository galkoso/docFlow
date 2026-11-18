# Dedicated proposal PR synchronization

## Current stage: provider capability and immutable intent

The Git adapter exposes `PullRequestProvider` separately from content operations.
It reads one PR, discovers PRs for a proposal branch across all states, creates a
draft, and updates title/body metadata. A provider observation is not an approval,
merge receipt or proof that a saved review snapshot is current. Application
orchestration, persistence, required checks and the user-facing synchronization
status remain pending. Published review snapshots already emit outbox events
requesting that synchronization.

The [GitHub PR REST API](https://docs.github.com/en/rest/pulls/pulls?apiVersion=2026-03-10)
provides the list/get/create/update operations used here. Draft creation uses
explicit head/base/title/body fields; metadata updates send only title/body.
The adapter does not expose retargeting, reopening, readiness or merge through
metadata updates. PR installation-token permissions were already part of the
existing GitHub App connection configuration.

## Authority and identity

PR operations require a numeric repository binding established by the GitHub App
connection's verified repository lookup. An unbound name-only adapter or a scope
for another numeric repository fails before IO. Both head/base repositories must
match that numeric binding and configured repository name. Forks, other branches,
non-main targets, malformed head/base SHAs and unsafe provider URLs are rejected.
Canonical PR URLs are reconstructed from the configured repository and validated
positive PR number. Responses include provider ID/node identity, observed commits,
draft flag and distinct open/closed/merged state.

Metadata input is bounded to a proposal-sized title and 64KiB UTF-8 body. Provider
bodies and models deliberately have no Debug/Serialize/Deserialize implementations;
private proposal context cannot accidentally become logged or browser-owned
workflow authority. The application must compose its own stable proposal/operation
markers and verify them before associating a discovered PR with a proposal.

## Discovery and reconciliation

Discovery uses owner-qualified proposal head and all PR states. It deliberately
omits the base filter: a PR retargeted outside the app must remain visible as an
invalid candidate, rather than disappearing and allowing another PR to be created.
Every returned candidate must match the expected branch, repository and main
base. Duplicate provider IDs/numbers and overfull pages fail closed.

Each page contains at most 100 items; page numbers are bounded to 100. A full last
page reports a scan limit rather than falsely proving exhaustion. Provider Link
URLs are never followed. The application must complete bounded discovery and
resolve conflicting/unmarked candidates before inferring absence.

Creation always requests a draft and disables maintainer edits through that PR
setting. Updates first read and validate an open PR, then write title/body only.
An update receipt must preserve PR number, provider ID/node identity and draft
flag and echo the exact intended metadata. Write responses with unexpected status,
malformed evidence, wrong identity/state/body, redirects, transport loss, cut-off
JSON or server errors remain uncertain. No PR write is automatically retried.
Rate-limit backoff is preserved; explicit provider rejection remains distinct.

These REST metadata writes do not atomically assert head/target SHAs. The worker
must fence its journal/lease, check current local authority and refs, and validate
the observed pair against the selected snapshot before publishing synchronization
success. Unknown creates require reconciliation before any further mutation;
absence from a single eventually consistent list response is not sufficient proof
that a timed-out create did not happen. No existing worker invokes these new
mutations yet.

## Immutable metadata intent

`PrSyncIntent` captures actor/organization/proposal/operation/snapshot identities,
frozen repository/installation/branch, head/target/base, proposal/configuration/
policy versions, snapshot/evidence/authority digests and exact title/body. Its
canonical SHA-256 request hash binds every captured field and metadata byte.
Restoration validates the stored hash and managed ownership frame and reuses the
stored body, so a deployment's newer renderer cannot change an uncertain retry.
This pure construction is not authorization or a database admission receipt. The
journal must resolve current edit authority and the exact active sealed snapshot
before storing it, and recheck those conditions around provider writes/results.

The body contains a snapshot-pinned website link, description, UI-impact decision,
questions, development/tracker/design links and source identities. Root managed
markers bind organization/proposal, operation and snapshot. Untrusted text is
quoted with HTML/Markdown escaping and neutralized mentions; it cannot inject a
managed root marker. Line endings are normalized for readable metadata projection.
Incomplete question coverage is explicitly labelled rather than represented as no
open questions. Invalid links, unsafe site origins and oversized context fail
before any provider call; production site links require HTTPS and only explicit
development configuration permits loopback HTTP.

Pure reconciliation decisions distinguish a new draft, metadata update, exact
synchronization, continued observation, conflict and stale refs. Unknown writes
and already-bound PR absence never authorize another create. Exact metadata and
source observations can reconcile an outcome; mismatches after a write starts
remain read-only. Closed/merged/ready, duplicate, foreign, malformed or unowned
candidates cannot authorize writes. An unbound candidate must match the original
operation marker, and cannot be overwritten when its metadata differs.

## Remaining full-spec work

- Persist one scoped PR identity per proposal, immutable sync intents and fenced
  mutation/reconciliation phases; consume review publication outbox events.
- Connect the bounded metadata projection to persisted decisions and complete open
  product questions when those workflows exist; capture them under journal locks.
- Expose authenticated synchronization status and explicit recovery without
  changing the saved snapshot or reporting provider failures as proposal closure.
- Enforce the required head-bound SpecSpace check and native branch protection;
  integrate current approvals, policy, target freshness and unresolved blockers.
- Add explicit readiness/close/reopen/merge capabilities with their own authority
  and journal evidence; preserve the full review and publication requirements.
