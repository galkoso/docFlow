# Product questions

Product questions are proposal-level business decisions, separate from formal
snapshot review approvals. Opening a question with an answer produces PROPOSED,
including when the caller is qualified in Product or the answer came from an
import. OPEN has no answer; PROPOSED has an unapproved answer; DECIDED records an
explicit Product decision. A blocking OPEN or PROPOSED question prevents merge.

An active organization member with comment authority can ask or propose an
answer. Deciding an answer requires current Reviewer/Maintainer/Admin authority
and Product qualification. Admin or Maintainer alone does not imply product
expertise. This decision does not substitute for the independent official
Product/Development approvals required on the snapshot.

The creator or qualified Product reviewer can reopen a proposed/decided question.
Reopening clears the current answer/decider while prior immutable revisions will
retain the earlier decision. A decided answer cannot silently be replaced with a
suggestion: reopen first, or record a new explicit Product decision. Only Product
authority can remove blocking status; adding it requires the creator or Product.

Closed, merged and provisioning proposals reject new mutations. During MERGING,
an action cannot introduce a new unresolved blocker. Nonblocking questions and
actions that do not introduce blockers remain possible. Text preserves original
UTF-8/CRLF bytes, rejects NUL/blank content and is limited to 10,000 Unicode scalar
values per question/answer.

The domain lifecycle is implemented and tested. Persistence, API, import
provenance validation, interface and readiness integration are subsequent stages;
the readiness report continues to treat question evidence as unknown until its
authoritative read/write integration is in place.
