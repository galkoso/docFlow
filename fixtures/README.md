# Synthetic specification repository

`repository/` is a six-document Hebrew/English test book, not an approved product
specification or a production Git repository. All content is invented for tests.
It exercises stable section IDs, tables, links, code, Hebrew marks and emoji.

The application must read approved content from a configured Git snapshot; these
files must never silently substitute for missing provider data.

`readable-contract.json` comes from the document engine's `readable_contract`
example and is checked for drift in CI. Angular consumes the exact unit metadata
and event windows to verify safe rendering and continuation of Hebrew text,
nested ordered lists, fenced code, GFM tables and protected HTML source.
