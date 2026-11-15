-- Recovery versions must not repeat after expiration/cleanup and recreation.
LOCK TABLE draft_buffers IN ACCESS EXCLUSIVE MODE;
CREATE SEQUENCE draft_buffer_version_seq;
SELECT setval('draft_buffer_version_seq', coalesce(max(version),0)+1, false) FROM draft_buffers;
ALTER TABLE draft_buffers ALTER COLUMN version SET DEFAULT nextval('draft_buffer_version_seq');
ALTER SEQUENCE draft_buffer_version_seq OWNED BY draft_buffers.version;
ALTER TABLE draft_buffers ADD COLUMN updated_at timestamptz NOT NULL DEFAULT now();
ALTER TABLE draft_buffers ADD COLUMN last_save_id uuid;
ALTER TABLE draft_buffers ADD COLUMN last_request_hash text
    CHECK (last_request_hash IS NULL OR last_request_hash ~ '^[0-9a-f]{64}$');
ALTER TABLE draft_buffers ADD CONSTRAINT draft_save_identity
    CHECK ((last_save_id IS NULL) = (last_request_hash IS NULL));
ALTER TABLE draft_buffers ADD CONSTRAINT draft_base_sha
    CHECK (base_head_sha ~ '^[0-9a-f]{40,128}$' AND length(base_head_sha)%2=0);
ALTER TABLE draft_buffers ADD CONSTRAINT draft_document_id
    CHECK (document_id ~ '^[A-Z0-9-]{1,120}$');
CREATE INDEX draft_buffers_expiry ON draft_buffers(expires_at);
