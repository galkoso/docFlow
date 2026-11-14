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
