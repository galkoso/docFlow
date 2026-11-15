# Lossless editor source model

`document-engine::editing` parses an authorized document into an exact YAML
preamble, ordered source blocks and a trailing suffix. Block keys bind the full
source hash and original byte range. UTF-8 byte offsets are distinct from browser
UTF-16 selection offsets.

An unchanged block retains its exact prefix and Markdown. A heading's prefix
includes its stable section marker. Unknown YAML fields/comments, Markdown
delimiter choices, code fences, image paths and link definitions survive an
unchanged round trip. Reference definitions and other syntax omitted from parser
events become protected source blocks. HTML, invalid links/images, unsupported
headings and excessively nested blocks are protected, rather than rewritten by
the rich editor. They remain inert text; this model never emits executable HTML.

An edit supplies the expected document SHA-256 and the complete new block order:

```json
{
  "expected_sha256": "<source-sha256>",
  "blocks": [
    { "action": "keep", "key": "<existing-block-key>" },
    { "action": "replace", "key": "<existing-paragraph-key>", "markdown": "Changed **paragraph**.\n\n" },
    { "action": "insert", "markdown": "<!-- ss:section:SEC-NEW -->\n## New section\n\n" }
  ],
  "deleted_keys": ["<explicitly-removed-block-key>"],
  "deleted_section_ids": ["SEC-EXPLICITLY-REMOVED"]
}
```

Every original block must appear once as retained, replaced or explicitly
deleted. Protected blocks require retention. Each replacement/insertion must
parse as one supported top-level block. Heading replacements keep their original
ID; new headings carry explicit unique IDs. Nested headings are included in
duplicate/removal checks. Section removals must match the declared removal list.
The assembled document is parsed again to reject changed block boundaries that
would consume a neighboring block. Callers receive exact source, new SHA-256,
whether bytes changed and the removed section IDs.

The source limit is 2MiB and the model limit is 10,000 blocks per document.
Heading lookup uses a map; located findings use sorted intervals. Resource limits
also apply while assembling edits. A no-op retains the original source hash.

The server must build the model from its own pinned source, rather than trusting
a browser-supplied model. This boundary is not a Git save or authorization grant.
The eventual commit use case must also check current assignment, proposal
state/version, provider head, catalog and incoming references, normalize changed
content according to policy, and journal the atomic Git operation. Explicit
authorized source mode is a separate path for fixing protected syntax. Browser
rich-text integration and its round-trip acceptance remain in progress.

## Proposal-scoped HTTP boundary

`GET /api/v1/proposals/:id/documents/:document/editor?commit=:sha` returns the
pinned model alongside the proposal number, branch, target, recorded head,
metadata version and current `can_edit` capability. Historical and closed views
remain readable with editing disabled. An invalid source document returns an
explicit source-model failure; the authorized source-repair path is still pending.

`POST .../editor/preview` accepts `commit`, `expected_proposal_version` and an
`edit` object using the protocol above. It requires current edit assignment,
Origin and session CSRF validation. It rejects stale proposal/head/document
versions and rechecks access after provider/CPU waits. The response distinguishes
`base_snapshot_sha` from the uncommitted Markdown SHA-256; it includes inert
render events, document findings and declared removed IDs. Preview does not
write a recovery buffer, operation journal, Git Commit or review approval.
Repository-wide validation and Git publication remain separate requirements.

## Browser representation

The browser core uses a ProseMirror schema with CommonMark text/marks, simple GFM
tables, strike-through and task items. Stable section markers become heading
attributes. Images use the pinned private proposal proxy; schema DOM contains
only known elements/attributes. Unsupported or non-round-trippable blocks become
protected atoms. A parsing/serialization probe compares semantic trees without
mutating ProseMirror attributes. Protected source cannot be removed by rich
transactions. Copy/split normalization keeps existing identities and assigns
fresh IDs to copied/new headings, including copies inserted before originals.

Unchanged nodes yield `keep` operations and exact original bytes. Changed nodes
serialize individually; omitted original nodes generate explicit block/section
deletions. Undo/redo returns to original source when the document tree returns to
its baseline. Buffer instances belong to one document/version and remain local;
page/session recovery integration and Git saving are subsequent work.

`cargo run -p specspace-document-engine --example editor_contract` generates
`fixtures/editor-contract.json`. Backend CI checks drift. Browser tests consume
the actual Rust models and compare their no-op and canonical heading changes
with Rust-validated output, alongside GFM, identity and inert-DOM checks.
