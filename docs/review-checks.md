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
Fenced check publication/reconciliation, native rule verification and serialized
merge still require implementation. Until those are connected, no runtime
required check is advertised as enforcing this policy.
