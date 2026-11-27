-- Check delivery is independent of the PR consumer's sent_at marker.
CREATE TABLE review_check_dispatch (
 event_id uuid PRIMARY KEY REFERENCES outbox(id),
 organization_id uuid NOT NULL REFERENCES organizations(id),
 proposal_id uuid NOT NULL,
 state text NOT NULL CHECK(state IN ('RUNNING','WAITING','ENQUEUED','SKIPPED','REJECTED')),
 reason text NOT NULL CHECK(reason~'^[A-Z0-9_]{1,80}$'),
 attempts integer NOT NULL CHECK(attempts>0),
 lease_owner uuid,
 leased_until timestamptz,
 next_at timestamptz,
 operation_id uuid REFERENCES review_check_intents(operation_id),
 audit_id uuid NOT NULL REFERENCES audit_events(id) DEFERRABLE INITIALLY DEFERRED,
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 CHECK((state='RUNNING')=(lease_owner IS NOT NULL AND leased_until IS NOT NULL)),
 CHECK(state='RUNNING' OR (lease_owner IS NULL AND leased_until IS NULL)),
 CHECK((state='WAITING')=(next_at IS NOT NULL)),
 CHECK((state='ENQUEUED')=(operation_id IS NOT NULL)),
 CHECK(operation_id IS NULL OR operation_id=event_id)
);
CREATE INDEX review_check_dispatch_retry ON review_check_dispatch(next_at,event_id) WHERE state='WAITING';
CREATE INDEX review_check_dispatch_expired ON review_check_dispatch(leased_until,event_id) WHERE state='RUNNING';
CREATE INDEX review_check_publications ON outbox(created_at,id) WHERE event_type='REVIEW_SNAPSHOT_PUBLISHED';
CREATE INDEX review_check_history ON review_check_intents(organization_id,proposal_id,created_at DESC,operation_id DESC);
ALTER TABLE review_check_intents ADD COLUMN source_event uuid UNIQUE REFERENCES review_check_dispatch(event_id) DEFERRABLE INITIALLY DEFERRED;
ALTER TABLE review_check_intents ADD COLUMN dispatch_worker uuid;
ALTER TABLE review_check_intents ADD COLUMN dispatch_attempt integer;
ALTER TABLE review_check_intents ADD CONSTRAINT review_check_publication_source CHECK(
 (source_event IS NULL AND dispatch_worker IS NULL AND dispatch_attempt IS NULL)
 OR (source_event IS NOT NULL AND dispatch_worker IS NOT NULL AND dispatch_attempt IS NOT NULL AND dispatch_attempt>0 AND purpose='EVALUATION' AND source_event=operation_id));
CREATE FUNCTION review_check_source_insert() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF NEW.source_event IS NOT NULL AND NOT EXISTS(
   SELECT 1 FROM review_check_dispatch d JOIN outbox e ON e.id=d.event_id
   WHERE d.event_id=NEW.source_event AND d.organization_id=NEW.organization_id AND d.proposal_id=NEW.proposal_id
     AND d.state='RUNNING' AND d.lease_owner=NEW.dispatch_worker AND d.attempts=NEW.dispatch_attempt AND d.leased_until>clock_timestamp()
     AND pr_sync_publication_actor(e.id)=NEW.actor_id AND e.payload->>'snapshot_id'=NEW.snapshot_id::text
   FOR SHARE OF d) THEN RAISE EXCEPTION 'check admission requires its live delivery fence and backed publication'; END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER review_check_source_insert BEFORE INSERT ON review_check_intents FOR EACH ROW EXECUTE FUNCTION review_check_source_insert();
CREATE FUNCTION review_check_dispatch_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF TG_OP='DELETE' THEN RAISE EXCEPTION 'check delivery is durable'; END IF;
 IF NOT EXISTS(SELECT 1 FROM outbox WHERE id=NEW.event_id AND organization_id=NEW.organization_id AND entity_id=NEW.proposal_id AND event_type='REVIEW_SNAPSHOT_PUBLISHED') THEN
   RAISE EXCEPTION 'check delivery requires its exact publication envelope'; END IF;
 IF TG_OP='INSERT' AND (NEW.state<>'RUNNING' OR NEW.attempts<>1) THEN RAISE EXCEPTION 'check delivery must be claimed first'; END IF;
 IF TG_OP='UPDATE' THEN
   IF ROW(NEW.event_id,NEW.organization_id,NEW.proposal_id,NEW.created_at) IS DISTINCT FROM ROW(OLD.event_id,OLD.organization_id,OLD.proposal_id,OLD.created_at)
      OR OLD.state IN ('ENQUEUED','SKIPPED','REJECTED') OR NEW.audit_id=OLD.audit_id THEN RAISE EXCEPTION 'check delivery scope and terminal receipts are immutable'; END IF;
   IF NEW.state='RUNNING' THEN
     IF NEW.attempts<>least(OLD.attempts::bigint+1,2147483647)
       OR NOT ((OLD.state='RUNNING' AND OLD.leased_until<=clock_timestamp()) OR (OLD.state='WAITING' AND OLD.next_at<=clock_timestamp())) THEN
       RAISE EXCEPTION 'check delivery reclaim requires expiry and the next attempt'; END IF;
   ELSIF OLD.state<>'RUNNING' OR OLD.leased_until<=clock_timestamp() OR NEW.attempts<>OLD.attempts THEN
     RAISE EXCEPTION 'check delivery result requires a live attempt'; END IF;
 END IF;
 IF NEW.state='RUNNING' AND (NEW.leased_until<=clock_timestamp() OR NEW.leased_until>clock_timestamp()+interval '2 minutes') THEN RAISE EXCEPTION 'check delivery lease is bounded'; END IF;
 IF NEW.state='ENQUEUED' THEN
   IF NOT EXISTS(SELECT 1 FROM review_check_intents i WHERE i.operation_id=NEW.operation_id AND i.source_event=NEW.event_id
     AND i.organization_id=NEW.organization_id AND i.proposal_id=NEW.proposal_id) THEN RAISE EXCEPTION 'check delivery requires its source-bound reservation'; END IF;
 ELSIF EXISTS(SELECT 1 FROM review_check_intents WHERE source_event=NEW.event_id) THEN RAISE EXCEPTION 'admitted checks cannot be discarded'; END IF;
 IF NEW.state='SKIPPED' AND (pr_sync_publication_actor(NEW.event_id) IS NULL OR pr_sync_snapshot_current(NEW.organization_id,NEW.proposal_id,(SELECT (payload->>'snapshot_id')::uuid FROM outbox WHERE id=NEW.event_id))) THEN
   RAISE EXCEPTION 'check skip requires a superseded backed publication'; END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER review_check_dispatch_guard BEFORE INSERT OR UPDATE OR DELETE ON review_check_dispatch FOR EACH ROW EXECUTE FUNCTION review_check_dispatch_guard();
CREATE TRIGGER review_check_dispatch_no_truncate BEFORE TRUNCATE ON review_check_dispatch FOR EACH STATEMENT EXECUTE FUNCTION review_submission_immutable();
CREATE FUNCTION review_check_dispatch_complete() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE event uuid; d review_check_dispatch;
BEGIN
 IF TG_TABLE_NAME='review_check_intents' THEN event:=NEW.source_event; ELSE event:=NEW.event_id; END IF;
 IF event IS NULL THEN RETURN NULL; END IF;
 SELECT * INTO d FROM review_check_dispatch WHERE event_id=event;
 IF d.event_id IS NULL OR (d.state='ENQUEUED') IS DISTINCT FROM (EXISTS(SELECT 1 FROM review_check_intents WHERE source_event=event))
   OR NOT EXISTS(SELECT 1 FROM audit_events a WHERE a.id=d.audit_id AND a.organization_id=d.organization_id AND a.entity_id=d.proposal_id
     AND a.actor_id IS NULL AND a.action='CHECK_OUTBOX_DISPATCH' AND a.after_ref=event::text AND a.operation_id IS NOT DISTINCT FROM d.operation_id
     AND a.result=d.state||':'||d.reason||':'||d.attempts::text) THEN RAISE EXCEPTION 'check delivery, reservation and exact audit must commit together'; END IF;
 RETURN NULL;
END $$;
CREATE CONSTRAINT TRIGGER review_check_dispatch_complete AFTER INSERT OR UPDATE ON review_check_dispatch DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION review_check_dispatch_complete();
CREATE CONSTRAINT TRIGGER review_check_dispatch_intent_complete AFTER INSERT ON review_check_intents DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION review_check_dispatch_complete();
CREATE FUNCTION review_check_dispatch_event_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF EXISTS(SELECT 1 FROM review_check_dispatch WHERE event_id=OLD.id) THEN
   IF TG_OP='DELETE' THEN RAISE EXCEPTION 'check source event is durable'; END IF;
   IF ROW(NEW.id,NEW.organization_id,NEW.entity_id,NEW.event_type,NEW.payload,NEW.created_at) IS DISTINCT FROM ROW(OLD.id,OLD.organization_id,OLD.entity_id,OLD.event_type,OLD.payload,OLD.created_at)
     OR (OLD.sent_at IS NOT NULL AND NEW.sent_at IS DISTINCT FROM OLD.sent_at) THEN RAISE EXCEPTION 'check source event is immutable'; END IF;
 END IF;
 IF TG_OP='DELETE' THEN RETURN OLD; END IF; RETURN NEW;
END $$;
CREATE TRIGGER review_check_dispatch_event_guard BEFORE UPDATE OR DELETE ON outbox FOR EACH ROW EXECUTE FUNCTION review_check_dispatch_event_guard();
