-- One unresolved Git mutation per proposal, including an UNKNOWN operation whose
-- worker exhausted retries. A failed job alone must never unlock another write.
CREATE UNIQUE INDEX proposal_active_git_mutation ON git_operations(proposal_id)
  WHERE proposal_id IS NOT NULL
    AND kind IN ('COMMIT','SUBMIT','IMPORT_APPLY','REFRESH_BASE','MERGE','REVERT')
    AND state IN ('PENDING','RUNNING','UNKNOWN');
CREATE UNIQUE INDEX proposal_save_job ON jobs(operation_id) WHERE kind='SAVE_PROPOSAL';
CREATE INDEX proposal_save_claim ON jobs(available_at,created_at,id)
  WHERE kind='SAVE_PROPOSAL' AND state IN ('QUEUED','RUNNING');

CREATE TABLE proposal_saves (
    operation_id uuid PRIMARY KEY,
    organization_id uuid NOT NULL,
    proposal_id uuid NOT NULL,
    repository_id uuid NOT NULL,
    actor_id uuid NOT NULL,
    branch text NOT NULL,
    remote_id text NOT NULL,
    installation_id text NOT NULL,
    expected_version bigint NOT NULL CHECK (expected_version>0),
    expected_head text NOT NULL CHECK (expected_head ~ '^[a-f0-9]{40,128}$' AND length(expected_head)%2=0),
    -- Private pending input; never included in job/status/audit payloads.
    request jsonb CHECK (jsonb_typeof(request)='object' AND octet_length(request::text)<=67108864),
    manifest jsonb CHECK (jsonb_typeof(manifest)='object' AND octet_length(manifest::text)<=1048576),
    manifest_hash text CHECK (manifest_hash ~ '^[a-f0-9]{64}$'),
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK ((manifest IS NULL)=(manifest_hash IS NULL)),
    FOREIGN KEY (organization_id,operation_id) REFERENCES git_operations(organization_id,id),
    FOREIGN KEY (organization_id,proposal_id) REFERENCES proposals(organization_id,id),
    FOREIGN KEY (organization_id,repository_id) REFERENCES repositories(organization_id,id),
    FOREIGN KEY (organization_id,actor_id) REFERENCES memberships(organization_id,user_id)
);
CREATE INDEX proposal_save_private_payload ON proposal_saves(created_at,operation_id) WHERE request IS NOT NULL;

CREATE FUNCTION preserve_save_intent() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE operation_state text;
BEGIN
  IF TG_OP='DELETE' THEN RAISE EXCEPTION 'save evidence cannot be deleted'; END IF;
  IF ROW(NEW.operation_id,NEW.organization_id,NEW.proposal_id,NEW.repository_id,
    NEW.actor_id,NEW.branch,NEW.remote_id,NEW.installation_id,NEW.expected_version,NEW.expected_head,NEW.created_at)
    IS DISTINCT FROM ROW(OLD.operation_id,OLD.organization_id,OLD.proposal_id,OLD.repository_id,
    OLD.actor_id,OLD.branch,OLD.remote_id,OLD.installation_id,OLD.expected_version,OLD.expected_head,OLD.created_at)
  THEN RAISE EXCEPTION 'save scope is immutable'; END IF;
  SELECT state INTO operation_state FROM git_operations WHERE id=NEW.operation_id;
  IF NEW.manifest IS DISTINCT FROM OLD.manifest OR NEW.manifest_hash IS DISTINCT FROM OLD.manifest_hash THEN
    IF OLD.manifest IS NOT NULL OR NEW.manifest IS NULL OR operation_state NOT IN ('RUNNING','SUCCEEDED')
    THEN RAISE EXCEPTION 'save manifest is immutable after the write boundary'; END IF;
  END IF;
  IF NEW.request IS DISTINCT FROM OLD.request THEN
    IF NEW.request IS NOT NULL OR operation_state NOT IN ('SUCCEEDED','FAILED')
    THEN RAISE EXCEPTION 'pending save source can only be purged after a definite result'; END IF;
  END IF;
  RETURN NEW;
END;
$$;
CREATE TRIGGER save_intent_immutable BEFORE UPDATE OR DELETE ON proposal_saves
  FOR EACH ROW EXECUTE FUNCTION preserve_save_intent();
