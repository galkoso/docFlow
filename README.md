# docFlow / SpecSpace

A Hebrew, RTL specification workspace with an Angular frontend and Rust backend.
Approved Markdown lives in Git; proposals, reviews and recovery metadata live in
PostgreSQL. External AI work uses downloadable bundles, without a model API.

The complete product contract is [the supplied specification](docs/specification.he.md).
Implementation status and verification evidence are tracked in
[the delivery checklist](docs/implementation.md). This repository is under active
development and is not yet ready for production use.

## Development history

Each coherent implementation step is committed separately and pushed to `origin`.
Commit messages identify the stage and behavior. A passing local check proves only
the behavior it covers; unverified acceptance criteria remain open in the checklist.
