-- Dedicated PR metadata work does not acquire the content-mutation slot.
ALTER TABLE git_operations DROP CONSTRAINT git_operations_kind_check;
ALTER TABLE git_operations ADD CONSTRAINT git_operations_kind_check
  CHECK(kind IN ('CREATE','COMMIT','SUBMIT','EXPORT','IMPORT_APPLY','REFRESH_BASE','MERGE','REVERT','PR_SYNC'));
CREATE UNIQUE INDEX proposal_active_pr_sync ON git_operations(organization_id,proposal_id)
  WHERE kind='PR_SYNC' AND state IN ('PENDING','RUNNING','UNKNOWN');
CREATE UNIQUE INDEX pr_sync_job ON jobs(operation_id) WHERE kind='SYNC_REVIEW_PR';
CREATE TABLE pr_sync_intents (
  operation_id uuid PRIMARY KEY,
  job_id uuid NOT NULL UNIQUE REFERENCES jobs(id),
  source_event uuid NOT NULL UNIQUE REFERENCES outbox(id),
  source_submission uuid NOT NULL UNIQUE REFERENCES review_submission_results(operation_id),
  organization_id uuid NOT NULL,
  proposal_id uuid NOT NULL,
  repository_id uuid NOT NULL,
  actor_id uuid NOT NULL REFERENCES users(id),
  snapshot_id uuid NOT NULL,
  branch text NOT NULL,
  remote_id text NOT NULL CHECK(remote_id~'^[1-9][0-9]{0,19}$'),
  installation_id text NOT NULL CHECK(installation_id~'^[1-9][0-9]{0,19}$'),
  head_sha text NOT NULL CHECK(head_sha~'^[a-f0-9]{40,128}$' AND length(head_sha)%2=0),
  target_sha text NOT NULL CHECK(target_sha~'^[a-f0-9]{40,128}$' AND length(target_sha)%2=0),
  merge_base_sha text NOT NULL CHECK(merge_base_sha~'^[a-f0-9]{40,128}$' AND length(merge_base_sha)%2=0),
  proposal_version bigint NOT NULL CHECK(proposal_version>0),
  configuration_version bigint NOT NULL CHECK(configuration_version>0),
  policy_version bigint NOT NULL CHECK(policy_version>0),
  fingerprint text NOT NULL CHECK(fingerprint~'^[a-f0-9]{64}$'),
  evidence_digest text NOT NULL CHECK(evidence_digest~'^[a-f0-9]{64}$'),
  authority_digest text NOT NULL CHECK(authority_digest~'^[a-f0-9]{64}$'),
  request_hash text NOT NULL CHECK(request_hash~'^[a-f0-9]{64}$'),
  title text NOT NULL CHECK(length(title) BETWEEN 3 AND 120),
  body text NOT NULL CHECK(octet_length(body)<=65536),
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  FOREIGN KEY(organization_id,operation_id) REFERENCES git_operations(organization_id,id),
  FOREIGN KEY(organization_id,proposal_id) REFERENCES proposals(organization_id,id),
  FOREIGN KEY(organization_id,repository_id) REFERENCES repositories(organization_id,id),
  FOREIGN KEY(organization_id,proposal_id,snapshot_id) REFERENCES review_snapshots(organization_id,proposal_id,id),
  FOREIGN KEY(organization_id,proposal_id,configuration_version)
    REFERENCES proposal_review_configurations(organization_id,proposal_id,version)
);
CREATE INDEX pr_sync_recent_admission ON pr_sync_intents(organization_id,created_at) INCLUDE(actor_id);
CREATE FUNCTION pr_sync_insert_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF NOT EXISTS(SELECT 1 FROM git_operations o JOIN jobs j ON j.operation_id=o.id AND j.organization_id=o.organization_id
      JOIN proposals p ON p.id=o.proposal_id AND p.organization_id=o.organization_id
      JOIN repositories r ON r.id=p.repository_id AND r.organization_id=p.organization_id
      JOIN organizations g ON g.id=p.organization_id
      JOIN review_snapshots h ON h.id=p.active_review_snapshot_id AND h.organization_id=p.organization_id AND h.proposal_id=p.id
      JOIN review_submission_results x ON x.operation_id=NEW.source_submission AND x.organization_id=p.organization_id AND x.proposal_id=p.id AND x.snapshot_id=h.id
      JOIN review_submissions s ON s.operation_id=x.operation_id AND s.organization_id=x.organization_id
      JOIN git_operations so ON so.id=s.operation_id AND so.organization_id=s.organization_id
      JOIN jobs sj ON sj.id=s.job_id AND sj.operation_id=so.id AND sj.organization_id=so.organization_id
      JOIN outbox e ON e.id=NEW.source_event AND e.organization_id=p.organization_id AND e.entity_id=p.id
      WHERE o.id=NEW.operation_id AND o.organization_id=NEW.organization_id AND o.proposal_id=NEW.proposal_id
      AND o.kind='PR_SYNC' AND o.state='PENDING' AND o.actor_id=NEW.actor_id
      AND o.request_hash=NEW.request_hash AND o.expected_sha=NEW.head_sha AND o.idempotency_key=NEW.source_event::text
      AND j.id=NEW.job_id AND j.kind='SYNC_REVIEW_PR' AND j.state='QUEUED' AND j.attempts=0
      AND j.payload=jsonb_build_object('operation_id',NEW.operation_id)
      AND so.kind='SUBMIT' AND so.state='SUCCEEDED' AND so.result_sha=h.head_sha AND so.actor_id=NEW.actor_id
      AND sj.kind='SUBMIT_REVIEW' AND sj.state='SUCCEEDED'
      AND sj.result=jsonb_build_object('phase','SNAPSHOT_READY','snapshot_id',h.id)
      AND e.event_type='REVIEW_SNAPSHOT_PUBLISHED' AND e.sent_at IS NULL
      AND e.payload=jsonb_build_object('snapshot_id',h.id,'operation_id',s.operation_id,'activated',x.activated,'pr_sync_required',true,'approvals_granted',false)
      AND h.state='READY' AND h.id=NEW.snapshot_id AND h.repository_id=NEW.repository_id
      AND h.fingerprint=NEW.fingerprint AND h.evidence_digest=NEW.evidence_digest
      AND h.head_sha=NEW.head_sha AND h.target_sha=NEW.target_sha AND h.merge_base_sha=NEW.merge_base_sha
      AND h.configuration_version=NEW.configuration_version AND h.policy_version=NEW.policy_version
      AND h.branch=NEW.branch AND h.remote_id=NEW.remote_id AND h.installation_id=NEW.installation_id
      AND p.repository_id=NEW.repository_id AND p.head_sha=NEW.head_sha AND p.target_sha=NEW.target_sha
      AND p.base_sha=h.recorded_base_sha AND p.version=NEW.proposal_version AND p.title=NEW.title AND p.branch=NEW.branch
      AND p.review_configuration_version=NEW.configuration_version AND g.policy_version=NEW.policy_version
      AND p.state IN ('IN_REVIEW','CHANGES_REQUESTED','APPROVED')
      AND r.provider='GITHUB' AND r.default_branch='main'
      AND ltrim(r.remote_id,'0+')=NEW.remote_id AND ltrim(r.installation_id,'0+')=NEW.installation_id
      FOR SHARE OF o,j,p,r,g,h,x,s,so,sj,e) THEN
    RAISE EXCEPTION 'PR sync requires a current backed publication, operation and job';
  END IF;
  RETURN NEW;
END $$;
CREATE TRIGGER pr_sync_insert BEFORE INSERT ON pr_sync_intents FOR EACH ROW EXECUTE FUNCTION pr_sync_insert_guard();
CREATE TRIGGER pr_sync_immutable BEFORE UPDATE OR DELETE ON pr_sync_intents FOR EACH ROW EXECUTE FUNCTION review_submission_immutable();
CREATE TRIGGER pr_sync_no_truncate BEFORE TRUNCATE ON pr_sync_intents FOR EACH STATEMENT EXECUTE FUNCTION review_submission_immutable();
CREATE FUNCTION pr_sync_scope_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF TG_TABLE_NAME='jobs' AND OLD.kind='SYNC_REVIEW_PR' THEN
    IF TG_OP='DELETE' THEN RAISE EXCEPTION 'PR sync job is durable'; END IF;
    IF ROW(NEW.id,NEW.organization_id,NEW.operation_id,NEW.kind,NEW.payload,NEW.max_attempts,NEW.created_at)
       IS DISTINCT FROM ROW(OLD.id,OLD.organization_id,OLD.operation_id,OLD.kind,OLD.payload,OLD.max_attempts,OLD.created_at)
       OR NEW.state='SUCCEEDED' THEN RAISE EXCEPTION 'PR sync requires its dedicated journal'; END IF;
  ELSIF TG_TABLE_NAME='git_operations' AND OLD.kind='PR_SYNC' THEN
    IF TG_OP='DELETE' THEN RAISE EXCEPTION 'PR sync operation is durable'; END IF;
    IF ROW(NEW.id,NEW.organization_id,NEW.proposal_id,NEW.actor_id,NEW.kind,NEW.idempotency_key,NEW.request_hash,NEW.expected_sha,NEW.created_at)
       IS DISTINCT FROM ROW(OLD.id,OLD.organization_id,OLD.proposal_id,OLD.actor_id,OLD.kind,OLD.idempotency_key,OLD.request_hash,OLD.expected_sha,OLD.created_at)
       OR NEW.state<>'PENDING' THEN RAISE EXCEPTION 'PR sync mutation requires a fenced worker'; END IF;
  END IF;
  IF TG_OP='DELETE' THEN RETURN OLD; END IF;
  RETURN NEW;
END $$;
CREATE TRIGGER pr_sync_job_scope BEFORE UPDATE OR DELETE ON jobs FOR EACH ROW EXECUTE FUNCTION pr_sync_scope_guard();
CREATE TRIGGER pr_sync_operation_scope BEFORE UPDATE OR DELETE ON git_operations FOR EACH ROW EXECUTE FUNCTION pr_sync_scope_guard();
CREATE FUNCTION pr_sync_complete() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE op uuid;
BEGIN
  IF TG_TABLE_NAME='jobs' THEN
    IF NEW.kind<>'SYNC_REVIEW_PR' THEN RETURN NULL; END IF;
    op:=NEW.operation_id;
  ELSIF TG_TABLE_NAME='git_operations' THEN
    IF NEW.kind<>'PR_SYNC' THEN RETURN NULL; END IF;
    op:=NEW.id;
  ELSE op:=NEW.operation_id;
  END IF;
  IF NOT EXISTS(SELECT 1 FROM pr_sync_intents i JOIN jobs j ON j.id=i.job_id
      JOIN git_operations o ON o.id=i.operation_id JOIN outbox e ON e.id=i.source_event
      WHERE i.operation_id=op AND j.kind='SYNC_REVIEW_PR' AND j.operation_id=o.id
      AND o.kind='PR_SYNC' AND o.state='PENDING' AND j.state<>'SUCCEEDED'
      AND j.organization_id=i.organization_id AND o.organization_id=i.organization_id AND e.sent_at IS NOT NULL
      AND EXISTS(SELECT 1 FROM audit_events a WHERE a.organization_id=i.organization_id AND a.entity_id=i.proposal_id
        AND a.actor_id=i.actor_id AND a.operation_id=i.operation_id AND a.action='PR_SYNC_RESERVED' AND a.after_ref=i.snapshot_id::text)) THEN
    RAISE EXCEPTION 'PR sync admission is incomplete';
  END IF;
  RETURN NULL;
END $$;
CREATE CONSTRAINT TRIGGER pr_sync_job_complete AFTER INSERT OR UPDATE ON jobs DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION pr_sync_complete();
CREATE CONSTRAINT TRIGGER pr_sync_operation_complete AFTER INSERT OR UPDATE ON git_operations DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION pr_sync_complete();
CREATE CONSTRAINT TRIGGER pr_sync_intent_complete AFTER INSERT ON pr_sync_intents DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION pr_sync_complete();
CREATE FUNCTION pr_sync_event_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF EXISTS(SELECT 1 FROM pr_sync_intents WHERE source_event=OLD.id) THEN
    IF TG_OP='DELETE' THEN RAISE EXCEPTION 'delivered PR event is durable'; END IF;
    IF ROW(NEW.id,NEW.organization_id,NEW.event_type,NEW.entity_id,NEW.payload,NEW.created_at)
       IS DISTINCT FROM ROW(OLD.id,OLD.organization_id,OLD.event_type,OLD.entity_id,OLD.payload,OLD.created_at)
       OR NEW.sent_at IS NULL OR (OLD.sent_at IS NOT NULL AND NEW.sent_at IS DISTINCT FROM OLD.sent_at) THEN
      RAISE EXCEPTION 'delivered PR event is immutable';
    END IF;
  END IF;
  IF TG_OP='DELETE' THEN RETURN OLD; END IF;
  RETURN NEW;
END $$;
CREATE TRIGGER pr_sync_event_scope BEFORE UPDATE OR DELETE ON outbox FOR EACH ROW EXECUTE FUNCTION pr_sync_event_guard();
