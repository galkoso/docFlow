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

## Required next implementation

The save application/journal/worker must bind the entire immutable request to
actor, proposal, repository configuration, expected head and metadata version.
It must validate the complete candidate book, stable identities, incoming links,
catalog, permitted assets and normalized changed Markdown before mutation. A
no-op must not create a commit. All changed files must be one atomic mutation.

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
