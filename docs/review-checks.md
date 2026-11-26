# Required review-policy calculation

The domain evaluator in `specspace_domain::review_check` calculates the future
`specspace/review-policy` required check from current trusted evidence. It does
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

Invalidation admission, automatic event dispatch, stale-operation resolution and
administrator recovery remain open. App configuration and native rule verification
also remain to be exposed to the authorized administrator. Runtime publishing of
admitted jobs does not establish native enforcement, proposal approval or merge
permission. Real GitHub integration acceptance, automatic invalidation and
serialized merge still require implementation.
