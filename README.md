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

## Local checks

Frontend: `cd frontend && npm ci && npm run build && npm test -- --watch=false`.
Run `npm start` from `frontend` for the development server.

Backend: `cd backend && cargo test --workspace --locked`.
Run `cargo fmt --all -- --check` and
`cargo clippy --workspace --all-targets --locked -- -D warnings` before committing.

The API runs with `DATABASE_URL=postgres://... cargo run -p specspace-api`
from `backend`. It binds to `127.0.0.1:3000` by default; set
`APP_LISTEN_ADDRESS` to change this. `/api/v1/health/live` reports process health;
`/api/v1/health/ready` checks PostgreSQL and returns 503 when unavailable.
These are infrastructure endpoints; product routes are still being implemented.

For local PostgreSQL, run `docker compose up -d --wait`. Copy `.env.example` to
`.env` and load its values into your shell. From `backend`, run
`cargo run -p specspace-infrastructure --bin migrate` to apply migrations, and
`cargo test --workspace --all-features --locked` to include PostgreSQL integration
tests. Integration tests create and remove isolated databases; use a dedicated
development database role with database-creation privileges, as provided by Compose.
The default test command runs unit checks without requiring PostgreSQL.

## Identity configuration

Set `APP_PUBLIC_URL`, `OIDC_ISSUER`, `OIDC_CLIENT_ID` and `OIDC_CLIENT_SECRET` in
the server environment. Register the exact redirect URI
`<APP_PUBLIC_URL>/api/v1/auth/callback` with the identity provider. Users must be
pre-provisioned in `users` and `memberships` using the provider's exact issuer and
stable subject; email addresses and browser-supplied roles do not grant access.

Production is the default `APP_ENV` and requires HTTPS for public/identity URLs.
`APP_ENV=development` permits loopback HTTP and requires a loopback API listener.
Serve frontend and `/api` at the same origin. Session cookies are HttpOnly;
write requests require the configured Origin and an `X-XSRF-TOKEN` header matching
the server session. OAuth tokens are never returned to the browser.
Open the frontend using the exact `APP_PUBLIC_URL` so the login cookie and callback
use the same origin. The Angular development server proxies `/api/**` to the local
API at `127.0.0.1:3000`.

Endpoints implemented so far: `/api/v1/auth/login`, `/api/v1/auth/callback`,
`/api/v1/auth/logout` and `/api/v1/me`. Identity-provider setup and the full browser
login flow still require environment validation; see the delivery ledger.

The current API schema is [docs/openapi.json](docs/openapi.json), generated from
Rust types and routes. Regenerate from `backend` with
`cargo run --quiet -p specspace-api --bin export-openapi > ../docs/openapi.json`.
Backend CI checks that this artifact matches the source contract.

Angular API types are generated with `npm ci --prefix tools/api-client`, then
`cd frontend && npm run api:generate`. The generator has an isolated TypeScript 5
tooling package; the Angular application continues to use TypeScript 6.

## Specification repository connection

Set `GITHUB_APP_ID` and `GITHUB_PRIVATE_KEY_SECRET_REF` together in the API server
environment. Secret references support `env:NAME` or an absolute `file:/path`;
the key is never accepted from a browser request. `GIT_PROVIDER` defaults to GitHub.
The organization's `repositories` record supplies the numeric GitHub repository
and installation IDs (`remote_id` and `installation_id`) and `default_branch=main`.
GitHub metadata is checked against the configured ID before the client is used.

Authenticated reads are available under `/api/v1/repositories`: repository list,
`/:id/specification`, `/:id/documents/:document?commit=...`, and
`/:id/find?q=...&commit=...`. Book metadata returns the pinned SHA; subsequent
document/search requests use it. Explicit versions must belong to main history.
The in-memory snapshot cache is bounded, disposable, and always checks current
access. Durable search indexing, the worker and the reader UI remain in progress.

## Recovery storage

Set `DRAFT_RETENTION_DAYS` explicitly (1–365 days) to enable server recovery
writes. The sample environment proposes 30 days; the organization must select its
production policy. An unset value disables recovery writes and is reported in
the recovery context. The API exposes the configured policy and each expiry.
Expired content is hidden immediately; a cleanup task removes up to 500 expired
buffers per minute. Shortening the policy also expires older buffers.
Policy shortening is persisted before the API serves requests, so increasing or
removing the policy later cannot revive expired buffers. Applying a configured
recovery policy requires PostgreSQL to be available at startup.

`GET /api/v1/proposals/:id/draft` lists the current user's buffer metadata;
`?document_id=DOC-ID` retrieves one buffer's exact source. `PUT` requires a
`save_id`, `document_id`, `base_head_sha`, `expected_draft_version` and
`content_utf8`. Retrying the same accepted save preserves its version. Conflicting
tabs receive 409. Recovery accepts unfinished source, retains its reported base
for comparison and works without Git. It creates no Commit or review approval.
Each user has up to 1,000 buffers / 20MiB per proposal, with 2MiB per document.
An authenticated, CSRF-protected `DELETE` with document ID and expected draft
version discards only that user's selected buffer. Recreated buffers receive new
versions, protecting them from delayed requests. Git saves remain a separate
implementation stage.

All recovery requests require `X-Expected-Actor` with the UUID captured when the
user began the intent. The server rejects a missing, malformed, duplicate or
different actor with `409 SESSION_IDENTITY_CHANGED`. This protects frozen retries
when another tab changes the shared login cookie. The header grants no access;
the server session and current permissions remain authoritative. Other
authenticated endpoints also reject this header when it is supplied incorrectly.

Proposal chapter/section links open `/changes/:id/edit/:document?commit=<sha>`.
The editor offers rich controls, complete source repair, server validation and
an inert preview. Recovery autosaves after two idle seconds with one request at
a time. An existing recovery copy requires an explicit choice; a different base
requires an explicit adoption decision. Unknown results retain the exact intent
for manual retry. No retry automatically follows login. Temporary same-user
session refresh preserves local rich history and unfinished link/image fields;
a different login or successful logout purges private memory. Navigation, unload
and logout warn about uncommitted work. A permitted local download is labelled
uncommitted; unrepresentable rich edits export the actual editor state, protected
source model and unfinished fields as JSON instead of stale Markdown. No editor
action currently creates a Git commit or submits a review. Browser layout,
RTL/IME and accessibility acceptance remain open.


The backend Git-save worker uses a durable journal, lease fencing and exact
commit/tree/content verification. It never repeats an uncertain Git write.
`SAVE_WORKER_TIMEOUT_SECONDS` defaults to 900 (30–3600 allowed). Apply migration
007 before starting this runtime. Known terminal saves purge private request
bodies; unresolved UNKNOWN saves retain their reservation and input pending
operational resolution. Save submission API and editor Git controls are still
pending; see [the save protocol](docs/commit-saves.md).
