-- Form preparation consumes the same non-recycling sequence as final proposals.
-- A number is a preview reservation, never a claim that a Git branch exists.
CREATE TABLE proposal_creation_contexts (
    id uuid PRIMARY KEY,
    organization_id uuid NOT NULL REFERENCES organizations(id),
    repository_id uuid NOT NULL,
    actor_id uuid NOT NULL REFERENCES users(id),
    idempotency_key text NOT NULL CHECK (length(idempotency_key) BETWEEN 1 AND 200),
    number bigint NOT NULL DEFAULT nextval('proposals_number_seq') UNIQUE,
    source_sha text NOT NULL CHECK (source_sha ~ '^[0-9a-f]{40,128}$' AND length(source_sha)%2=0),
    remote_id text NOT NULL,
    installation_id text NOT NULL,
    expires_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (organization_id,repository_id) REFERENCES repositories(organization_id,id),
    UNIQUE (organization_id,actor_id,idempotency_key),
    UNIQUE (organization_id,repository_id,actor_id,id,number)
);
ALTER TABLE proposals ADD COLUMN creation_context_id uuid UNIQUE;
ALTER TABLE proposals ADD CONSTRAINT proposal_creation_context_scope
    FOREIGN KEY (organization_id,repository_id,author_id,creation_context_id,number)
    REFERENCES proposal_creation_contexts(organization_id,repository_id,actor_id,id,number);
CREATE INDEX proposal_creation_context_expiry ON proposal_creation_contexts(expires_at);
