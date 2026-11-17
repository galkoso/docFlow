-- A publishing row can never commit: finalization seals complete metadata,
-- and deferred guards require the result, proposal pointer and job outcome.
ALTER TABLE review_submissions ADD CONSTRAINT review_submission_org_operation UNIQUE(organization_id,operation_id);
CREATE TABLE review_snapshots (
  id uuid PRIMARY KEY,
  organization_id uuid NOT NULL,
  proposal_id uuid NOT NULL,
  repository_id uuid NOT NULL,
  created_operation uuid NOT NULL,
  created_by uuid NOT NULL REFERENCES users(id),
  state text NOT NULL DEFAULT 'PUBLISHING' CHECK(state IN ('PUBLISHING','READY')),
  fingerprint text NOT NULL CHECK(fingerprint~'^[a-f0-9]{64}$'),
  plan_fingerprint text NOT NULL CHECK(plan_fingerprint~'^[a-f0-9]{64}$'),
  evidence_digest text NOT NULL CHECK(evidence_digest~'^[a-f0-9]{64}$'),
  media_validation_version integer NOT NULL CHECK(media_validation_version>0),
  head_sha text NOT NULL CHECK(head_sha~'^[a-f0-9]{40,128}$' AND length(head_sha)%2=0),
  target_sha text NOT NULL CHECK(target_sha~'^[a-f0-9]{40,128}$' AND length(target_sha)%2=0),
  merge_base_sha text NOT NULL CHECK(merge_base_sha~'^[a-f0-9]{40,128}$' AND length(merge_base_sha)%2=0),
  recorded_base_sha text NOT NULL CHECK(recorded_base_sha~'^[a-f0-9]{40,128}$' AND length(recorded_base_sha)%2=0),
  prepared_version bigint NOT NULL CHECK(prepared_version>0),
  configuration_version bigint NOT NULL CHECK(configuration_version>0),
  policy_version bigint NOT NULL CHECK(policy_version>0),
  branch text NOT NULL,
  remote_id text NOT NULL,
  installation_id text NOT NULL,
  proposal_number bigint NOT NULL CHECK(proposal_number>0),
  title text NOT NULL CHECK(length(title) BETWEEN 3 AND 120),
  delivery_status text NOT NULL CHECK(delivery_status IN ('PLANNED','IMPLEMENTED','RELEASED')),
  metadata jsonb NOT NULL CHECK(jsonb_typeof(metadata)='object' AND octet_length(metadata::text)<=4194304
    AND metadata-ARRAY['schema_version','summary','context','assignments','validation']='{}'::jsonb),
  file_count integer NOT NULL CHECK(file_count BETWEEN 1 AND 40000),
  file_ids jsonb NOT NULL CHECK(jsonb_typeof(file_ids)='array' AND jsonb_array_length(file_ids)=file_count AND octet_length(file_ids::text)<=4194304),
  media_count integer NOT NULL CHECK(media_count BETWEEN 0 AND 20000),
  media_required jsonb NOT NULL CHECK(jsonb_typeof(media_required)='array' AND jsonb_array_length(media_required)=media_count AND octet_length(media_required::text)<=4194304),
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  created_transaction bigint NOT NULL DEFAULT txid_current(),
  UNIQUE(organization_id,proposal_id,id),
  FOREIGN KEY(organization_id,proposal_id) REFERENCES proposals(organization_id,id),
  FOREIGN KEY(organization_id,repository_id) REFERENCES repositories(organization_id,id),
  FOREIGN KEY(organization_id,created_operation) REFERENCES review_submissions(organization_id,operation_id),
  FOREIGN KEY(organization_id,proposal_id,configuration_version) REFERENCES proposal_review_configurations(organization_id,proposal_id,version)
);
CREATE INDEX review_snapshot_history ON review_snapshots(organization_id,proposal_id,created_at DESC,id);
CREATE TABLE review_snapshot_files (
  organization_id uuid NOT NULL,
  proposal_id uuid NOT NULL,
  snapshot_id uuid NOT NULL,
  ordinal integer NOT NULL CHECK(ordinal BETWEEN 0 AND 39999),
  file_id text NOT NULL CHECK(file_id~'^[a-f0-9]{64}$'),
  change jsonb NOT NULL CHECK(jsonb_typeof(change)='object' AND octet_length(change::text)<=8192 AND change->>'id' IS NOT DISTINCT FROM file_id),
  PRIMARY KEY(snapshot_id,ordinal),UNIQUE(snapshot_id,file_id),
  FOREIGN KEY(organization_id,proposal_id,snapshot_id) REFERENCES review_snapshots(organization_id,proposal_id,id)
);
CREATE TABLE review_snapshot_media (
  organization_id uuid NOT NULL,
  proposal_id uuid NOT NULL,
  snapshot_id uuid NOT NULL,
  ordinal integer NOT NULL CHECK(ordinal BETWEEN 0 AND 19999),
  path text NOT NULL CHECK(path~'^assets/[a-f0-9]{64}\.(png|jpg|jpeg|webp)$'),
  receipt jsonb NOT NULL CHECK(jsonb_typeof(receipt)='object' AND octet_length(receipt::text)<=2048
    AND receipt-ARRAY['path','blob_oid','bytes','source_sha256','display_sha256','display_width','display_height']='{}'::jsonb
    AND receipt ?& ARRAY['path','blob_oid','bytes','source_sha256','display_sha256','display_width','display_height']
    AND jsonb_typeof(receipt->'blob_oid')='string' AND jsonb_typeof(receipt->'display_sha256')='string'
    AND jsonb_typeof(receipt->'bytes')='number' AND jsonb_typeof(receipt->'display_width')='number' AND jsonb_typeof(receipt->'display_height')='number'
    AND receipt->>'path' IS NOT DISTINCT FROM path
    AND receipt->>'source_sha256' IS NOT DISTINCT FROM split_part(substr(path,8),'.',1)
    AND receipt->>'blob_oid'~'^([a-f0-9]{40}|[a-f0-9]{64})$'
    AND receipt->>'display_sha256'~'^[a-f0-9]{64}$'
    AND (receipt->>'bytes')::bigint BETWEEN 0 AND 5242880
    AND (receipt->>'display_width')::integer BETWEEN 1 AND 2048
    AND (receipt->>'display_height')::integer BETWEEN 1 AND 2048),
  PRIMARY KEY(snapshot_id,ordinal),UNIQUE(snapshot_id,path),
  FOREIGN KEY(organization_id,proposal_id,snapshot_id) REFERENCES review_snapshots(organization_id,proposal_id,id)
);
ALTER TABLE proposals ADD COLUMN active_review_snapshot_id uuid;
ALTER TABLE proposals ADD CONSTRAINT proposal_active_review_snapshot FOREIGN KEY(organization_id,id,active_review_snapshot_id)
  REFERENCES review_snapshots(organization_id,proposal_id,id);
CREATE TABLE review_submission_results (
  operation_id uuid PRIMARY KEY,
  organization_id uuid NOT NULL,
  proposal_id uuid NOT NULL,
  snapshot_id uuid NOT NULL,
  applied_version bigint NOT NULL CHECK(applied_version>0),
  activated boolean NOT NULL,
  fence_worker uuid NOT NULL,
  fence_attempt integer NOT NULL CHECK(fence_attempt>0),
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  created_transaction bigint NOT NULL DEFAULT txid_current(),
  FOREIGN KEY(organization_id,operation_id) REFERENCES review_submissions(organization_id,operation_id),
  FOREIGN KEY(organization_id,proposal_id,snapshot_id) REFERENCES review_snapshots(organization_id,proposal_id,id)
);
CREATE FUNCTION review_snapshot_insert_guard() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE p proposals%ROWTYPE; s review_submissions%ROWTYPE; h proposal_review_configurations%ROWTYPE; current_policy bigint;
BEGIN
  SELECT * INTO s FROM review_submissions WHERE operation_id=NEW.created_operation AND organization_id=NEW.organization_id;
  SELECT * INTO p FROM proposals WHERE id=NEW.proposal_id AND organization_id=NEW.organization_id FOR UPDATE;
  SELECT * INTO h FROM proposal_review_configurations WHERE organization_id=NEW.organization_id AND proposal_id=NEW.proposal_id AND version=NEW.configuration_version;
  SELECT policy_version INTO current_policy FROM organizations WHERE id=NEW.organization_id FOR SHARE;
  IF NEW.state<>'PUBLISHING' OR s.operation_id IS NULL OR p.id IS NULL OR h.proposal_id IS NULL
      OR s.proposal_id<>NEW.proposal_id OR s.repository_id<>NEW.repository_id OR s.actor_id<>NEW.created_by
      OR s.expected_head<>NEW.head_sha OR s.expected_target<>NEW.target_sha OR s.expected_version<>NEW.prepared_version
      OR s.configuration_version<>NEW.configuration_version OR s.policy_version<>NEW.policy_version
      OR s.remote_id<>NEW.remote_id OR s.installation_id<>NEW.installation_id OR s.branch<>NEW.branch
      OR p.version<>NEW.prepared_version OR p.head_sha IS DISTINCT FROM NEW.head_sha OR p.base_sha IS DISTINCT FROM NEW.recorded_base_sha
      OR p.repository_id<>NEW.repository_id OR p.branch<>NEW.branch OR p.number<>NEW.proposal_number OR p.title<>NEW.title
      OR p.delivery_status<>NEW.delivery_status OR p.review_configuration_version<>NEW.configuration_version
      OR current_policy<>NEW.policy_version OR p.state NOT IN ('DRAFT','IN_REVIEW','CHANGES_REQUESTED','APPROVED')
      OR NEW.metadata->>'schema_version' IS DISTINCT FROM '1'
      OR NEW.metadata#>>'{summary,fingerprint}' IS DISTINCT FROM NEW.plan_fingerprint
      OR NEW.metadata#>>'{summary,proposal_id}' IS DISTINCT FROM NEW.proposal_id::text
      OR NEW.metadata#>>'{summary,repository_id}' IS DISTINCT FROM NEW.repository_id::text
      OR NEW.metadata#>>'{summary,branch}' IS DISTINCT FROM NEW.branch
      OR NEW.metadata#>>'{summary,head_sha}' IS DISTINCT FROM NEW.head_sha
      OR NEW.metadata#>>'{summary,target_sha}' IS DISTINCT FROM NEW.target_sha
      OR NEW.metadata#>>'{summary,merge_base_sha}' IS DISTINCT FROM NEW.merge_base_sha
      OR NEW.metadata#>>'{summary,recorded_base_sha}' IS DISTINCT FROM NEW.recorded_base_sha
      OR (NEW.metadata#>>'{summary,expected_proposal_version}')::bigint IS DISTINCT FROM NEW.prepared_version
      OR (NEW.metadata#>>'{summary,configuration_version}')::bigint IS DISTINCT FROM NEW.configuration_version
      OR (NEW.metadata#>>'{summary,policy_version}')::bigint IS DISTINCT FROM NEW.policy_version
      OR (NEW.metadata#>>'{summary,changed_files}')::integer IS DISTINCT FROM NEW.file_count
      OR (NEW.metadata#>>'{summary,media_checks_required}')::integer IS DISTINCT FROM NEW.media_count
      OR NEW.metadata#>>'{context,author_id}' IS DISTINCT FROM p.author_id::text
      OR NEW.metadata#>>'{context,owner_id}' IS DISTINCT FROM p.owner_id::text
      OR NEW.metadata#>>'{context,team_id}' IS DISTINCT FROM p.team_id::text
      OR NEW.metadata#>>'{context,description}' IS DISTINCT FROM p.description
      OR NEW.metadata#>>'{context,ui_impact}' IS DISTINCT FROM h.ui_impact
      OR NEW.metadata#>>'{summary,ui_impact}' IS DISTINCT FROM h.ui_impact
      OR NEW.metadata#>>'{context,ui_impact_reason}' IS DISTINCT FROM h.ui_impact_reason
      OR NEW.metadata->'assignments' IS DISTINCT FROM h.assignments
      OR NEW.metadata#>'{validation,complete}' IS DISTINCT FROM 'true'::jsonb
      OR NEW.metadata#>'{validation,blockers_total}' IS DISTINCT FROM '0'::jsonb
  THEN RAISE EXCEPTION 'review snapshot does not match its verified publication scope'; END IF;
  NEW.created_transaction:=txid_current();RETURN NEW;
END $$;
CREATE TRIGGER review_snapshot_insert BEFORE INSERT ON review_snapshots FOR EACH ROW EXECUTE FUNCTION review_snapshot_insert_guard();
CREATE FUNCTION review_snapshot_child_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF TG_OP<>'INSERT' THEN RAISE EXCEPTION 'review snapshot evidence is immutable'; END IF;
  IF NOT EXISTS(SELECT 1 FROM review_snapshots WHERE id=NEW.snapshot_id AND organization_id=NEW.organization_id
      AND proposal_id=NEW.proposal_id AND state='PUBLISHING' AND created_transaction=txid_current() FOR SHARE) THEN
    RAISE EXCEPTION 'review evidence requires its publishing transaction';
  END IF;
  RETURN NEW;
END $$;
CREATE TRIGGER review_snapshot_file_guard BEFORE INSERT OR UPDATE OR DELETE ON review_snapshot_files FOR EACH ROW EXECUTE FUNCTION review_snapshot_child_guard();
CREATE TRIGGER review_snapshot_media_guard BEFORE INSERT OR UPDATE OR DELETE ON review_snapshot_media FOR EACH ROW EXECUTE FUNCTION review_snapshot_child_guard();
CREATE FUNCTION review_snapshot_seal() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE ids jsonb; receipts jsonb; first_media integer; last_media integer; count_media bigint;
BEGIN
  IF TG_OP='DELETE' THEN RAISE EXCEPTION 'review snapshot history is immutable'; END IF;
  IF OLD.state<>'PUBLISHING' OR NEW.state<>'READY' OR OLD.created_transaction<>txid_current()
      OR (to_jsonb(NEW)-'state') IS DISTINCT FROM (to_jsonb(OLD)-'state') THEN
    RAISE EXCEPTION 'review snapshot can only be sealed once';
  END IF;
  SELECT jsonb_agg(file_id ORDER BY ordinal) INTO ids FROM review_snapshot_files WHERE snapshot_id=NEW.id;
  IF ids IS DISTINCT FROM NEW.file_ids OR EXISTS(SELECT 1 FROM review_snapshot_files WHERE snapshot_id=NEW.id AND ordinal>=NEW.file_count)
      OR (SELECT min(ordinal) FROM review_snapshot_files WHERE snapshot_id=NEW.id)<>0
      OR (SELECT max(ordinal) FROM review_snapshot_files WHERE snapshot_id=NEW.id)<>NEW.file_count-1 THEN
    RAISE EXCEPTION 'review snapshot files are incomplete';
  END IF;
  SELECT coalesce(jsonb_agg(jsonb_build_object('path',path,'blob_oid',receipt->>'blob_oid','bytes',(receipt->>'bytes')::bigint) ORDER BY ordinal),'[]'::jsonb),count(*),min(ordinal),max(ordinal)
    INTO receipts,count_media,first_media,last_media FROM review_snapshot_media WHERE snapshot_id=NEW.id;
  IF receipts<>NEW.media_required OR count_media<>NEW.media_count OR (count_media>0 AND (first_media<>0 OR last_media<>count_media-1)) THEN
    RAISE EXCEPTION 'review snapshot media is incomplete';
  END IF;
  RETURN NEW;
END $$;
CREATE TRIGGER review_snapshot_seal BEFORE UPDATE OR DELETE ON review_snapshots FOR EACH ROW EXECUTE FUNCTION review_snapshot_seal();
CREATE FUNCTION review_publication_insert_guard() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE s review_submissions%ROWTYPE; h review_snapshots%ROWTYPE; p proposals%ROWTYPE;
BEGIN
  SELECT * INTO s FROM review_submissions WHERE operation_id=NEW.operation_id AND organization_id=NEW.organization_id;
  SELECT * INTO h FROM review_snapshots WHERE id=NEW.snapshot_id AND proposal_id=NEW.proposal_id AND organization_id=NEW.organization_id;
  SELECT * INTO p FROM proposals WHERE id=NEW.proposal_id AND organization_id=NEW.organization_id FOR UPDATE;
  IF s.operation_id IS NULL OR h.id IS NULL OR h.state<>'READY' OR s.proposal_id<>NEW.proposal_id
      OR p.id IS NULL OR p.version<>s.expected_version OR p.head_sha IS DISTINCT FROM s.expected_head
      OR p.base_sha IS DISTINCT FROM h.recorded_base_sha OR p.review_configuration_version<>h.configuration_version
      OR p.state NOT IN ('DRAFT','IN_REVIEW','CHANGES_REQUESTED','APPROVED')
      OR s.repository_id<>h.repository_id OR s.expected_head<>h.head_sha OR s.expected_target<>h.target_sha
      OR s.configuration_version<>h.configuration_version OR s.policy_version<>h.policy_version
      OR s.branch<>h.branch OR s.remote_id<>h.remote_id OR s.installation_id<>h.installation_id
      OR NEW.applied_version<>s.expected_version+(CASE WHEN NEW.activated THEN 1 ELSE 0 END)
      OR (NEW.activated AND (h.created_operation IS DISTINCT FROM NEW.operation_id OR h.created_transaction<>txid_current()))
      OR (NOT NEW.activated AND (p.active_review_snapshot_id IS DISTINCT FROM NEW.snapshot_id OR p.state NOT IN ('IN_REVIEW','APPROVED','CHANGES_REQUESTED')))
      OR NOT EXISTS(SELECT 1 FROM jobs j JOIN git_operations o ON o.id=j.operation_id AND o.organization_id=j.organization_id
        WHERE j.id=s.job_id AND o.id=s.operation_id AND o.kind='SUBMIT' AND o.state='PENDING'
        AND j.kind='SUBMIT_REVIEW' AND j.state='RUNNING' AND j.lease_owner=NEW.fence_worker
        AND j.attempts=NEW.fence_attempt AND j.leased_until>clock_timestamp() FOR SHARE OF j,o)
  THEN RAISE EXCEPTION 'review publication requires its current submission lease'; END IF;
  NEW.created_transaction:=txid_current();RETURN NEW;
END $$;
CREATE TRIGGER review_publication_insert BEFORE INSERT ON review_submission_results FOR EACH ROW EXECUTE FUNCTION review_publication_insert_guard();
CREATE TRIGGER review_publication_immutable BEFORE UPDATE OR DELETE ON review_submission_results FOR EACH ROW EXECUTE FUNCTION review_submission_immutable();
CREATE FUNCTION proposal_review_snapshot_guard() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE semantic_change boolean;
BEGIN
  semantic_change:=ROW(NEW.head_sha,NEW.target_sha,NEW.base_sha,NEW.review_configuration_version,NEW.repository_id,NEW.branch,NEW.author_id,NEW.owner_id,NEW.team_id,NEW.description,NEW.title,NEW.delivery_status)
    IS DISTINCT FROM ROW(OLD.head_sha,OLD.target_sha,OLD.base_sha,OLD.review_configuration_version,OLD.repository_id,OLD.branch,OLD.author_id,OLD.owner_id,OLD.team_id,OLD.description,OLD.title,OLD.delivery_status);
  IF NEW.active_review_snapshot_id IS NOT NULL AND NEW.active_review_snapshot_id IS DISTINCT FROM OLD.active_review_snapshot_id THEN
    IF NOT EXISTS(SELECT 1 FROM review_submission_results x JOIN review_snapshots h ON h.id=x.snapshot_id
        WHERE x.organization_id=NEW.organization_id AND x.proposal_id=NEW.id AND x.snapshot_id=NEW.active_review_snapshot_id
        AND x.created_transaction=txid_current() AND x.activated AND x.applied_version=NEW.version
        AND NEW.version=OLD.version+1 AND NEW.state='IN_REVIEW' AND OLD.state IN ('DRAFT','IN_REVIEW','CHANGES_REQUESTED','APPROVED')
        AND h.head_sha=NEW.head_sha AND h.target_sha=NEW.target_sha AND h.recorded_base_sha=NEW.base_sha
        AND h.configuration_version=NEW.review_configuration_version AND h.repository_id=NEW.repository_id
        AND h.branch=NEW.branch AND h.title=NEW.title AND h.delivery_status=NEW.delivery_status
        AND h.metadata#>>'{context,author_id}'=NEW.author_id::text AND h.metadata#>>'{context,owner_id}'=NEW.owner_id::text
        AND h.metadata#>>'{context,team_id}'=NEW.team_id::text AND h.metadata#>>'{context,description}'=NEW.description) THEN
      RAISE EXCEPTION 'review activation requires its atomic publication result';
    END IF;
  ELSIF semantic_change THEN
    NEW.active_review_snapshot_id:=NULL;
    IF OLD.state='APPROVED' AND NEW.state='APPROVED' THEN NEW.state:='IN_REVIEW'; END IF;
  END IF;
  RETURN NEW;
END $$;
CREATE TRIGGER proposal_review_snapshot_guard BEFORE UPDATE ON proposals FOR EACH ROW EXECUTE FUNCTION proposal_review_snapshot_guard();
CREATE FUNCTION review_publication_complete() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE x review_submission_results%ROWTYPE; h review_snapshots%ROWTYPE; op uuid;
BEGIN
  IF TG_TABLE_NAME='review_snapshots' THEN
    SELECT * INTO h FROM review_snapshots WHERE id=NEW.id;
    IF h.state<>'READY' OR NOT EXISTS(SELECT 1 FROM review_submission_results WHERE snapshot_id=h.id) THEN RAISE EXCEPTION 'review snapshot was not atomically published'; END IF;
    op:=h.created_operation;
  ELSE op:=NEW.operation_id; END IF;
  SELECT * INTO x FROM review_submission_results WHERE operation_id=op;
  IF x.operation_id IS NULL OR NOT EXISTS(SELECT 1 FROM review_submissions s JOIN jobs j ON j.id=s.job_id JOIN git_operations o ON o.id=s.operation_id
      JOIN proposals p ON p.id=s.proposal_id WHERE s.operation_id=op AND j.state='SUCCEEDED' AND o.state='SUCCEEDED'
      AND j.result->>'phase'='SNAPSHOT_READY' AND j.result->>'snapshot_id'=x.snapshot_id::text
      AND p.active_review_snapshot_id=x.snapshot_id AND p.version=x.applied_version)
      OR NOT EXISTS(SELECT 1 FROM outbox WHERE organization_id=x.organization_id AND entity_id=x.proposal_id
        AND event_type='REVIEW_SNAPSHOT_PUBLISHED' AND payload->>'snapshot_id'=x.snapshot_id::text AND payload->>'operation_id'=op::text)
      OR NOT EXISTS(SELECT 1 FROM audit_events WHERE organization_id=x.organization_id AND entity_id=x.proposal_id
        AND operation_id=op AND action='REVIEW_SNAPSHOT_PUBLISHED' AND after_ref=x.snapshot_id::text) THEN
    RAISE EXCEPTION 'review publication outcome is incomplete';
  END IF;
  RETURN NULL;
END $$;
CREATE CONSTRAINT TRIGGER review_snapshot_complete AFTER INSERT OR UPDATE ON review_snapshots DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION review_publication_complete();
CREATE CONSTRAINT TRIGGER review_publication_complete AFTER INSERT ON review_submission_results DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION review_publication_complete();
CREATE FUNCTION review_policy_version_monotonic() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN IF NEW.policy_version<OLD.policy_version THEN RAISE EXCEPTION 'review policy version cannot move backwards'; END IF; RETURN NEW; END $$;
CREATE TRIGGER review_policy_version_monotonic BEFORE UPDATE ON organizations FOR EACH ROW EXECUTE FUNCTION review_policy_version_monotonic();
-- A changed-and-restored provider binding must not revive an old approval.
CREATE FUNCTION review_repository_binding_invalidate() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF ROW(NEW.provider,NEW.remote_id,NEW.installation_id,NEW.default_branch)
      IS DISTINCT FROM ROW(OLD.provider,OLD.remote_id,OLD.installation_id,OLD.default_branch) THEN
    UPDATE proposals SET active_review_snapshot_id=NULL,version=version+1,
      state=CASE WHEN state='APPROVED' THEN 'IN_REVIEW' ELSE state END,updated_at=clock_timestamp()
      WHERE organization_id=NEW.organization_id AND repository_id=NEW.id AND active_review_snapshot_id IS NOT NULL;
  END IF;
  RETURN NEW;
END $$;
CREATE TRIGGER review_repository_binding_invalidate AFTER UPDATE ON repositories FOR EACH ROW EXECUTE FUNCTION review_repository_binding_invalidate();
CREATE TRIGGER review_snapshot_no_truncate BEFORE TRUNCATE ON review_snapshots FOR EACH STATEMENT EXECUTE FUNCTION review_submission_immutable();
CREATE TRIGGER review_snapshot_files_no_truncate BEFORE TRUNCATE ON review_snapshot_files FOR EACH STATEMENT EXECUTE FUNCTION review_submission_immutable();
CREATE TRIGGER review_snapshot_media_no_truncate BEFORE TRUNCATE ON review_snapshot_media FOR EACH STATEMENT EXECUTE FUNCTION review_submission_immutable();
CREATE TRIGGER review_publication_no_truncate BEFORE TRUNCATE ON review_submission_results FOR EACH STATEMENT EXECUTE FUNCTION review_submission_immutable();
