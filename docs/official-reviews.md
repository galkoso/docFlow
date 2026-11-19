# Official snapshot decisions

Migration 018 and `PgOfficialReviews` provide official `APPROVE_ALL`,
`REQUEST_CHANGES` and `COMMENT` actions. Current organization members read history
through `GET /api/v1/proposals/{proposal}/review/snapshots/{snapshot}/decisions`.
An optional `after` UUID seeks an immutable revision cursor; pages contain at
most twenty entries. Foreign/unknown cursors fail rather than changing scope.

The same URL accepts POST with the session, `X-Expected-Actor`, allowed Origin,
CSRF header and `OfficialReviewRequest`. New decisions return 201; exact receipt
replay returns 200. All responses use no-store. GET performs no provider IO and
separates an effective decision from current reviewer eligibility and validity
for the stored database context. It does not claim live Git or merge readiness.

Each request names a sealed snapshot, expected fingerprint, qualified review
domain, explanation and idempotency UUID. The server checks current organization
membership, active reviewer/maintainer/admin role, domain qualification, frozen
assignment and independence from the author. It requires the active snapshot,
current policy/configuration and repository binding. Approving all changes also
requires a decided UI impact and both the recorded base and verified merge base
to equal the target. An explicit explanation is mandatory for every action.

Before a new write, the service releases its database transaction and connects a
provider through the exact frozen numeric binding. Two branch/main reads must
match the sealed head and target. A second transaction rechecks current authority
and snapshot context, including changes that committed while a row lock was
awaited. Provider IO has a fifteen-second bound; no transaction spans that IO.
Provider state can still change after observation, so merge/check consumers must
recheck current refs and eligibility; a stored decision is not merge permission.

The decision, human audit, recalculation event and latest non-comment decision
pointer commit atomically. History, scope and event envelopes are immutable.
Comments append to history without replacing an approval or revision request.
A revision request changes the proposal to `CHANGES_REQUESTED`; replacing the last
currently qualified request returns it to `IN_REVIEW`. The service never sets
`APPROVED`, merges content or changes refs. Full policy evaluation remains required.

Exact replay checks current reviewer access and the original intent, then returns
the stored receipt without provider IO, another event or a new decision, including
after proposal closure. A different actor/body/scope cannot reuse the request ID.
New persisted actions are limited to thirty per reviewer and two hundred per
organization in ten minutes; replay does not consume that quota. Migration 019
also charges authorized provider-read attempts, including failures/timeouts, at
those limits. Failed request bodies remain bound to their original request ID.
Eight read operations can run concurrently per API service instance, and no
database connection remains held during provider reads. Significant source
failures append a content-free audit record.

Rate limits persist a cooldown scoped to organization, repository and captured
remote/installation IDs. Other requests/reviewers cannot bypass it by changing
their request IDs. The longest observed cooldown wins; 429 responses preserve
remaining Retry-After seconds. Receipt replay bypasses provider IO, quota and
cooldown after current reviewer access is checked. Provider failures do not close
the proposal or delete its saved snapshot.

Reviewer controls are added in the following implementation stage.

Section visits/coverage, broader threaded discussions and product questions,
current policy-evidence assembly, required-check publication/reconciliation and
native repository enforcement remain pending, alongside all remaining full-spec
work. A recorded approve-all action is not final approval or merge authorization.
