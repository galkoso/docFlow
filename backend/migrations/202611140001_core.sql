-- Git stores content. These tables store workflow and recovery metadata only.
CREATE TABLE organizations (
    id uuid PRIMARY KEY,
    name text NOT NULL CHECK (length(name) BETWEEN 1 AND 200),
    policy_version bigint NOT NULL DEFAULT 1 CHECK (policy_version > 0)
);

CREATE TABLE users (
    id uuid PRIMARY KEY,
    issuer text NOT NULL,
    external_subject text NOT NULL,
    display_name text NOT NULL,
    status text NOT NULL DEFAULT 'ACTIVE' CHECK (status IN ('ACTIVE', 'DISABLED')),
    UNIQUE (issuer, external_subject)
);

CREATE TABLE memberships (
    organization_id uuid NOT NULL REFERENCES organizations(id),
    user_id uuid NOT NULL REFERENCES users(id),
    role text NOT NULL CHECK (role IN ('READER', 'CONTRIBUTOR', 'REVIEWER', 'MAINTAINER', 'ADMIN')),
    review_domains text[] NOT NULL DEFAULT '{}'
      CHECK (review_domains <@ ARRAY['PRODUCT','DEVELOPMENT','DESIGN']::text[]),
    PRIMARY KEY (organization_id, user_id)
);

CREATE TABLE teams (
    id uuid PRIMARY KEY,
    organization_id uuid NOT NULL REFERENCES organizations(id),
    name text NOT NULL,
    UNIQUE (organization_id, id),
    UNIQUE (organization_id, name)
);

CREATE TABLE team_members (
    organization_id uuid NOT NULL,
    team_id uuid NOT NULL,
    user_id uuid NOT NULL,
    PRIMARY KEY (team_id, user_id),
    FOREIGN KEY (organization_id, team_id) REFERENCES teams(organization_id, id),
    FOREIGN KEY (organization_id, user_id) REFERENCES memberships(organization_id, user_id)
);

CREATE TABLE repositories (
    id uuid PRIMARY KEY,
    organization_id uuid NOT NULL REFERENCES organizations(id),
    provider text NOT NULL CHECK (provider = 'GITHUB'),
    remote_id text NOT NULL,
    installation_id text NOT NULL,
    default_branch text NOT NULL DEFAULT 'main',
    current_main_sha text,
    UNIQUE (organization_id, id),
    UNIQUE (organization_id, provider, remote_id)
);

CREATE TABLE proposals (
    id uuid PRIMARY KEY,
    number bigint GENERATED ALWAYS AS IDENTITY UNIQUE,
    organization_id uuid NOT NULL,
    repository_id uuid NOT NULL,
    title text NOT NULL CHECK (length(title) BETWEEN 3 AND 120),
    branch text NOT NULL CHECK (branch LIKE 'change/%' AND length(branch) BETWEEN 8 AND 100),
    branch_key text GENERATED ALWAYS AS (lower(branch COLLATE "C")) STORED,
    owner_id uuid NOT NULL,
    author_id uuid NOT NULL,
    team_id uuid NOT NULL,
    state text NOT NULL DEFAULT 'PROVISIONING' CHECK (state IN (
      'PROVISIONING','DRAFT','IN_REVIEW','CHANGES_REQUESTED','APPROVED','MERGING','MERGED','CLOSED')),
    description text NOT NULL CHECK (length(description) BETWEEN 10 AND 10000),
    ui_impact text NOT NULL CHECK (ui_impact IN ('YES','NO','UNDECIDED')),
    ui_impact_reason text,
    base_sha text,
    head_sha text,
    target_sha text,
    version bigint NOT NULL DEFAULT 1 CHECK (version > 0),
    delivery_status text NOT NULL DEFAULT 'PLANNED' CHECK (delivery_status IN ('PLANNED','IMPLEMENTED','RELEASED')),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (organization_id, id),
    UNIQUE (repository_id, branch_key),
    FOREIGN KEY (organization_id, repository_id) REFERENCES repositories(organization_id, id),
    FOREIGN KEY (organization_id, owner_id) REFERENCES memberships(organization_id, user_id),
    FOREIGN KEY (organization_id, author_id) REFERENCES memberships(organization_id, user_id),
    FOREIGN KEY (organization_id, team_id) REFERENCES teams(organization_id, id),
    CHECK (ui_impact <> 'NO' OR length(trim(coalesce(ui_impact_reason, ''))) > 0),
    CHECK (state = 'PROVISIONING' OR (base_sha IS NOT NULL AND head_sha IS NOT NULL AND target_sha IS NOT NULL))
);
CREATE INDEX proposals_listing ON proposals (organization_id, updated_at DESC, id);

CREATE TABLE proposal_collaborators (
    organization_id uuid NOT NULL,
    proposal_id uuid NOT NULL,
    user_id uuid NOT NULL,
    access text NOT NULL CHECK (access IN ('EDIT','READ')),
    PRIMARY KEY (proposal_id, user_id),
    FOREIGN KEY (organization_id, proposal_id) REFERENCES proposals(organization_id, id),
    FOREIGN KEY (organization_id, user_id) REFERENCES memberships(organization_id, user_id)
);

CREATE TABLE draft_buffers (
    organization_id uuid NOT NULL,
    proposal_id uuid NOT NULL,
    user_id uuid NOT NULL,
    document_id text NOT NULL,
    base_head_sha text NOT NULL,
    version bigint NOT NULL DEFAULT 1 CHECK (version > 0),
    content text NOT NULL CHECK (octet_length(content) <= 2097152),
    expires_at timestamptz NOT NULL,
    PRIMARY KEY (proposal_id, user_id, document_id),
    FOREIGN KEY (organization_id, proposal_id) REFERENCES proposals(organization_id, id),
    FOREIGN KEY (organization_id, user_id) REFERENCES memberships(organization_id, user_id)
);

CREATE TABLE git_operations (
    id uuid PRIMARY KEY,
    organization_id uuid NOT NULL REFERENCES organizations(id),
    proposal_id uuid,
    actor_id uuid NOT NULL,
    kind text NOT NULL CHECK (kind IN ('CREATE','COMMIT','SUBMIT','EXPORT','IMPORT_APPLY','REFRESH_BASE','MERGE','REVERT')),
    idempotency_key text NOT NULL CHECK (length(idempotency_key) BETWEEN 1 AND 200),
    request_hash text NOT NULL,
    expected_sha text,
    result_sha text,
    state text NOT NULL DEFAULT 'PENDING' CHECK (state IN ('PENDING','RUNNING','SUCCEEDED','FAILED','UNKNOWN')),
    error_code text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (organization_id, id),
    UNIQUE (organization_id, actor_id, kind, idempotency_key),
    FOREIGN KEY (organization_id, proposal_id) REFERENCES proposals(organization_id, id),
    FOREIGN KEY (organization_id, actor_id) REFERENCES memberships(organization_id, user_id)
);

CREATE TABLE jobs (
    id uuid PRIMARY KEY,
    organization_id uuid NOT NULL REFERENCES organizations(id),
    operation_id uuid,
    kind text NOT NULL,
    state text NOT NULL DEFAULT 'QUEUED' CHECK (state IN ('QUEUED','RUNNING','SUCCEEDED','FAILED','CANCELLED')),
    payload jsonb NOT NULL DEFAULT '{}',
    result jsonb,
    attempts integer NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    max_attempts integer NOT NULL DEFAULT 5 CHECK (max_attempts > 0),
    available_at timestamptz NOT NULL DEFAULT now(),
    lease_owner uuid,
    leased_until timestamptz,
    heartbeat_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (organization_id, operation_id) REFERENCES git_operations(organization_id, id),
    CHECK (state <> 'RUNNING' OR (leased_until IS NOT NULL AND lease_owner IS NOT NULL))
);
CREATE INDEX jobs_queued ON jobs (available_at, created_at, id) WHERE state = 'QUEUED';

CREATE TABLE outbox (
    id uuid PRIMARY KEY,
    organization_id uuid NOT NULL REFERENCES organizations(id),
    event_type text NOT NULL,
    entity_id uuid NOT NULL,
    payload jsonb NOT NULL DEFAULT '{}',
    created_at timestamptz NOT NULL DEFAULT now(),
    sent_at timestamptz
);
CREATE INDEX outbox_pending ON outbox (created_at, id) WHERE sent_at IS NULL;

CREATE TABLE webhook_deliveries (
    provider text NOT NULL,
    delivery_id text NOT NULL,
    repository_id uuid NOT NULL REFERENCES repositories(id),
    received_at timestamptz NOT NULL DEFAULT now(),
    processed_at timestamptz,
    PRIMARY KEY (provider, delivery_id)
);

CREATE TABLE audit_events (
    id uuid PRIMARY KEY,
    organization_id uuid NOT NULL REFERENCES organizations(id),
    actor_id uuid REFERENCES users(id),
    action text NOT NULL,
    entity_id uuid NOT NULL,
    before_ref text,
    after_ref text,
    operation_id uuid,
    result text NOT NULL,
    timestamp timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (organization_id, operation_id) REFERENCES git_operations(organization_id, id)
);

CREATE FUNCTION reject_audit_mutation() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  RAISE EXCEPTION 'audit_events is append-only';
END;
$$;
CREATE TRIGGER audit_append_only BEFORE UPDATE OR DELETE ON audit_events
  FOR EACH ROW EXECUTE FUNCTION reject_audit_mutation();
CREATE TRIGGER audit_no_truncate BEFORE TRUNCATE ON audit_events
  FOR EACH STATEMENT EXECUTE FUNCTION reject_audit_mutation();
