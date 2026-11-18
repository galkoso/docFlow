# Dedicated proposal PR synchronization

## Current stage: automatic outbox dispatch and fenced provider work

The Git adapter exposes `PullRequestProvider` separately from content operations.
It reads one PR, discovers PRs for a proposal branch across all states, creates a
draft, and updates title/body metadata. A provider observation is not an approval,
merge receipt or proof that a saved review snapshot is current. PostgreSQL admission
now freezes publication outbox events into dedicated PR operations/jobs and stores
fenced write phases, PR identities and observation results. A bounded runtime
worker performs provider orchestration and a transactional dispatcher delivers
publication events automatically. Required checks, explicit operator recovery and
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
Generic job leasing/completion excludes PR jobs. Automatic and explicit admission
share the same transaction-local implementation and durable transport record.

## Automatic publication-event dispatch

Migration 015 adds a dedicated transport record and canonical source validation.
The dispatcher derives the original actor from the actual succeeded submission,
publication result, sealed snapshot and exact event flags; it never trusts an
event-owned actor or approval claim. Payload parsing is bounded to 8KiB. A valid
current event is admitted under current edit authority. The frozen intent/job,
transport ENQUEUED result, audit and delivery marker commit together. A copied
backed publication can acknowledge the existing operation without creating work
or granting current write authority to a revoked actor.

Superseded, closed or redirected snapshots become audited SKIPPED transport
outcomes. Malformed/unbacked events or invalid projections become REJECTED, leaving
saved snapshots and proposal state intact. Busy work retries after 30 seconds,
admission quota after 60, and missing actor access or review setup after 300.
WAITING events remain unsent and cannot hold later eligible publications at the
front of the queue. Current access is checked again when a retry becomes due.
Unchanged retry reasons do not duplicate audit entries. Database unavailability
rolls back the attempt rather than inventing an outcome.

Proposal-before-event locking, transaction advisory selection and a savepoint
around admission make concurrent dispatch/manual replay safe. No provider IO
occurs here. Database guards freeze captured envelopes and terminal transport
outcomes and require matching audit/delivery evidence. Migration upgrades backfill
already delivered intents without changing original jobs, writes or uncertainty.

One dispatcher runs alongside the PR worker when `APP_PUBLIC_URL` is configured.
It requires a validated HTTPS root origin, with loopback HTTP allowed only in
explicit development mode. Without a configured origin dispatch stays disabled;
the API startup log reports `pr_dispatchers=0`. A transport outcome describes
event delivery, not provider synchronization, review approval or publication.

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
Provider rate-limit backoff is preserved. Authenticated synchronization status
and bounded operator recovery are available as described below.

## Stored status and explicit operator recovery

`GET /api/v1/proposals/{proposal}/review/snapshots/{snapshot}/pr-sync` is a
current-member, actor-bound read of stored evidence. It performs no provider IO.
Transport delivery, automatic work, unresolved writes and the last saved provider
observation are separate. Observation receipt time is distinct from this GET's
read time, and an older/different snapshot cannot claim current synchronization.
Neither a PR link nor an observed match grants approval or merge readiness.

Migration 017 adds append-only recovery receipts and guarded bounded job resumes.
An active organization admin can explicitly POST to the status URL plus `/recover`
with the session, `X-Expected-Actor`, allowed `Origin`, CSRF header and JSON:

```json
{
  "request_id": "e807a370-7df8-4b34-977c-a0f6f6bd8b3c",
  "expected_fingerprint": "<the selected snapshot's 64-character lowercase fingerprint>"
}
```

The POST resumes the latest stopped intent for that exact snapshot. It never
creates a replacement PR intent or resets attempt numbers. A queued receipt is
202 with a Location pointing back to stored status, not a provider result. Replay
requires the same admin, proposal, snapshot and fingerprint under the same request
ID and checks current admin access again. It returns the original admission even
after completion; it cannot restart the job again. Different requests cannot
restart live/successful/cancelled work. New admissions are limited to two per
admin and twelve per organization in ten minutes; replay does not consume quota.

- `RETRY_UNSTARTED`: no write marker exists and the operation failed. Original
  editor authority, exact current scope/digest and repository binding must still
  match. A new eight-attempt window permits the existing worker to retry its
  original immutable intent, with all normal checks before any provider write.
- `OBSERVE_STARTED`: the failed job has a durable write marker and an UNKNOWN
  operation. Eight more attempts may only observe. The marker, operation identity,
  immutable metadata and any bound PR remain intact. Empty listings and uncertain
  responses never authorize another write. The original editor may still observe;
  otherwise the admin of this recovery window must remain active and an admin on
  every heartbeat, IO boundary and final publication. An older admin grant cannot
  replace a revoked latest window. Read-only delegation can reconcile after proposal
  closure or editor revocation; revoked original authority yields a SUPERSEDED
  receipt, not current synchronization or approval.

The queued job, operation state, added attempt budget and admin audit commit
together. Database guards reject blind resets, incomplete recovery receipts and
changes to recovery history. Repository binding changes stop recovery before IO.
The shared review page shows only a server-provided admin recovery hint; the POST
rechecks authority and context. A lost HTTP reply retains one request ID for
explicit replay while that page scope remains open. Refresh and polling remain
GET-only, and scoped denial purges the shared snapshot/body. Navigating away or
changing identity clears local recovery state; the server keeps its durable
admission and uncertainty.

## Remaining full-spec work

- Add definitive provider-rejection/negative-proof resolution, external closed/
  merged PR synchronization and transport rejections after correction. Bounded
  observation recovery does not declare an uncertain write absent or replace a PR.
- Connect the bounded metadata projection to persisted decisions and complete open
  product questions when those workflows exist; capture them under journal locks.
- Enforce the required head-bound SpecSpace check and native branch protection;
  integrate current approvals, policy, target freshness and unresolved blockers.
- Add explicit readiness/close/reopen/merge capabilities with their own authority
  and journal evidence; preserve the full review and publication requirements.
