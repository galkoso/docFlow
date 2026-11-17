# Review configuration

`GET` and `PUT /api/v1/proposals/{proposal}/review/configuration` read and save
reviewer assignments and the UI-impact decision. Both require a session and the
captured `X-Expected-Actor`. Writes also require the same-origin CSRF headers.
Responses are private and `no-store`.

Saving configuration does not submit a review, create a PR, or approve content.
Partial assignments and undecided UI impact can be saved; formal submission and
approval have additional requirements. The reviewer picker and review page remain
to be implemented.

## Write intent and retries

A write contains `request_id`, `expected_head_sha`, `expected_proposal_version`,
`expected_configuration_version`, and `settings`:

```json
{
  "request_id": "00000000-0000-4000-8000-000000000001",
  "expected_head_sha": "1111111111111111111111111111111111111111",
  "expected_proposal_version": 1,
  "expected_configuration_version": 1,
  "settings": {
    "ui_impact": "no",
    "ui_impact_reason": "This change only clarifies permission rules.",
    "assignments": [
      {
        "reviewer_id": "00000000-0000-4000-8000-000000000002",
        "domain": "PRODUCT",
        "required": true
      }
    ]
  }
}
```

IDs and hashes in this example are synthetic. Domains are `PRODUCT`,
`DEVELOPMENT`, and `DESIGN`; UI impact is `yes`, `no`, or `undecided`. A `no`
decision requires a nonblank reason. There are at most 200 distinct user/domain
assignments, a reason has at most 10,000 Unicode characters, and the HTTP body is
limited to 128 KiB. Unknown JSON fields and query parameters are rejected.

Capture the actor, versions, key, and settings before sending. After a lost
response, retry that intent with the same key. Do not replace expected versions
or settings under that key. Ordering of assignments, surrounding reason
whitespace, and SHA case are canonicalized. Reusing a key for different canonical
input returns `409 IDEMPOTENCY_KEY_REUSED`.

Identical retries return the originally applied versions plus the current
configuration. They remain observable after subsequent edits, a pending Git
operation, or proposal closure, provided the actor still has edit authority.
Revoked authority cannot be restored through an old receipt. A new intent must
match all expected versions and the saved head; otherwise it returns
`409 REVIEW_CONFIGURATION_STALE`. New writes also reject final/provisioning states
and unresolved Git mutations, including unknown outcomes.

## Authorization and eligibility

An active organization member can read the configuration. An owner, assigned
editor, or organization Admin can change it; Readers cannot edit. Organization
boundaries are enforced even for Admins. Each selected reviewer must currently
be active, hold Reviewer/Maintainer/Admin role, have the selected domain, and be
someone other than the proposal author.

Read responses include current eligibility alongside immutable selections.
Revoking membership or qualification invalidates eligibility without deleting
historical assignments. Final submission must independently enforce distinct
qualified product/development reviewers and design when UI impact is yes.

## Persistence and invalidation

Migration 009 creates an initial revision for existing and newly created
proposals. A changed configuration publishes a new immutable revision, advances
both proposal and configuration versions, records an audit event, and emits
`PROPOSAL_REVIEW_CONFIGURATION_CHANGED` with `invalidate_review: true` in the
same transaction. An `APPROVED` proposal returns to `IN_REVIEW`. A canonical
no-op stores only its replay receipt; it preserves versions and approval state.

The proposal is locked before reading its selected immutable revision, so a
request waiting for a concurrent writer reads the newly committed revision.
Database constraints check that relational assignments exactly match the
canonical revision manifest, including when deferred constraints are forced
early. Old revisions, assignments, and operation receipts reject updates,
deletes, and truncation. New assignments cannot be appended to a revision in a
later transaction. Membership deletion does not cascade into review history.

The durable invalidation event is a prerequisite for the upcoming formal-review
workflow. Snapshot publication, decisions, discussions, PR/check orchestration,
and merge gates are still separate implementation work.
