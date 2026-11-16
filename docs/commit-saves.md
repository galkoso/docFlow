# Git save boundary

The editor currently provides local source and private recovery, not Git saves.
This document records the save protocol as its implementation is added.

## Implemented: pinned commit evidence

`GitProvider::commit_receipt` reads one exact immutable commit object. The GitHub
adapter checks the returned SHA against the requested SHA and returns its tree,
parents, complete message and UTC committer timestamp. Messages are bounded at
64KiB, parent lists at 16; malformed hashes/dates, repeated/self parents and NUL
messages are rejected. Ordinary reader metadata stays independent of these
extra reconciliation fields.

`CommitReceipt::matches_operation` checks one exact expected parent and a final
two-line `SpecSpace-Operation` / `SpecSpace-Actor` trailer block. Both UUIDs must
match the journal intent. Duplicate or reserved fields in earlier text, extra
trailer/footer lines, nil identities and self-parent results fail the match.
CRLF message framing is accepted. User commit messages cannot supply reserved
identity fields; the adapter supplies them from the server-owned request.

This match is evidence of operation identity only. It does not verify intended
file contents, current authorization, proposal version, branch reachability, or
whether the observed commit has been published into the recorded proposal state.
No worker may equate matching trailers or a changed branch head with a completed
save.

The existing adapter performs one atomic `createCommitOnBranch` request with the
exact expected head and all file changes. It does not retry uncertain mutations.
The new evidence read likewise performs no writes.

## Implemented: candidate Markdown validation

`save_validation::validate` checks a complete candidate book, including all
`docs/*.md` files outside the catalog. Catalog mismatches, duplicate identities,
heading identity/title problems, orphan section markers, disallowed link schemes, broken
document/section references and absent image paths block a draft plan. Section
identity checks include nested and empty headings; incoming links in untouched
chapters are checked against the complete new book. Local fragments, `ss:` IDs
and repository document paths resolve to exact document ownership. URI escapes
are decoded once with strict UTF-8 and canonical repository-path validation.
External URLs are never fetched.

Protected HTML/unsupported heading levels remain visible warnings rather than
being removed from source. Missing image alt is also a draft warning; it still
must block review/merge as required by policy. The checker returns the exact set
of referenced assets. Path existence is not media-byte verification: the worker
must read these assets at the same pinned commit and verify hash, MIME and pixels
before it can write a save. This checker does not authorize an operation or
declare a proposal ready for review/merge.

Reports keep at most 1,000 findings while retaining full reported counts and
blocker counts, so late blockers cannot be hidden by warnings. Paths longer than
240 characters are rejected with explicitly truncated diagnostic paths. Internal
section/asset maps are excluded from diagnostic JSON. Catalog findings can be
streamed without first materializing every repeated diagnostic.

Reference processing has explicit resource bounds. Resolved-destination work is
limited to 64MiB across validation passes, and definition scanning to 128MiB
across the book. The checker also detects exhaustion of pulldown-cmark 0.13's
reference expansion budget, which otherwise turns later reference uses into
plain text. A callback probe counts original URL/title costs using short
destinations. It preserves first-definition-wins semantics, escaped labels,
multiline titles and quoted definitions; duplicate-definition rescanning is
bounded. Exceeding a budget yields an incomplete blocking report, never a
truncated valid result. These limits do not change source bytes.

## Implemented: existing-document save planning

`ValidatedSave` captures actor/proposal/repository UUIDs, expected commit and
proposal version, message, and up to 50 documents with exact base SHA-256 hashes.
The complete raw intent is fingerprinted with the `specspace-save-v1` protocol.
A 64KiB buffered hash writer avoids allocating another fully JSON-escaped copy.
Nil scope IDs, malformed hashes/IDs, duplicate document IDs, NUL content, invalid
messages and input beyond 2MiB per file / 10MiB per request fail validation.

`prepare` accepts only the matching pinned repository/commit, resolves file paths
through a valid catalog, checks exact original hashes and applies all candidate
changes together. It normalizes changed files to LF/NFC without reserializing
Markdown or unknown YAML fields. Unchanged files retain their original bytes.
Raw no-ops and edits normalizing back to Git produce an empty mutation list;
normalized output is checked against the atomic byte limit again. The complete
candidate then passes through book validation, so a section deletion cannot
leave an incoming link in an untouched chapter. Repairs of invalid old source
report unknown removed identities rather than claiming none were removed.

The resulting plan exposes read-only access to its original scope, fingerprint,
head/version, canonical changed source, hashes and removed identities. Per-file
flags identify normalization and normalization-only byte changes; they do not
claim a complete semantic/formatting diff. `file_changes` produces one bounded
batch of puts for the provider. This existing-document path cannot add arbitrary
files, edit the catalog, or remove a source file. File creation/import workflows
remain separate work. A plan is not authorization and does not call Git or the
database; assets and current context still require worker verification.

## Required next implementation

The save journal/worker must bind the planned intent to current authorization,
immutable repository configuration, expected head and current metadata version.
It must load the complete pinned snapshot itself, verify referenced media bytes,
and persist the canonical plan before mutation. A no-op must not create a commit.
All changed files must be one atomic mutation.

The mutation boundary must be durable and lease-fenced, with current eligibility
checked before Git I/O. After an uncertain write, recovery may only observe and
verify the operation's commit against its expected parent, planned file content
and complete tree changes. It must establish branch reachability and recheck
current context before publishing recorded head/version and invalidating review
state. Revocation, external advancement, missing/diverged refs and failures
between Git and PostgreSQL must remain explicit; none authorize a blind retry.

An authenticated API and the editor's Git-save interaction still need to connect
this workflow, flush active fields, preserve later local edits and frozen retries,
and show a verified SHA/time only after publication succeeds. Git-save acceptance
and crash/restart tests remain open.

Provider reference: [GitHub Git commit objects](https://docs.github.com/en/rest/git/commits?apiVersion=2026-03-10#get-a-commit).
