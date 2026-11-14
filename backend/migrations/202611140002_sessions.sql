CREATE TABLE sessions (
    id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES users(id),
    token_hash bytea NOT NULL UNIQUE CHECK (octet_length(token_hash) = 32),
    csrf_hash bytea NOT NULL CHECK (octet_length(csrf_hash) = 32),
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL
);
CREATE INDEX sessions_expiry ON sessions (expires_at);

CREATE TABLE oidc_login_flows (
    state_hash bytea PRIMARY KEY CHECK (octet_length(state_hash) = 32),
    browser_binding_hash bytea NOT NULL CHECK (octet_length(browser_binding_hash) = 32),
    nonce text NOT NULL,
    pkce_verifier text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL
);
CREATE INDEX oidc_flows_expiry ON oidc_login_flows (expires_at);
