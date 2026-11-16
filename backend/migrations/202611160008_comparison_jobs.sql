-- Derived, actor-private read jobs. No Markdown or binary content is persisted.
CREATE TABLE comparison_snapshots (
  id uuid PRIMARY KEY,
  organization_id uuid NOT NULL,
  proposal_id uuid NOT NULL,
  repository_id uuid NOT NULL,
  actor_id uuid NOT NULL REFERENCES users(id),
  job_id uuid NOT NULL UNIQUE,
  idempotency_key text NOT NULL CHECK(length(idempotency_key) BETWEEN 1 AND 200),
  request_hash text NOT NULL CHECK(request_hash ~ '^[a-f0-9]{64}$'),
  branch text NOT NULL,
  recorded_head text NOT NULL,
  recorded_base text NOT NULL,
  proposal_version bigint NOT NULL CHECK(proposal_version>0),
  proposal_state text NOT NULL,
  remote_id text NOT NULL,
  installation_id text NOT NULL,
  selected_head text NOT NULL,
  selected_target text,
  state text NOT NULL DEFAULT 'PENDING' CHECK(state IN ('PENDING','READY','FAILED')),
  metadata jsonb CHECK(jsonb_typeof(metadata)='object' AND octet_length(metadata::text)<=4194304),
  change_count integer NOT NULL DEFAULT 0 CHECK(change_count BETWEEN 0 AND 40000),
  error_code text CHECK(error_code ~ '^[A-Z0-9_]{1,100}$'),
  created_at timestamptz NOT NULL DEFAULT now(),
  completed_at timestamptz,
  UNIQUE(organization_id,id),
  UNIQUE(organization_id,actor_id,idempotency_key),
  CHECK((state='READY')=(metadata IS NOT NULL)),
  CHECK((state='PENDING')=(completed_at IS NULL)),
  CHECK(state='READY' OR change_count=0),
  FOREIGN KEY(organization_id,proposal_id) REFERENCES proposals(organization_id,id),
  FOREIGN KEY(organization_id,repository_id) REFERENCES repositories(organization_id,id),
  FOREIGN KEY(organization_id,job_id) REFERENCES jobs(organization_id,id)
);
CREATE INDEX comparison_actor_pending ON comparison_snapshots(organization_id,actor_id,created_at) WHERE state='PENDING';
CREATE INDEX comparison_actor_recent ON comparison_snapshots(organization_id,actor_id,created_at);
CREATE INDEX comparison_job_claim ON jobs(available_at,created_at,id) WHERE kind='COMPARE_PROPOSAL' AND state IN ('QUEUED','RUNNING');
CREATE TABLE comparison_files (
  organization_id uuid NOT NULL,
  snapshot_id uuid NOT NULL,
  ordinal integer NOT NULL CHECK(ordinal BETWEEN 0 AND 39999),
  file_id text NOT NULL CHECK(file_id ~ '^[a-f0-9]{64}$'),
  change jsonb NOT NULL CHECK(jsonb_typeof(change)='object' AND octet_length(change::text)<=8192 AND change->>'id' IS NOT DISTINCT FROM file_id),
  PRIMARY KEY(snapshot_id,ordinal),
  UNIQUE(snapshot_id,file_id),
  FOREIGN KEY(organization_id,snapshot_id) REFERENCES comparison_snapshots(organization_id,id)
);
CREATE FUNCTION comparison_file_pending() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE current_state text;
BEGIN
  IF TG_OP='UPDATE' THEN RAISE EXCEPTION 'comparison file rows are immutable'; END IF;
  IF TG_OP='DELETE' THEN
    SELECT state INTO current_state FROM comparison_snapshots WHERE id=OLD.snapshot_id AND organization_id=OLD.organization_id FOR SHARE;
  ELSE
    SELECT state INTO current_state FROM comparison_snapshots WHERE id=NEW.snapshot_id AND organization_id=NEW.organization_id FOR SHARE;
  END IF;
  IF current_state IS DISTINCT FROM 'PENDING' THEN RAISE EXCEPTION 'comparison files require a pending publication'; END IF;
  IF TG_OP='DELETE' THEN RETURN OLD; END IF;
  RETURN NEW;
END;
$$;
CREATE TRIGGER comparison_file_guard BEFORE INSERT OR UPDATE OR DELETE ON comparison_files FOR EACH ROW EXECUTE FUNCTION comparison_file_pending();
CREATE FUNCTION comparison_intent_immutable() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE count_files bigint; first_ordinal integer; last_ordinal integer;
BEGIN
  IF TG_OP='DELETE' THEN RAISE EXCEPTION 'comparison identity cannot be deleted'; END IF;
  IF OLD.state<>'PENDING' THEN RAISE EXCEPTION 'terminal comparison is immutable'; END IF;
  IF ROW(NEW.id,NEW.organization_id,NEW.proposal_id,NEW.repository_id,NEW.actor_id,NEW.job_id,NEW.idempotency_key,NEW.request_hash,NEW.branch,NEW.recorded_head,NEW.recorded_base,NEW.proposal_version,NEW.proposal_state,NEW.remote_id,NEW.installation_id,NEW.selected_head,NEW.selected_target,NEW.created_at)
    IS DISTINCT FROM ROW(OLD.id,OLD.organization_id,OLD.proposal_id,OLD.repository_id,OLD.actor_id,OLD.job_id,OLD.idempotency_key,OLD.request_hash,OLD.branch,OLD.recorded_head,OLD.recorded_base,OLD.proposal_version,OLD.proposal_state,OLD.remote_id,OLD.installation_id,OLD.selected_head,OLD.selected_target,OLD.created_at)
  THEN RAISE EXCEPTION 'comparison intent is immutable'; END IF;
  IF NEW.state='READY' THEN
    SELECT count(*),min(ordinal),max(ordinal) INTO count_files,first_ordinal,last_ordinal FROM comparison_files WHERE snapshot_id=NEW.id AND organization_id=NEW.organization_id;
    IF count_files<>NEW.change_count OR (count_files>0 AND (first_ordinal<>0 OR last_ordinal<>count_files-1))
      OR (NEW.metadata->>'changes_total')::integer IS DISTINCT FROM NEW.change_count
      OR NEW.metadata->>'head_sha' IS DISTINCT FROM NEW.selected_head
      OR (NEW.selected_target IS NOT NULL AND NEW.metadata->>'target_sha' IS DISTINCT FROM NEW.selected_target)
      OR NEW.metadata->>'proposal_id' IS DISTINCT FROM NEW.proposal_id::text
      OR NEW.metadata->>'repository_id' IS DISTINCT FROM NEW.repository_id::text
      OR (NEW.metadata->>'proposal_version')::bigint IS DISTINCT FROM NEW.proposal_version
    THEN RAISE EXCEPTION 'comparison publication is incomplete'; END IF;
  END IF;
  RETURN NEW;
END;
$$;
CREATE TRIGGER comparison_intent_guard BEFORE UPDATE OR DELETE ON comparison_snapshots FOR EACH ROW EXECUTE FUNCTION comparison_intent_immutable();
