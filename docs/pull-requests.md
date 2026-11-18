# Dedicated proposal PR synchronization

## Current stage: provider capability

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

## Remaining full-spec work

- Persist one scoped PR identity per proposal, immutable sync intents and fenced
  mutation/reconciliation phases; consume review publication outbox events.
- Compose bounded title/body with website/snapshot links, description, decisions,
  open product questions and development links, using stable server-owned markers.
- Expose authenticated synchronization status and explicit recovery without
  changing the saved snapshot or reporting provider failures as proposal closure.
- Enforce the required head-bound SpecSpace check and native branch protection;
  integrate current approvals, policy, target freshness and unresolved blockers.
- Add explicit readiness/close/reopen/merge capabilities with their own authority
  and journal evidence; preserve the full review and publication requirements.
