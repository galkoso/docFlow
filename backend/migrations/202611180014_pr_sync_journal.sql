-- A write record is append-only and survives every lease/reconciliation attempt.
CREATE TABLE pr_sync_writes (
  operation_id uuid PRIMARY KEY REFERENCES pr_sync_intents(operation_id),
  kind text NOT NULL CHECK(kind IN ('CREATE','UPDATE')),
  pr_number bigint CHECK(pr_number>0),
  provider_id text CHECK(provider_id~'^[1-9][0-9]{0,19}$'),
  fence_worker uuid NOT NULL,
  fence_attempt integer NOT NULL CHECK(fence_attempt>0),
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  CHECK((kind='CREATE' AND pr_number IS NULL AND provider_id IS NULL)
     OR (kind='UPDATE' AND pr_number IS NOT NULL AND provider_id IS NOT NULL))
);
CREATE TABLE proposal_pull_requests (
  proposal_id uuid PRIMARY KEY,
  organization_id uuid NOT NULL,
  repository_id uuid NOT NULL,
  remote_id text NOT NULL CHECK(remote_id~'^[1-9][0-9]{0,19}$'),
  installation_id text NOT NULL CHECK(installation_id~'^[1-9][0-9]{0,19}$'),
  branch text NOT NULL,
  pr_number bigint NOT NULL CHECK(pr_number>0),
  provider_id text NOT NULL CHECK(provider_id~'^[1-9][0-9]{0,19}$'),
  node_id text NOT NULL CHECK(length(node_id) BETWEEN 1 AND 255 AND node_id~'^[A-Za-z0-9_=+/-]+$'),
  url text NOT NULL CHECK(length(url)<=2048 AND url~'^https://github.com/[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+/pull/[1-9][0-9]*$'),
  created_operation uuid NOT NULL UNIQUE REFERENCES pr_sync_intents(operation_id),
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  UNIQUE(organization_id,proposal_id),
  UNIQUE(organization_id,remote_id,pr_number),
  UNIQUE(organization_id,remote_id,provider_id),
  FOREIGN KEY(organization_id,proposal_id) REFERENCES proposals(organization_id,id),
  FOREIGN KEY(organization_id,repository_id) REFERENCES repositories(organization_id,id)
);
CREATE TABLE pr_sync_results (
  operation_id uuid PRIMARY KEY REFERENCES pr_sync_intents(operation_id),
  disposition text NOT NULL CHECK(disposition IN ('SYNCED','SUPERSEDED')),
  pr_number bigint NOT NULL CHECK(pr_number>0),
  provider_id text NOT NULL CHECK(provider_id~'^[1-9][0-9]{0,19}$'),
  observed_head text NOT NULL CHECK(observed_head~'^[a-f0-9]{40,128}$' AND length(observed_head)%2=0),
  observed_target text NOT NULL CHECK(observed_target~'^[a-f0-9]{40,128}$' AND length(observed_target)%2=0),
  request_hash text NOT NULL CHECK(request_hash~'^[a-f0-9]{64}$'),
  fence_worker uuid NOT NULL,
  fence_attempt integer NOT NULL CHECK(fence_attempt>0),
  created_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
CREATE INDEX pr_sync_claim ON jobs(available_at,created_at,id)
  WHERE kind='SYNC_REVIEW_PR' AND state IN ('QUEUED','RUNNING');
CREATE FUNCTION pr_sync_write_insert() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF NOT EXISTS(SELECT 1 FROM pr_sync_intents i JOIN jobs j ON j.id=i.job_id
      JOIN git_operations o ON o.id=i.operation_id
      JOIN proposals p ON p.id=i.proposal_id AND p.organization_id=i.organization_id
      JOIN repositories r ON r.id=p.repository_id AND r.organization_id=p.organization_id
      JOIN organizations g ON g.id=p.organization_id
      WHERE i.operation_id=NEW.operation_id AND o.state='PENDING' AND o.kind='PR_SYNC'
      AND j.kind='SYNC_REVIEW_PR' AND j.state='RUNNING' AND j.lease_owner=NEW.fence_worker
      AND j.attempts=NEW.fence_attempt AND j.leased_until>clock_timestamp()
      AND p.active_review_snapshot_id=i.snapshot_id AND p.head_sha=i.head_sha AND p.target_sha=i.target_sha
      AND p.version=i.proposal_version AND p.review_configuration_version=i.configuration_version
      AND g.policy_version=i.policy_version AND p.state IN ('IN_REVIEW','APPROVED','CHANGES_REQUESTED')
      AND p.repository_id=i.repository_id AND p.branch=i.branch AND r.provider='GITHUB'
      AND r.default_branch='main' AND ltrim(r.remote_id,'0+')=i.remote_id AND ltrim(r.installation_id,'0+')=i.installation_id
      AND ((NEW.kind='CREATE' AND NOT EXISTS(SELECT 1 FROM proposal_pull_requests b WHERE b.proposal_id=i.proposal_id))
        OR (NEW.kind='UPDATE' AND EXISTS(SELECT 1 FROM proposal_pull_requests b WHERE b.proposal_id=i.proposal_id
          AND b.organization_id=i.organization_id AND b.repository_id=i.repository_id AND b.remote_id=i.remote_id
          AND b.installation_id=i.installation_id AND b.branch=i.branch AND b.pr_number=NEW.pr_number AND b.provider_id=NEW.provider_id)))
      FOR SHARE OF j,o,p,r,g) THEN RAISE EXCEPTION 'PR write requires its current fence and scope'; END IF;
  RETURN NEW;
END $$;
CREATE TRIGGER pr_sync_write_insert BEFORE INSERT ON pr_sync_writes FOR EACH ROW EXECUTE FUNCTION pr_sync_write_insert();
CREATE FUNCTION pr_sync_binding_insert() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF NOT EXISTS(SELECT 1 FROM pr_sync_intents i JOIN pr_sync_writes w ON w.operation_id=i.operation_id AND w.kind='CREATE'
      JOIN jobs j ON j.id=i.job_id JOIN git_operations o ON o.id=i.operation_id
      WHERE i.operation_id=NEW.created_operation AND i.proposal_id=NEW.proposal_id AND i.organization_id=NEW.organization_id
      AND i.repository_id=NEW.repository_id AND i.remote_id=NEW.remote_id AND i.installation_id=NEW.installation_id AND i.branch=NEW.branch
      AND o.state IN ('RUNNING','UNKNOWN') AND j.kind='SYNC_REVIEW_PR' AND j.state='RUNNING' AND j.leased_until>clock_timestamp()
      FOR SHARE OF j,o) THEN RAISE EXCEPTION 'PR identity requires its original creation journal'; END IF;
  RETURN NEW;
END $$;
CREATE TRIGGER pr_sync_binding_insert BEFORE INSERT ON proposal_pull_requests FOR EACH ROW EXECUTE FUNCTION pr_sync_binding_insert();
CREATE FUNCTION pr_sync_result_insert() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF NOT EXISTS(SELECT 1 FROM pr_sync_intents i JOIN jobs j ON j.id=i.job_id JOIN git_operations o ON o.id=i.operation_id
      JOIN proposal_pull_requests b ON b.proposal_id=i.proposal_id AND b.organization_id=i.organization_id
      JOIN proposals p ON p.id=i.proposal_id AND p.organization_id=i.organization_id
      JOIN repositories r ON r.id=p.repository_id AND r.organization_id=p.organization_id
      JOIN organizations g ON g.id=p.organization_id
      WHERE i.operation_id=NEW.operation_id AND i.request_hash=NEW.request_hash AND o.state IN ('PENDING','RUNNING','UNKNOWN')
      AND j.kind='SYNC_REVIEW_PR' AND j.state='RUNNING' AND j.lease_owner=NEW.fence_worker
      AND j.attempts=NEW.fence_attempt AND j.leased_until>clock_timestamp()
      AND b.repository_id=i.repository_id AND b.remote_id=i.remote_id AND b.installation_id=i.installation_id AND b.branch=i.branch
      AND b.pr_number=NEW.pr_number AND b.provider_id=NEW.provider_id
      AND p.repository_id=i.repository_id AND p.branch=i.branch AND r.provider='GITHUB' AND r.default_branch='main'
      AND ltrim(r.remote_id,'0+')=i.remote_id AND ltrim(r.installation_id,'0+')=i.installation_id
      AND (NEW.disposition='SUPERSEDED' OR (p.active_review_snapshot_id=i.snapshot_id AND p.version=i.proposal_version
        AND p.head_sha=i.head_sha AND p.target_sha=i.target_sha AND p.review_configuration_version=i.configuration_version
        AND g.policy_version=i.policy_version AND p.state IN ('IN_REVIEW','APPROVED','CHANGES_REQUESTED')
        AND NEW.observed_head=i.head_sha AND NEW.observed_target=i.target_sha))
      FOR SHARE OF j,o,b,p,r,g) THEN RAISE EXCEPTION 'PR receipt requires current authority, identity and lease'; END IF;
  RETURN NEW;
END $$;
CREATE TRIGGER pr_sync_result_insert BEFORE INSERT ON pr_sync_results FOR EACH ROW EXECUTE FUNCTION pr_sync_result_insert();
CREATE TRIGGER pr_sync_write_immutable BEFORE UPDATE OR DELETE ON pr_sync_writes FOR EACH ROW EXECUTE FUNCTION review_submission_immutable();
CREATE TRIGGER pr_sync_write_no_truncate BEFORE TRUNCATE ON pr_sync_writes FOR EACH STATEMENT EXECUTE FUNCTION review_submission_immutable();
CREATE TRIGGER pr_sync_binding_immutable BEFORE UPDATE OR DELETE ON proposal_pull_requests FOR EACH ROW EXECUTE FUNCTION review_submission_immutable();
CREATE TRIGGER pr_sync_binding_no_truncate BEFORE TRUNCATE ON proposal_pull_requests FOR EACH STATEMENT EXECUTE FUNCTION review_submission_immutable();
CREATE TRIGGER pr_sync_result_immutable BEFORE UPDATE OR DELETE ON pr_sync_results FOR EACH ROW EXECUTE FUNCTION review_submission_immutable();
CREATE TRIGGER pr_sync_result_no_truncate BEFORE TRUNCATE ON pr_sync_results FOR EACH STATEMENT EXECUTE FUNCTION review_submission_immutable();
CREATE OR REPLACE FUNCTION pr_sync_scope_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF TG_TABLE_NAME='jobs' AND OLD.kind='SYNC_REVIEW_PR' THEN
    IF TG_OP='DELETE' THEN RAISE EXCEPTION 'PR sync job is durable'; END IF;
    IF ROW(NEW.id,NEW.organization_id,NEW.operation_id,NEW.kind,NEW.payload,NEW.max_attempts,NEW.created_at)
       IS DISTINCT FROM ROW(OLD.id,OLD.organization_id,OLD.operation_id,OLD.kind,OLD.payload,OLD.max_attempts,OLD.created_at)
       OR (NEW.state='SUCCEEDED' AND NOT EXISTS(SELECT 1 FROM pr_sync_results WHERE operation_id=OLD.operation_id AND disposition='SYNCED'))
       OR (OLD.state IN ('SUCCEEDED','CANCELLED','FAILED') AND NEW.state<>OLD.state) THEN
      RAISE EXCEPTION 'PR sync requires its dedicated journal'; END IF;
  ELSIF TG_TABLE_NAME='git_operations' AND OLD.kind='PR_SYNC' THEN
    IF TG_OP='DELETE' THEN RAISE EXCEPTION 'PR sync operation is durable'; END IF;
    IF ROW(NEW.id,NEW.organization_id,NEW.proposal_id,NEW.actor_id,NEW.kind,NEW.idempotency_key,NEW.request_hash,NEW.expected_sha,NEW.created_at)
       IS DISTINCT FROM ROW(OLD.id,OLD.organization_id,OLD.proposal_id,OLD.actor_id,OLD.kind,OLD.idempotency_key,OLD.request_hash,OLD.expected_sha,OLD.created_at)
       OR (OLD.state IN ('SUCCEEDED','FAILED') AND NEW.state<>OLD.state)
       OR (NEW.state='PENDING' AND OLD.state<>'PENDING')
       OR (NEW.state='RUNNING' AND (OLD.state<>'PENDING' OR NOT EXISTS(SELECT 1 FROM pr_sync_writes WHERE operation_id=OLD.id)))
       OR (NEW.state='FAILED' AND EXISTS(SELECT 1 FROM pr_sync_writes WHERE operation_id=OLD.id))
       OR (NEW.state='SUCCEEDED' AND NOT EXISTS(SELECT 1 FROM pr_sync_results WHERE operation_id=OLD.id)) THEN
      RAISE EXCEPTION 'PR sync mutation requires its fenced journal'; END IF;
  END IF;
  IF TG_OP='DELETE' THEN RETURN OLD; END IF; RETURN NEW;
END $$;
CREATE OR REPLACE FUNCTION pr_sync_complete() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE op uuid; valid boolean;
BEGIN
  IF TG_TABLE_NAME='jobs' THEN
    IF NEW.kind<>'SYNC_REVIEW_PR' THEN RETURN NULL; END IF; op:=NEW.operation_id;
  ELSIF TG_TABLE_NAME='git_operations' THEN
    IF NEW.kind<>'PR_SYNC' THEN RETURN NULL; END IF; op:=NEW.id;
  ELSIF TG_TABLE_NAME='proposal_pull_requests' THEN op:=NEW.created_operation;
  ELSE op:=NEW.operation_id; END IF;
  SELECT EXISTS(SELECT 1 FROM pr_sync_intents i JOIN jobs j ON j.id=i.job_id
      JOIN git_operations o ON o.id=i.operation_id JOIN outbox e ON e.id=i.source_event
      LEFT JOIN pr_sync_writes w ON w.operation_id=i.operation_id LEFT JOIN pr_sync_results x ON x.operation_id=i.operation_id
      WHERE i.operation_id=op AND j.kind='SYNC_REVIEW_PR' AND j.operation_id=o.id AND o.kind='PR_SYNC'
      AND j.organization_id=i.organization_id AND o.organization_id=i.organization_id AND e.sent_at IS NOT NULL
      AND EXISTS(SELECT 1 FROM audit_events a WHERE a.organization_id=i.organization_id AND a.entity_id=i.proposal_id
        AND a.actor_id=i.actor_id AND a.operation_id=i.operation_id AND a.action='PR_SYNC_RESERVED' AND a.after_ref=i.snapshot_id::text)
      AND ((x.operation_id IS NULL AND o.state IN ('PENDING','RUNNING','UNKNOWN','FAILED')
        AND j.state IN ('QUEUED','RUNNING','FAILED') AND (o.state<>'FAILED' OR j.state='FAILED')
        AND ((w.operation_id IS NULL AND o.state IN ('PENDING','FAILED')) OR (w.operation_id IS NOT NULL AND o.state IN ('RUNNING','UNKNOWN'))))
        OR (x.operation_id IS NOT NULL AND o.state='SUCCEEDED' AND o.result_sha=x.observed_head
          AND j.state=CASE WHEN x.disposition='SYNCED' THEN 'SUCCEEDED' ELSE 'CANCELLED' END
          AND j.result=jsonb_build_object('phase',x.disposition,'snapshot_id',i.snapshot_id,'pr_number',x.pr_number)
          AND EXISTS(SELECT 1 FROM audit_events a WHERE a.operation_id=i.operation_id AND a.organization_id=i.organization_id
            AND a.actor_id=i.actor_id AND a.entity_id=i.proposal_id AND a.action='PR_SYNC_RESOLVED' AND a.result=x.disposition)))
      AND (w.operation_id IS NULL OR EXISTS(SELECT 1 FROM audit_events a WHERE a.operation_id=i.operation_id
        AND a.organization_id=i.organization_id AND a.actor_id=i.actor_id AND a.entity_id=i.proposal_id AND a.action='PR_SYNC_WRITE_STARTED' AND a.result=w.kind))
      AND NOT EXISTS(SELECT 1 FROM proposal_pull_requests b WHERE b.created_operation=op AND x.operation_id IS NULL)) INTO valid;
  IF NOT valid THEN RAISE EXCEPTION 'PR sync journal outcome is incomplete'; END IF; RETURN NULL;
END $$;
CREATE CONSTRAINT TRIGGER pr_sync_write_complete AFTER INSERT ON pr_sync_writes DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION pr_sync_complete();
CREATE CONSTRAINT TRIGGER pr_sync_binding_complete AFTER INSERT ON proposal_pull_requests DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION pr_sync_complete();
CREATE CONSTRAINT TRIGGER pr_sync_result_complete AFTER INSERT ON pr_sync_results DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION pr_sync_complete();
