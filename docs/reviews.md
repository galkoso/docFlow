# Formal review preparation

Review configuration is editable at `/changes/:id/review`; its contract is in
[`review-configuration.md`](review-configuration.md). Formal submission, shared
snapshot publication, decisions, discussions, and provider PR/check orchestration
are still being implemented.

## Current server context

`ReviewExpectation` contains only the expected saved head, selected target, and
proposal/configuration/policy versions. `ReviewAuthorization::resolve` loads
`ReviewScope`, pairing the exact comparison repository binding and refs with
current review metadata and memberships. These types cannot be deserialized from
an HTTP request.

`PgReviewAuthorization` resolves the actor's active organization membership and
current edit authority. Readers and unassigned non-Admins cannot submit. Admins
remain confined to their organization. The proposal must be editable, all saved
head/version expectations must match, its owner must remain an active non-Reader,
and no unresolved content/submission/merge mutation may be pending. Repository
provider, numeric connection identity and main-branch configuration are validated
through the same resolver used by verified comparisons.

The transaction locks the proposal before reading its immutable configuration
revision. A waiting read sees the revision committed by the writer, while an old
expectation becomes stale. Reviewer memberships are loaded fresh: absent members
remain absent, disabled users are inactive, and role/domain changes are retained
for the preparation rules to reject. Historical assignments are not erased.
Domain order and duplicate qualifications are canonicalized so equivalent arrays
compare equally. Display-name changes are outside authority comparisons.

Scope equality supports rechecks across asynchronous work. The resolver makes no
Git calls and publishes no jobs or snapshots. An expected target is still a
selection: the comparison loader and `PreparedReview` must prove its lineage and
current-main match. The full preparation path must also validate all source and
media, recheck authorization/refs, and fence its eventual publication transaction.
An earlier successful scope read alone cannot authorize later publication.

## Bounded comparison and plan orchestration

`ReviewPreparer` resolves current review scope before loading any Git content,
loads the exact saved head/selected target through `ComparisonReader`, and
requires its frozen comparison context to match the resolved review scope.
Full edit/reviewer metadata is checked again after loading, including failed
comparison reads.

`PreparedReview` runs on the blocking pool with the verified complete comparison.
The two-slot preparation limit remains held through that CPU work when its caller
cancels or times out. A deadline bounds each preparation or later recheck. There
is no automatic retry, Git write, or branch/PR creation in this path.

After processing, authority is checked before either the plan or its validation
diagnostics can be returned. A fresh provider connection binds the same numeric
repository/install identity; branch and main refs must still match the expected
head/target, and authority is checked after connecting and reading refs. A later
`recheck` repeats authority/ref observations without reloading source.

The non-deserializable `ReviewCandidate` retains the source comparison and full
plan for the upcoming media and publication stages. Its media requirements are
not verification receipts. It has no public submission endpoint or published
shared snapshot yet. The current scope resolver is for pre-reservation work;
the durable worker still needs authority tied to its own fenced submission lease,
media verification and an atomic publication transaction. Fresh provider
observations cannot lock Git against later external changes, so approval and
merge gates must recheck their own exact refs.
