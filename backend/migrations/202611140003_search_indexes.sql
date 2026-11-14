-- Entirely derived from immutable Git snapshots; safe to delete and rebuild.
-- Text payloads are shared only within an organization and normalizer version.
ALTER TABLE jobs ADD CONSTRAINT jobs_organization_identity UNIQUE (organization_id, id);

CREATE TABLE search_index_texts (
    organization_id uuid NOT NULL REFERENCES organizations(id),
    normalizer_version integer NOT NULL CHECK (normalizer_version > 0),
    text_hash text NOT NULL CHECK (text_hash ~ '^[0-9a-f]{64}$'),
    original_text text NOT NULL CHECK (octet_length(original_text) <= 2097152),
    normalized_text text NOT NULL CHECK (octet_length(normalized_text) <= 8388608),
    source_map bytea NOT NULL CHECK (octet_length(source_map) BETWEEN 4 AND 58720260),
    original_bytes integer GENERATED ALWAYS AS (octet_length(original_text)) STORED,
    normalized_bytes integer GENERATED ALWAYS AS (octet_length(normalized_text)) STORED,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (organization_id, normalizer_version, text_hash)
);

CREATE TABLE search_index_snapshots (
    organization_id uuid NOT NULL,
    repository_id uuid NOT NULL,
    snapshot_sha text NOT NULL CHECK (snapshot_sha ~ '^[0-9a-f]{40,128}$' AND length(snapshot_sha) % 2 = 0),
    normalizer_version integer NOT NULL CHECK (normalizer_version > 0),
    job_id uuid NOT NULL,
    state text NOT NULL DEFAULT 'PENDING' CHECK (state IN ('PENDING', 'READY', 'FAILED')),
    manifest_hash text CHECK (manifest_hash ~ '^[0-9a-f]{64}$'),
    document_count integer CHECK (document_count BETWEEN 0 AND 1000),
    block_count integer CHECK (block_count >= 0),
    original_bytes bigint CHECK (original_bytes BETWEEN 0 AND 20971520),
    normalized_bytes bigint CHECK (normalized_bytes BETWEEN 0 AND 83886080),
    error_code text,
    created_at timestamptz NOT NULL DEFAULT now(),
    ready_at timestamptz,
    PRIMARY KEY (repository_id, snapshot_sha, normalizer_version),
    UNIQUE (organization_id, repository_id, snapshot_sha, normalizer_version),
    FOREIGN KEY (organization_id, repository_id) REFERENCES repositories(organization_id, id),
    FOREIGN KEY (organization_id, job_id) REFERENCES jobs(organization_id, id),
    CHECK (state <> 'READY' OR (manifest_hash IS NOT NULL AND document_count IS NOT NULL
      AND block_count IS NOT NULL AND original_bytes IS NOT NULL AND normalized_bytes IS NOT NULL
      AND ready_at IS NOT NULL AND error_code IS NULL))
);

CREATE FUNCTION protect_search_text() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF NEW IS DISTINCT FROM OLD THEN RAISE EXCEPTION 'search text payloads are immutable'; END IF;
  RETURN NEW;
END;
$$;
CREATE TRIGGER search_text_immutable BEFORE UPDATE ON search_index_texts
  FOR EACH ROW EXECUTE FUNCTION protect_search_text();

CREATE TABLE search_index_blocks (
    organization_id uuid NOT NULL,
    repository_id uuid NOT NULL,
    snapshot_sha text NOT NULL,
    normalizer_version integer NOT NULL,
    book_order integer NOT NULL CHECK (book_order >= 0),
    document_id text NOT NULL CHECK (length(document_id) BETWEEN 1 AND 120),
    section_id text NOT NULL CHECK (length(section_id) <= 120),
    block_id text NOT NULL CHECK (length(block_id) BETWEEN 1 AND 120),
    text_hash text NOT NULL,
    PRIMARY KEY (repository_id, snapshot_sha, normalizer_version, book_order),
    UNIQUE (repository_id, snapshot_sha, normalizer_version, document_id, block_id),
    FOREIGN KEY (organization_id, repository_id, snapshot_sha, normalizer_version)
      REFERENCES search_index_snapshots(organization_id, repository_id, snapshot_sha, normalizer_version) ON DELETE CASCADE,
    FOREIGN KEY (organization_id, normalizer_version, text_hash)
      REFERENCES search_index_texts(organization_id, normalizer_version, text_hash)
);
CREATE INDEX search_blocks_document ON search_index_blocks
  (repository_id, snapshot_sha, normalizer_version, document_id, block_id);

-- A READY marker is published only with a complete, contiguous block set and
-- matching byte totals. Readers cannot mistake a partial write for zero matches.
CREATE FUNCTION validate_ready_search_index() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE actual_blocks bigint; actual_documents bigint; first_order integer; last_order integer;
        actual_original bigint; actual_normalized bigint;
BEGIN
  IF TG_OP = 'UPDATE' AND OLD.state = 'READY' AND NEW IS DISTINCT FROM OLD THEN
    RAISE EXCEPTION 'ready search snapshots are immutable; delete and rebuild';
  END IF;
  IF NEW.state = 'READY' THEN
    SELECT count(*), count(DISTINCT b.document_id), min(b.book_order), max(b.book_order),
           coalesce(sum(t.original_bytes), 0), coalesce(sum(t.normalized_bytes), 0)
    INTO actual_blocks, actual_documents, first_order, last_order, actual_original, actual_normalized
    FROM search_index_blocks b JOIN search_index_texts t
      ON t.organization_id = b.organization_id AND t.normalizer_version = b.normalizer_version AND t.text_hash = b.text_hash
    WHERE b.repository_id = NEW.repository_id AND b.snapshot_sha = NEW.snapshot_sha AND b.normalizer_version = NEW.normalizer_version;
    IF actual_blocks <> NEW.block_count OR actual_documents > NEW.document_count
       OR actual_original <> NEW.original_bytes OR actual_normalized <> NEW.normalized_bytes
       OR (actual_blocks > 0 AND (first_order <> 0 OR last_order <> actual_blocks - 1)) THEN
      RAISE EXCEPTION 'search snapshot is incomplete';
    END IF;
  END IF;
  RETURN NEW;
END;
$$;
CREATE TRIGGER search_snapshot_ready BEFORE INSERT OR UPDATE ON search_index_snapshots
  FOR EACH ROW EXECUTE FUNCTION validate_ready_search_index();

CREATE FUNCTION protect_ready_search_blocks() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE parent_state text;
BEGIN
  IF TG_OP <> 'INSERT' THEN
    SELECT state INTO parent_state FROM search_index_snapshots s
      WHERE s.repository_id = OLD.repository_id AND s.snapshot_sha = OLD.snapshot_sha
        AND s.normalizer_version = OLD.normalizer_version FOR SHARE;
    IF parent_state = 'READY' THEN RAISE EXCEPTION 'ready search blocks are immutable'; END IF;
  END IF;
  IF TG_OP <> 'DELETE' THEN
    SELECT state INTO parent_state FROM search_index_snapshots s
      WHERE s.repository_id = NEW.repository_id AND s.snapshot_sha = NEW.snapshot_sha
        AND s.normalizer_version = NEW.normalizer_version FOR SHARE;
    IF parent_state = 'READY' THEN RAISE EXCEPTION 'ready search blocks are immutable'; END IF;
  END IF;
  IF TG_OP = 'DELETE' THEN RETURN OLD; END IF;
  RETURN NEW;
END;
$$;
CREATE TRIGGER search_blocks_immutable BEFORE INSERT OR UPDATE OR DELETE ON search_index_blocks
  FOR EACH ROW EXECUTE FUNCTION protect_ready_search_blocks();
