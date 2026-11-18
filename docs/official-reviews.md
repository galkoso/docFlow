# Official snapshot decisions

Migration 018 and `PgOfficialReviews` provide the persistence/service foundation
for official `APPROVE_ALL`, `REQUEST_CHANGES` and `COMMENT` actions. This stage is
not yet exposed through HTTP or the review page. History reads, section visits/
coverage, reviewer controls and required-check publication remain to be connected.

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
organization in ten minutes; replay does not consume that quota. Provider-request
admission/backoff limits must also be wired before exposing the service through
an HTTP mutation endpoint.
