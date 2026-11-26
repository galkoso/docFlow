-- An operator can extend the same stopped job. Original intents, markers,
-- attempt history and provider identities remain immutable.
CREATE TABLE review_check_recoveries (
 organization_id uuid NOT NULL,
 request_id uuid NOT NULL,
 proposal_id uuid NOT NULL,
 operation_id uuid NOT NULL REFERENCES review_check_intents(operation_id),
 actor_id uuid NOT NULL REFERENCES users(id),
 request_hash text NOT NULL CHECK(request_hash~'^[a-f0-9]{64}$'),
 mode text NOT NULL CHECK(mode IN ('RETRY_UNSTARTED','OBSERVE_STARTED')),
 previous_attempts integer NOT NULL CHECK(previous_attempts>=0),
 previous_max_attempts integer NOT NULL CHECK(previous_max_attempts>0),
 max_attempts integer NOT NULL CHECK(max_attempts::bigint=previous_max_attempts::bigint+8),
 audit_id uuid NOT NULL UNIQUE REFERENCES audit_events(id) DEFERRABLE INITIALLY DEFERRED,
 event_id uuid NOT NULL UNIQUE REFERENCES outbox(id) DEFERRABLE INITIALLY DEFERRED,
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 created_transaction bigint NOT NULL DEFAULT txid_current(),
 PRIMARY KEY(organization_id,request_id),
 UNIQUE(operation_id,max_attempts),
 FOREIGN KEY(organization_id,proposal_id) REFERENCES proposals(organization_id,id)
);
CREATE INDEX review_check_recovery_quota ON review_check_recoveries(organization_id,created_at,actor_id);
CREATE FUNCTION review_check_recovery_insert() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF NEW.created_transaction<>txid_current() OR NOT EXISTS(
   SELECT 1 FROM review_check_intents i JOIN jobs j ON j.id=i.job_id JOIN git_operations o ON o.id=i.operation_id
   JOIN memberships m ON m.organization_id=i.organization_id AND m.user_id=NEW.actor_id JOIN users u ON u.id=m.user_id
   WHERE i.operation_id=NEW.operation_id AND i.organization_id=NEW.organization_id AND i.proposal_id=NEW.proposal_id
     AND i.request_hash=NEW.request_hash AND j.kind='PUBLISH_REVIEW_CHECK' AND j.state='FAILED'
     AND j.attempts=NEW.previous_attempts AND j.max_attempts=NEW.previous_max_attempts
     AND j.lease_owner IS NULL AND j.leased_until IS NULL AND m.role='ADMIN' AND u.status='ACTIVE'
     AND NOT EXISTS(SELECT 1 FROM review_check_results x WHERE x.operation_id=i.operation_id)
     AND ((NEW.mode='OBSERVE_STARTED' AND o.state='UNKNOWN' AND EXISTS(SELECT 1 FROM review_check_writes WHERE operation_id=i.operation_id))
       OR (NEW.mode='RETRY_UNSTARTED' AND o.state='FAILED' AND NOT EXISTS(SELECT 1 FROM review_check_writes WHERE operation_id=i.operation_id)))
   FOR SHARE OF i,j,o,m,u) OR NOT review_check_lock_scope(NEW.operation_id,NEW.mode='RETRY_UNSTARTED') THEN
   RAISE EXCEPTION 'check recovery requires current admin, original binding and exact stopped work'; END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER review_check_recovery_insert BEFORE INSERT ON review_check_recoveries FOR EACH ROW EXECUTE FUNCTION review_check_recovery_insert();
CREATE TRIGGER review_check_recovery_immutable BEFORE UPDATE OR DELETE ON review_check_recoveries FOR EACH ROW EXECUTE FUNCTION review_submission_immutable();
CREATE TRIGGER review_check_recovery_no_truncate BEFORE TRUNCATE ON review_check_recoveries FOR EACH STATEMENT EXECUTE FUNCTION review_submission_immutable();
CREATE OR REPLACE FUNCTION review_check_scope_guard() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE recovery boolean;
BEGIN
 IF TG_TABLE_NAME='jobs' AND (OLD.kind='PUBLISH_REVIEW_CHECK' OR (TG_OP='UPDATE' AND NEW.kind='PUBLISH_REVIEW_CHECK')) THEN
   IF TG_OP='DELETE' THEN RAISE EXCEPTION 'check job is durable'; END IF;
   recovery:=OLD.state='FAILED' AND NEW.state='QUEUED' AND NEW.attempts=OLD.attempts
     AND NEW.max_attempts::bigint=OLD.max_attempts::bigint+8 AND NEW.lease_owner IS NULL AND NEW.leased_until IS NULL
     AND EXISTS(SELECT 1 FROM review_check_recoveries c WHERE c.operation_id=OLD.operation_id
       AND c.previous_attempts=OLD.attempts AND c.previous_max_attempts=OLD.max_attempts
       AND c.max_attempts=NEW.max_attempts AND c.created_transaction=txid_current());
   IF ROW(NEW.id,NEW.organization_id,NEW.operation_id,NEW.kind,NEW.payload,NEW.created_at)
     IS DISTINCT FROM ROW(OLD.id,OLD.organization_id,OLD.operation_id,OLD.kind,OLD.payload,OLD.created_at)
     OR (NEW.max_attempts<>OLD.max_attempts AND NOT recovery)
     OR (NEW.state='SUCCEEDED' AND (OLD.state<>'RUNNING' OR OLD.leased_until<=clock_timestamp()
       OR NOT EXISTS(SELECT 1 FROM review_check_results x WHERE x.operation_id=OLD.operation_id AND x.fence_worker=OLD.lease_owner AND x.fence_attempt=OLD.attempts)))
     OR (OLD.state IN ('SUCCEEDED','FAILED','CANCELLED') AND NEW.state<>OLD.state AND NOT recovery)
     OR NEW.attempts<OLD.attempts OR NEW.attempts>OLD.attempts+1 OR NEW.attempts>NEW.max_attempts
     OR (NEW.state='RUNNING' AND ((NEW.attempts=OLD.attempts AND (OLD.state<>'RUNNING' OR OLD.lease_owner<>NEW.lease_owner OR OLD.leased_until<=clock_timestamp()))
       OR (NEW.attempts=OLD.attempts+1 AND OLD.state='RUNNING' AND OLD.leased_until>clock_timestamp())
       OR NEW.leased_until>clock_timestamp()+interval '5 minutes')) THEN RAISE EXCEPTION 'check job requires dedicated live fencing'; END IF;
 ELSIF TG_TABLE_NAME='git_operations' AND (OLD.kind='REVIEW_CHECK' OR (TG_OP='UPDATE' AND NEW.kind='REVIEW_CHECK')) THEN
   IF TG_OP='DELETE' THEN RAISE EXCEPTION 'check operation is durable'; END IF;
   recovery:=OLD.state='FAILED' AND NEW.state='PENDING' AND EXISTS(
     SELECT 1 FROM review_check_recoveries c WHERE c.operation_id=OLD.id
       AND c.mode='RETRY_UNSTARTED' AND c.created_transaction=txid_current());
   IF ROW(NEW.id,NEW.organization_id,NEW.proposal_id,NEW.actor_id,NEW.kind,NEW.idempotency_key,NEW.request_hash,NEW.expected_sha,NEW.created_at)
     IS DISTINCT FROM ROW(OLD.id,OLD.organization_id,OLD.proposal_id,OLD.actor_id,OLD.kind,OLD.idempotency_key,OLD.request_hash,OLD.expected_sha,OLD.created_at)
     OR (OLD.state IN ('SUCCEEDED','FAILED') AND NEW.state<>OLD.state AND NOT recovery)
     OR (NEW.state IN ('PENDING','FAILED') AND EXISTS(SELECT 1 FROM review_check_writes WHERE operation_id=OLD.id))
     OR (NEW.state IN ('RUNNING','UNKNOWN') AND NOT EXISTS(SELECT 1 FROM review_check_writes WHERE operation_id=OLD.id))
     OR (NEW.state='SUCCEEDED' AND NOT EXISTS(SELECT 1 FROM review_check_results WHERE operation_id=OLD.id)) THEN
     RAISE EXCEPTION 'check operation requires its durable write/result evidence'; END IF;
 END IF;
 IF TG_OP='DELETE' THEN RETURN OLD; END IF;
 RETURN NEW;
END $$;
CREATE FUNCTION review_check_recovery_complete() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF NOT EXISTS(SELECT 1 FROM review_check_intents i JOIN jobs j ON j.id=i.job_id JOIN git_operations o ON o.id=i.operation_id
   JOIN audit_events a ON a.id=NEW.audit_id JOIN outbox e ON e.id=NEW.event_id
   WHERE i.operation_id=NEW.operation_id AND j.state='QUEUED' AND j.attempts=NEW.previous_attempts
     AND j.max_attempts=NEW.max_attempts AND j.lease_owner IS NULL AND j.leased_until IS NULL
     AND o.state=CASE WHEN NEW.mode='OBSERVE_STARTED' THEN 'UNKNOWN' ELSE 'PENDING' END
     AND a.organization_id=NEW.organization_id AND a.entity_id=NEW.proposal_id AND a.actor_id=NEW.actor_id
     AND a.operation_id=NEW.operation_id AND a.action='REVIEW_CHECK_RECOVERY_QUEUED' AND a.result=NEW.mode AND a.after_ref=NEW.request_id::text
     AND e.organization_id=NEW.organization_id AND e.entity_id=NEW.proposal_id AND e.event_type='REVIEW_CHECK_RECOVERY_QUEUED'
     AND e.payload=jsonb_build_object('operation_id',NEW.operation_id,'request_id',NEW.request_id,'mode',NEW.mode)) THEN
   RAISE EXCEPTION 'check recovery requires atomic queue, operation, audit and event'; END IF;
 RETURN NULL;
END $$;
CREATE CONSTRAINT TRIGGER review_check_recovery_complete AFTER INSERT ON review_check_recoveries DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION review_check_recovery_complete();
CREATE FUNCTION review_check_recovery_event_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF EXISTS(SELECT 1 FROM review_check_recoveries WHERE event_id=OLD.id) THEN
   IF TG_OP='DELETE' THEN RAISE EXCEPTION 'check recovery event is durable'; END IF;
   IF ROW(NEW.id,NEW.organization_id,NEW.event_type,NEW.entity_id,NEW.payload,NEW.created_at)
     IS DISTINCT FROM ROW(OLD.id,OLD.organization_id,OLD.event_type,OLD.entity_id,OLD.payload,OLD.created_at)
     OR (OLD.sent_at IS NOT NULL AND NEW.sent_at IS DISTINCT FROM OLD.sent_at) THEN RAISE EXCEPTION 'check recovery event is immutable'; END IF;
 END IF;
 IF TG_OP='DELETE' THEN RETURN OLD; END IF; RETURN NEW;
END $$;
CREATE TRIGGER review_check_recovery_event_guard BEFORE UPDATE OR DELETE ON outbox FOR EACH ROW EXECUTE FUNCTION review_check_recovery_event_guard();
