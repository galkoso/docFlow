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
