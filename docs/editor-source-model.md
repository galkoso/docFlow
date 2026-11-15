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
