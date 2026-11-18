-- Recovery adds a bounded attempt window to the same immutable operation.
-- A recorded write is never erased, failed, or authorized to write again.
CREATE TABLE pr_sync_recoveries (
  organization_id uuid NOT NULL,
  request_id uuid NOT NULL,
  proposal_id uuid NOT NULL,
  snapshot_id uuid NOT NULL REFERENCES review_snapshots(id),
  operation_id uuid NOT NULL REFERENCES pr_sync_intents(operation_id),
  actor_id uuid NOT NULL REFERENCES users(id),
  fingerprint text NOT NULL CHECK(fingerprint~'^[a-f0-9]{64}$'),
  mode text NOT NULL CHECK(mode IN ('RETRY_UNSTARTED','OBSERVE_STARTED')),
  previous_attempts integer NOT NULL CHECK(previous_attempts>=0),
  previous_max_attempts integer NOT NULL CHECK(previous_max_attempts>0),
  max_attempts integer NOT NULL CHECK(max_attempts=previous_max_attempts+8),
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  created_transaction bigint NOT NULL DEFAULT txid_current(),
  PRIMARY KEY(organization_id,request_id),
  UNIQUE(operation_id,max_attempts),
  FOREIGN KEY(organization_id,proposal_id) REFERENCES proposals(organization_id,id)
);
CREATE INDEX pr_recovery_quota ON pr_sync_recoveries(organization_id,created_at,actor_id);
CREATE FUNCTION pr_sync_recovery_insert() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF NEW.created_transaction<>txid_current() OR NOT EXISTS(
    SELECT 1 FROM pr_sync_intents i JOIN jobs j ON j.id=i.job_id
    JOIN git_operations o ON o.id=i.operation_id
    JOIN proposals p ON p.id=i.proposal_id AND p.organization_id=i.organization_id
    JOIN repositories r ON r.id=p.repository_id AND r.organization_id=p.organization_id
    JOIN memberships m ON m.organization_id=i.organization_id AND m.user_id=NEW.actor_id
    JOIN users u ON u.id=m.user_id
    WHERE i.operation_id=NEW.operation_id AND i.organization_id=NEW.organization_id
      AND i.proposal_id=NEW.proposal_id AND i.snapshot_id=NEW.snapshot_id AND i.fingerprint=NEW.fingerprint
      AND j.kind='SYNC_REVIEW_PR' AND j.state='FAILED' AND j.attempts=NEW.previous_attempts
      AND j.max_attempts=NEW.previous_max_attempts AND j.lease_owner IS NULL AND j.leased_until IS NULL
      AND m.role='ADMIN' AND u.status='ACTIVE' AND p.repository_id=i.repository_id AND p.branch=i.branch
      AND r.provider='GITHUB' AND r.default_branch='main' AND ltrim(r.remote_id,'0+')=i.remote_id
      AND ltrim(r.installation_id,'0+')=i.installation_id
      AND NOT EXISTS(SELECT 1 FROM pr_sync_results x WHERE x.operation_id=i.operation_id)
      AND ((NEW.mode='OBSERVE_STARTED' AND o.state='UNKNOWN' AND EXISTS(SELECT 1 FROM pr_sync_writes w WHERE w.operation_id=i.operation_id))
        OR (NEW.mode='RETRY_UNSTARTED' AND o.state='FAILED' AND NOT EXISTS(SELECT 1 FROM pr_sync_writes w WHERE w.operation_id=i.operation_id)
          AND p.version=i.proposal_version AND pr_sync_snapshot_current(i.organization_id,i.proposal_id,i.snapshot_id)))
    FOR SHARE OF i,j,o,p,r,m,u
  ) THEN RAISE EXCEPTION 'PR recovery requires current admin, failed work and original binding'; END IF;
  RETURN NEW;
END $$;
CREATE TRIGGER pr_sync_recovery_insert BEFORE INSERT ON pr_sync_recoveries FOR EACH ROW EXECUTE FUNCTION pr_sync_recovery_insert();
CREATE TRIGGER pr_sync_recovery_immutable BEFORE UPDATE OR DELETE ON pr_sync_recoveries FOR EACH ROW EXECUTE FUNCTION review_submission_immutable();
CREATE TRIGGER pr_sync_recovery_no_truncate BEFORE TRUNCATE ON pr_sync_recoveries FOR EACH STATEMENT EXECUTE FUNCTION review_submission_immutable();

CREATE OR REPLACE FUNCTION pr_sync_scope_guard() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE recovery boolean;
BEGIN
  IF TG_TABLE_NAME='jobs' AND OLD.kind='SYNC_REVIEW_PR' THEN
    IF TG_OP='DELETE' THEN RAISE EXCEPTION 'PR sync job is durable'; END IF;
    recovery:=OLD.state='FAILED' AND NEW.state='QUEUED' AND NEW.attempts=OLD.attempts
      AND NEW.max_attempts=OLD.max_attempts+8 AND NEW.lease_owner IS NULL AND NEW.leased_until IS NULL
      AND EXISTS(SELECT 1 FROM pr_sync_recoveries r WHERE r.operation_id=OLD.operation_id
        AND r.previous_attempts=OLD.attempts AND r.previous_max_attempts=OLD.max_attempts
        AND r.max_attempts=NEW.max_attempts AND r.created_transaction=txid_current());
    IF ROW(NEW.id,NEW.organization_id,NEW.operation_id,NEW.kind,NEW.payload,NEW.created_at)
       IS DISTINCT FROM ROW(OLD.id,OLD.organization_id,OLD.operation_id,OLD.kind,OLD.payload,OLD.created_at)
       OR (NEW.max_attempts<>OLD.max_attempts AND NOT recovery)
       OR (NEW.state='SUCCEEDED' AND NOT EXISTS(SELECT 1 FROM pr_sync_results WHERE operation_id=OLD.operation_id AND disposition='SYNCED'))
       OR (OLD.state IN ('SUCCEEDED','CANCELLED','FAILED') AND NEW.state<>OLD.state AND NOT recovery) THEN
      RAISE EXCEPTION 'PR sync requires its dedicated journal'; END IF;
  ELSIF TG_TABLE_NAME='git_operations' AND OLD.kind='PR_SYNC' THEN
    IF TG_OP='DELETE' THEN RAISE EXCEPTION 'PR sync operation is durable'; END IF;
    recovery:=OLD.state='FAILED' AND NEW.state='PENDING' AND EXISTS(
      SELECT 1 FROM pr_sync_recoveries r WHERE r.operation_id=OLD.id
        AND r.mode='RETRY_UNSTARTED' AND r.created_transaction=txid_current());
    IF ROW(NEW.id,NEW.organization_id,NEW.proposal_id,NEW.actor_id,NEW.kind,NEW.idempotency_key,NEW.request_hash,NEW.expected_sha,NEW.created_at)
       IS DISTINCT FROM ROW(OLD.id,OLD.organization_id,OLD.proposal_id,OLD.actor_id,OLD.kind,OLD.idempotency_key,OLD.request_hash,OLD.expected_sha,OLD.created_at)
       OR (OLD.state IN ('SUCCEEDED','FAILED') AND NEW.state<>OLD.state AND NOT recovery)
       OR (NEW.state='PENDING' AND OLD.state<>'PENDING' AND NOT recovery)
       OR (NEW.state='RUNNING' AND (OLD.state<>'PENDING' OR NOT EXISTS(SELECT 1 FROM pr_sync_writes WHERE operation_id=OLD.id)))
       OR (NEW.state='FAILED' AND EXISTS(SELECT 1 FROM pr_sync_writes WHERE operation_id=OLD.id))
       OR (NEW.state='SUCCEEDED' AND NOT EXISTS(SELECT 1 FROM pr_sync_results WHERE operation_id=OLD.id)) THEN
      RAISE EXCEPTION 'PR sync mutation requires its fenced journal'; END IF;
  END IF;
  IF TG_OP='DELETE' THEN RETURN OLD; END IF; RETURN NEW;
END $$;
CREATE FUNCTION pr_sync_recovery_complete() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF NOT EXISTS(SELECT 1 FROM pr_sync_intents i JOIN jobs j ON j.id=i.job_id JOIN git_operations o ON o.id=i.operation_id
      WHERE i.operation_id=NEW.operation_id AND j.state='QUEUED' AND j.attempts=NEW.previous_attempts
      AND j.max_attempts=NEW.max_attempts AND j.lease_owner IS NULL AND j.leased_until IS NULL
      AND o.state=CASE WHEN NEW.mode='OBSERVE_STARTED' THEN 'UNKNOWN' ELSE 'PENDING' END
      AND EXISTS(SELECT 1 FROM audit_events a WHERE a.organization_id=NEW.organization_id AND a.entity_id=NEW.proposal_id
        AND a.actor_id=NEW.actor_id AND a.operation_id=NEW.operation_id AND a.action='PR_SYNC_RECOVERY_QUEUED'
        AND a.result=NEW.mode AND a.after_ref=NEW.request_id::text)) THEN
    RAISE EXCEPTION 'PR recovery requires atomic job, operation and operator audit';
  END IF;
  RETURN NULL;
END $$;
CREATE CONSTRAINT TRIGGER pr_sync_recovery_complete AFTER INSERT ON pr_sync_recoveries DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION pr_sync_recovery_complete();
