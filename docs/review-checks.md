# Required review-policy calculation

The domain evaluator in `specspace_domain::review_check` calculates the
`specspace/review-policy` check from current trusted evidence. It does
not publish provider checks, change proposal state or authorize merge.

`Context` deliberately has no deserialization path. The server must obtain the
active sealed snapshot, exact current branch/main refs, policy, current qualified
memberships, applicable assignments and latest official decisions. Required
assignments must belong to that configuration. Duplicate current decisions,
duplicate memberships/assignments and malformed identities/refs fail evaluation.

Product and development approvals require distinct independent qualified people.
UI changes additionally require design approval; undecided UI impact blocks the
check. Every explicitly required reviewer must approve, even when another pair
already meets the minimum quorum. Old snapshots, changed head/target/policy and
revoked or foreign reviewers cannot supply current approval. A comment is not
an approval, and a current qualified request for changes blocks success.

Base update, content/media validation, mandatory section review coverage, blocking
threads and blocking product questions each carry explicit verification state.
Unknown or unfinished evidence keeps the check pending. A failed gate fails it;
success requires every gate to pass. A passed section-coverage input must cover
all required changes for every counted reviewer, not a browser checkbox.

External required provider checks belong to the separate merge gate. Feeding the
required SpecSpace check back into its own prerequisites would create a circular
dependency. Successful site-policy evaluation alone does not establish provider
mergeability, native branch protection, current PR ownership or merge authority.

Persisted official decisions, section coverage, discussion/product blockers and
the stored and fresh provider-backed readiness readers are implemented. The
existing HTTP report remains stored-only. Fresh observations recheck reader and
reviewer authority after provider IO; they do not themselves publish anything.

The explicit `ReviewCheckProvider` adapter capability supports scoped read/list,
pending-only creation and preflight-checked updates. Repository, App ID, exact
head and fixed check name bind every response. A canonical operation UUID in
`external_id` allows later journal reconciliation; GitHub does not deduplicate
creates by that field. List reads request `filter=all`, App ID and exact name,
validate counts/unique IDs and stop with an error at the page budget instead of
claiming absence from incomplete results. Non-success terminal outcomes such as
neutral/skipped/cancelled remain distinct from success.

Provider writes contain only fixed content-free status messages. Creation always
starts in progress. Completion updates accept success/failure only from trusted
server callers; those callers must establish fresh policy evidence and serialize
publication. A completed run cannot be reset to pending: explicitly fail it or
journal a new pending run. Updates preflight the exact run/head/App/operation;
they never change name, head or external identity. Unexpected write responses,
redirects, malformed receipts and ambiguous provider failures return an unknown
result without automatic retry. Quota/backoff and definite access/not-found
errors remain separate. Private content, reviewer names and question text never
enter check output.

The REST contract was checked against the
[GitHub Checks API documentation](https://docs.github.com/en/rest/checks/runs?apiVersion=2026-03-10).

`CheckPublicationIntent` now freezes the actor/organization/proposal/snapshot,
numeric repository/installation/App identity, branch and exact refs, versions,
snapshot/evidence/authority digests, desired result and whole readiness-report
digest into a canonical request hash. Evaluation intents require consistent
provider-verified report identities and conclusions; the stored HTTP report
cannot produce one. Restore checks the complete hash rather than silently
rebuilding a changed request. These constructors do not grant publication
authority or prove that the evidence remains current.

Recovery decisions require complete bounded discovery and exact phase evidence.
An unknown create is never repeated on apparent absence; discovered pending runs
can be adopted only after the create-start record. A completion receipt requires
the exact update-start record and run/operation identity. Duplicate operations,
substituted runs and oversized discovery sets stop recovery. Other evaluations
of the same head do not prove that this operation succeeded.

Invalidation has a new journal operation but targets the exact original check
run and its original provider operation ID. It can only publish failure and
never creates a replacement run or depends on old approval evidence. The frozen
reason and target participate in the intent hash. A dedicated check-provider
factory now uses the existing frozen numeric connection/token selection path;
it preserves binding-change, missing-setup and source failures without giving a
content reader mutation authority.

Migration 031 stores immutable check intents, CREATE/UPDATE start markers,
adopted run identities and final receipts. A nullable `repositories.check_app_id`
disables admission until the exact numeric App is configured. Admission validates
the scoped ready snapshot, numeric connection, current actor role and metadata,
plus the exact queued job/operation and reserved audit/outbox pair. An active
check operation serializes this lane per proposal without taking the content
mutation slot. Generic workers cannot claim or complete its dedicated jobs.

Provider writes require a live owner/attempt fence and current authority/context;
receipt adoption may preserve a known run after review context or actor access
changes, while final publication remains forbidden. A reclaimed attempt cannot
reuse an old fence or replace a write-start marker. Started operations cannot be
failed or rewound to unstarted, and failed jobs with started writes retain UNKNOWN
operations. Completion commits its exact run/result, job/operation outcome and
separate audit/outbox pair together; partial receipts roll back. Delivered event
metadata, intents, start markers, runs and results cannot be rewritten or deleted.
Maintainer/Admin invalidation can target the original run after closure or a
context change and can only record failure.

These database guards do not verify provider refs or recompute application policy
proofs. The journal service must capture/restore the canonical intent, independently
recheck live provider and policy evidence, fence IO, and reconcile uncertainty.
`PgReviewChecks::evaluate` now admits evaluation work from server identities only.
It checks current write authority before observing an exact retry, rejects missing
App configuration and an occupied lane before provider IO, and obtains a fresh
provider-backed report. A short final transaction locks the proposal, organization
quota and actor/reviewer authority, then compares scope and private evidence
digests before inserting the operation/job/intent/audit/outbox together. The job
payload contains only the operation UUID. Admission is limited to 60 per actor
and 600 per organization in ten minutes; retries do not consume another slot.

The private readiness capture hashes current memberships/qualifications, effective
decisions/coverage, immutable snapshot facts, App/connection settings and append
counters for question/discussion revisions, official reviews, unit approvals and
acknowledged source visits. It excludes read time and text bodies. Equal visible
conclusions therefore cannot conceal changed underlying policy evidence. Locked
captures retain current positive authority until commit; no transaction spans
provider reads. Existing stored-report semantics and HTTP types are unchanged.

An exact reservation retry restores its original canonical intent and job without
new Git reads or new quota admission, including after closure. Current action
authority is still checked first; another actor/resource or intent purpose cannot
reuse that operation. A concurrent identical reservation wins once. New operations
cannot silently replace active work, and failed transactions leave no partial
queue/audit state. This is queue admission, not check publication or approval.

The execution journal now claims and renews dedicated owner/attempt leases,
reclaims expired work, derives phases from immutable markers/bindings, and stops
automatic attempts at exhaustion. `check`, `start_write` and `complete` verify
fresh provider refs/policy outside a transaction and recheck current local facts
and the live lease inside the short write transaction. The worker must supply
complete bounded check discovery; reconciliation rejects duplicate/substituted
runs, and each CREATE/UPDATE marker can be inserted only once. No Check Run HTTP
mutation is sent by the journal itself.

Known run identities can be adopted after authority loss, preserving evidence for
reconciliation without allowing another mutation. A completion receipt requires
the exact UPDATE phase, run, original provider operation and desired conclusion,
plus current policy and lease. It atomically publishes the audit/outbox pair and
job/operation result. The original completing frame can replay its committed
receipt without another Git call; other attempts remain fenced out. Replay also
validates the immutable run/phase chain instead of treating damaged bindings as
absence. A receipt does not approve or merge the proposal.

Deferral records a bounded content-free code and preserves a started operation as
UNKNOWN, including after exhaustion. Retry timing never rearms a marker. Provider
cooldown is stored for the exact organization/repository/remote/installation and
is shared with other provider consumers; new admission/observation respects it.
Heartbeats and fresh-policy operations must not race their own DB transactions.

`ReviewCheckWorker` connects the execution journal to the configured bound check
provider. One instance runs with the API. `REVIEW_CHECK_WORKER_TIMEOUT_SECONDS`
defaults to 120 and accepts 30–900 seconds; each claim uses a 120-second lease.
Provider IO renews it every 15 seconds. Journal operations renew at their boundaries
and never run alongside their own heartbeat transaction. Shutdown cancels the
attempt, preserving any start marker and lease for fenced recovery after expiry.

Every attempt restores the durable phase, exhausts bounded all-run discovery and
reads the exact owned run before deciding. Counts must agree across pages, IDs and
scope must be valid and unique, and discovery/read status must agree. Incomplete
or changing evidence cannot authorize a create. The worker starts a pending run,
adopts its exact receipt, updates once to the frozen result, then independently
observes and records completion. Fresh policy and refs are checked at mutation
and completion boundaries; the provider factory is reconnected before writes.
No transaction spans Check Run IO.

A lost create response resumes by observation and adoption; apparent absence never
permits another POST. A lost update response resumes by observation without another
PATCH. Permission or ref changes preserve known run identity while preventing
further writes or stale publication receipts. Fixed error codes and bounded
exponential retries preserve uncertainty; provider Retry-After also persists the
shared connection cooldown, which prevents another provider read during the wait.
Only UUIDs and typed outcomes enter worker logs. Private source or review text
never enters check output or errors.

`PgReviewChecks::invalidate` admits explicit Maintainer/Admin revocation using
an original evaluation operation and a bounded reason. It resolves the canonical
intent and exact adopted run from the immutable journal. The new intent keeps the
original head/App/provider operation identity and can only publish failure. Its
reason records the operator's rationale; admission does not claim to have observed
an automatic policy-change event. Old approvals and live old refs are unnecessary,
so the worker can revoke the run after closure or a head/target/policy change.
Current repository/installation/App binding and active privileged membership are
still required before admission and before each write/completion.

Admission locks the proposal, organization quota and current authority in the
shared writer order. It reserves the operation, job, canonical intent, audit and
outbox atomically. Exact replay checks current privilege first, restores the same
intent/job and consumes no new quota, including after configuration changes.
Actor, proposal, target operation, reason and purpose cannot be substituted. New
invalidation shares evaluation's 60-per-actor/600-per-organization ten-minute
limits, persistent connection cooldown and active-operation lane. An unadopted
run cannot be targeted; an unresolved active operation still requires recovery
before a separate invalidation can enter that lane. Admission itself has no
provider IO and never changes proposal state.

Migration 032 adds explicit administrator recovery of the same stopped job.
`PgReviewChecks::recover` binds a new request UUID to the exact original operation
and canonical intent hash. It keeps the original publishing actor, intent, provider
identity, markers and monotonically increasing attempts. RETRY_UNSTARTED requires
current original authority and local policy evidence; the worker checks live refs
before any write. OBSERVE_STARTED retains UNKNOWN and resumes the existing phase,
including safe identity adoption after original authority loss. Recovery does not
transfer publication authority to the administrator.

Each accepted recovery raises the existing attempt ceiling by eight. Admission is
limited to two per administrator and twelve per organization in ten minutes, and
respects the shared provider cooldown and active lane. Current Admin authority
precedes exact replay; replay restores the original accepted mode/budget without
adding attempts or another recovery. A current job or completed publication cannot
be reopened. Failed invalidation jobs retain their failure-only purpose.

The recovery row, queue/budget update, operation state and separate audit/outbox
pair commit together. Database guards require the exact receipt from the same
transaction before permitting a failed job to reopen or its budget to change.
Recovery records and event payloads are immutable. Old leases stay fenced out;
started writes remain single-use. Missing audit/events roll back the entire
recovery. The service performs no provider IO inside its transaction.

Administrator HTTP access is available at
`GET /api/v1/proposals/:proposal/review/checks/:operation` and the matching
`POST .../recover`. Reads require a current Admin session and expected actor,
return journal metadata with `provider_verified=false`, and perform no provider IO,
claims or mutations. The canonical request hash binds the explicit recovery body:
`request_id` plus `expected_request_hash`. The accepted receipt omits private job
identifiers and points Location to the same authorized status URL. Its mode and
budget describe that accepted recovery, including exact replay.

Recovery additionally requires the existing Origin/CSRF checks. Unknown query/body
fields, mismatched identities and oversized bodies are rejected. Fixed errors
separate stale context, active/completed jobs, idempotency mismatch, admission
limits, provider cooldown and unavailable journal data. Provider cooldown preserves
its own Retry-After; recovery quotas use 600 seconds. Responses are no-store.
Status reads recheck current authority and withhold malformed error metadata.
OpenAPI and the generated TypeScript schema include both routes.

`GET /api/v1/proposals/:proposal/review/checks` now discovers scoped operation
history in descending immutable creation/UUID order. Its bounded keyset pages
(default 20, maximum 50) validate the cursor in the same organization/proposal.
The whole page is authorized and validated in one transaction; malformed rows
cannot produce a partial successful page. Page and item responses bind the current
viewer and organization. Discovery reads no provider data and reserves no work.

The administrator page at `/changes/:id/review/checks` supports cursor and selected
operation URLs and is linked from proposal detail for administrator profiles.
The server still checks the actual organization role. The client verifies actor,
organization, proposal, operation, snapshot, intent hash, bounded counters, phase/
state consistency, unique pages and cursors before displaying metadata. Historical
publication is explicitly distinct from live provider state or merge permission.

Recovery is explicit. The original request UUID, canonical hash and actor/scope
are frozen in page memory while its result is uncertain. Timeouts, lost or
unverifiable replies preserve that same request for manual exact retry. Refresh
only reads status. Other operators may advance the budget in increments of eight
or advance an unstarted retry to observation; receipt validation accepts those
forward transitions while rejecting identity changes and phase rewinds. A valid
receipt triggers separate status observation. Cooldown only enables the retry
button after its deadline; it never posts or polls automatically.

Account/role loss clears private data and pending intent. Reads are cancelled on
scope changes, obsolete replies cannot install data, and synchronous event guards
prevent an action before reactive cleanup. Route/query changes, logout and unload
warn while a recovery result remains uncertain. Restoration from an operation URL
only reads metadata and cannot reconstruct or resend an old mutation. Native
labelled controls, focus outlines, RTL/LTR isolation and wrapping layouts support
keyboard and narrow-screen use. DOM/router tests cover these controls and guards;
physical keyboard, visual RTL and 320px checks remain pending because browser
selection and discovery found no connected browser.

Migration 033 and one `ReviewCheckDispatcher` in the API now deliver saved
`REVIEW_SNAPSHOT_PUBLISHED` events into check evaluation automatically. The source
must be backed by the real succeeded submission, job/result, ready snapshot and
exact publication flags. The existing canonical publication verifier supplies the
original publishing actor; event-owned IDs and flags grant no authority. Current
actor access, configured App, metadata, private policy evidence, provider refs,
admission quotas and the active check lane are rechecked by evaluation.

Check delivery has an independent record and never consumes or resets the PR
consumer's `outbox.sent_at`. Either consumer can run first, including for events
already delivered to PR synchronization. A short claim transaction assigns a
120-second owner/attempt lease. Evaluation has a 90-second deadline; provider
reads hold no transaction, source-event lock or delivery lock. The final admission
checks that same live fence and commits its source-bound intent/job/audit/outbox
with the ENQUEUED delivery receipt/audit in one transaction. The source event UUID
is the immutable check-operation identity. Manual evaluation retries cannot
substitute for a dispatched source, and dispatched admission cannot silently
adopt an unrelated intent.

Delivery independently records WAITING, ENQUEUED, SKIPPED or REJECTED outcomes.
Superseded backed snapshots skip before provider IO. Invalid envelopes are
quarantined; missing setup/authority, an occupied lane, quota and provider problems
retain a timed retry. Retry-After also persists shared connection cooldown for
the frozen source connection. Claims increment attempts after expiry; old owners
and old attempts cannot reserve work or rewrite terminal delivery. Shutdown leaves
the read-only attempt reclaimable after expiry. Missing acknowledgement audit
rolls back the check reservation. Source envelopes, association fields and terminal
receipts cannot be rewritten. Logs/audits contain fixed codes and identifiers.
The new scoped history index supports administrator keyset discovery.

Migration 034 extends the same dispatcher to `REVIEW_DECISION_RECORDED`,
`REVIEW_SOURCE_ACKNOWLEDGED` and `REVIEW_UNIT_APPROVED`. Each event must match the
immutable review, visit/delivery/attempt or individual approval record by its
stored event ID, exact organization/proposal, payload and audit. The original
reviewer becomes the publication actor and must still have current authority.
Copying a legitimate payload into another event grants no authority. A live
delivery lease cannot substitute the proposal owner for the reviewer.

These events trigger a fresh full readiness calculation, including current
independent votes and section coverage. They do not contribute a trusted result
from the envelope. Independent votes can advance pending to success; a later
request for changes produces failure. Visits and individual section approvals
remain distinct from proposal approval. Every evaluation retains its own source
event operation ID and provider Check Run; earlier runs never satisfy a later
operation. Events wait while the proposal's check lane is occupied. Unstarted
stale work stops without provider writes so a due event can retry; a started
uncertain operation still requires the existing reconciliation/recovery path.

ENQUEUED means delivery to the journal. The existing check worker separately
journals its single-use provider writes, reconciles uncertainty and rechecks
current authority/policy before publication. No proposal is approved or merged by
this dispatcher. Discussion/question blocker events, automatic invalidation
on policy/authority/ref changes, stale-operation resolution, native rule
verification and the remaining merge workflow remain open. App configuration
and native rule verification still need an administrator workflow. Real
GitHub/OIDC integration acceptance and the full specification's other requirements
remain pending.
