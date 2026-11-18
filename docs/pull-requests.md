# Dedicated proposal PR synchronization

## Current stage: runtime provider worker and fenced durable journal

The Git adapter exposes `PullRequestProvider` separately from content operations.
It reads one PR, discovers PRs for a proposal branch across all states, creates a
draft, and updates title/body metadata. A provider observation is not an approval,
merge receipt or proof that a saved review snapshot is current. PostgreSQL admission
now freezes publication outbox events into dedicated PR operations/jobs and stores
fenced write phases, PR identities and observation results. A bounded runtime
worker now performs provider orchestration. Outbox dispatch, required checks and
the user-facing synchronization status remain pending.

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

`PrProviderFactory` resolves the PR capability separately from content providers.
The PostgreSQL GitHub source checks the frozen repository UUID, numeric repository,
installation and main target before selecting a connection. Content and PR
capabilities share the same verified adapter/token cache, keyed by all three
repository/installation identities, with the existing expiry margin and bounded
capacity. Connecting uses the exact settings just checked; it does not reselect
mutable configuration after validation. This is connection binding, not actor or
journal authorization, which the synchronization worker must independently recheck.

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
that a timed-out create did not happen.

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

## Durable publication-event admission

`PgPrSync::reserve` requires the publication's original actor to retain current
edit access. It validates the exact event schema and flags against a successful
submission operation/job, immutable publication result and sealed READY snapshot.
New work also requires that snapshot to remain active with matching current
repository/installation/branch, commits, configuration and policy. It captures
current projection metadata and fresh authority under locks without provider IO.
The authority digest also includes title, proposal number and links, which are
absent from the review-scope digest. Question coverage remains explicitly partial.

One transaction stores the immutable intent and exact title/body/hash, creates a
`PR_SYNC` operation and `SYNC_REVIEW_PR` job, appends audit evidence and marks the
event delivered. `outbox.sent_at` means journal delivery, never provider success.
Repeated/concurrent delivery returns the original bytes and identifiers even if
the site origin or proposal metadata later changes; current actor access is still
required. A unique source submission prevents duplicate publication receipts from
creating independent operations. Failure rolls back delivery and all journal rows.

One unresolved PR operation per proposal is separate from the content-mutation
slot, so document saves and new review submissions remain possible. New admissions
are serialized per organization and limited to two active/ten recent per actor and
twelve active/sixty recent per organization, with a ten-minute recent window.
Exact replay precedes quotas. Database guards freeze intents, source events, jobs
and operation scope, require atomic admission evidence, and reject generic success.
Generic job leasing/completion excludes PR jobs. No runtime dispatcher invokes
this admission method yet.

## Fenced writes and observation results

`PrSyncLease` is server-owned and binds worker UUID, attempt, job and the restored
immutable intent. Claiming an expired lease increments the attempt even when the
worker UUID is reused. Current edit access and the frozen repository binding are
required for observation. An untouched intent additionally requires its original
active snapshot and authority digest before heartbeats or writes. Once a write
starts, read-only reconciliation may continue after metadata advances or the
proposal closes; it cannot authorize another write.

`start_write` independently resolves the durable PR binding, checks current local
authority and applies the reconciliation decision to the caller's complete bounded
discovery. Before returning, it atomically appends an immutable CREATE/UPDATE
record with its original fence and audits the boundary. Updates require the exact
persisted provider identity; an absent bound PR never authorizes a create. The
caller must check live provider refs around IO: database fences cannot atomically
assert external refs. No database transaction is held during provider calls.

A started write is never repeated by this journal. Reclaim/defer changes the
operation to UNKNOWN and preserves the write record. Eight attempts bound automatic
work. Exhaustion before any write ends as a definite failure; exhaustion after a
write ends automatic observation while retaining UNKNOWN and its admission slot.
An exact provider observation can reconcile an uncertain outcome without another
write. Closed/ready/foreign candidates, changed metadata or missing identities
cannot be treated as exact receipts. Explicit recovery for provider rejections,
closed provider PRs, revoked original actors and exhausted observations remains
part of the pending recovery workflow; started writes currently retain uncertainty
even when the adapter reports a definite rejection.

Completion requires current edit access, the original repository/installation,
exact owned title/body and an open draft PR with the expected identity. A current
source/ref pair records SYNCED. A superseded local context or observed ref pair
records SUPERSEDED while retaining the provider identity for the next projection;
it grants no approval and changes no content or proposal state. The immutable
binding, result, job/operation outcome and audit commit together. Late fences and
audit failures roll back the entire receipt, including a newly discovered binding.
Database guards prevent unbacked writes, forged generic success, identity changes
and incomplete terminal outcomes.

## Runtime provider orchestration

`PrSyncWorker` rechecks current authority and connects both provider capabilities
to the frozen binding. Discovery requests at most two candidates: two prove a
conflict, while zero/one with no next page proves exhaustion. Foreign, conflicting
or incomplete observations cannot authorize a write. A listed candidate whose
direct GET returns 404 stays observation work rather than becoming proof of
absence. Known PRs are read directly even when omitted from a list response.

Before writing, live branch/main must match the frozen pair. After recording the
journal boundary, the worker checks local authority and refs again, then sends one
draft-create or metadata-update. Positive replies are read back directly and
checked against bounded discovery; exact owned metadata and current provider refs
must agree before final database publication. Unknown/reclaimed attempts never
mutate again, including when the list remains empty. Ref disagreement defers
observation; an exact older projection can resolve as SUPERSEDED without losing
its durable PR identity.

One worker runs in the API process. `PR_SYNC_WORKER_TIMEOUT_SECONDS` defaults to
120, with a 30–900 second attempt deadline. Leases last 60 seconds and renew every
15 seconds during long provider calls and between short calls. Heartbeats never
run alongside the worker's own journal transactions. Graceful shutdown cancels
in-flight work; a persisted write phase forces read-only recovery after restart.
Provider rate-limit backoff is preserved. Explicit rejection/operator recovery,
outbox dispatch and authenticated synchronization status remain pending.

## Remaining full-spec work

- Dispatch publication outbox events, including stale/poison-event handling and
  explicit recovery for rejected, exhausted, closed-provider and
  revoked-original-actor cases.
- Connect the bounded metadata projection to persisted decisions and complete open
  product questions when those workflows exist; capture them under journal locks.
- Expose authenticated synchronization status and explicit recovery without
  changing the saved snapshot or reporting provider failures as proposal closure.
- Enforce the required head-bound SpecSpace check and native branch protection;
  integrate current approvals, policy, target freshness and unresolved blockers.
- Add explicit readiness/close/reopen/merge capabilities with their own authority
  and journal evidence; preserve the full review and publication requirements.
