-- Validate provenance without treating event-owned UUIDs/flags as authority.
CREATE FUNCTION pr_sync_publication_actor(event uuid) RETURNS uuid LANGUAGE sql STABLE AS $$
  SELECT s.actor_id FROM outbox e JOIN review_submission_results x
    ON x.operation_id=CASE WHEN e.payload->>'operation_id'~'^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$'
      THEN (e.payload->>'operation_id')::uuid ELSE NULL END
    AND x.organization_id=e.organization_id AND x.proposal_id=e.entity_id
  JOIN review_submissions s ON s.operation_id=x.operation_id AND s.organization_id=x.organization_id AND s.proposal_id=x.proposal_id
  JOIN review_snapshots h ON h.id=x.snapshot_id AND h.organization_id=x.organization_id AND h.proposal_id=x.proposal_id
  JOIN git_operations o ON o.id=s.operation_id AND o.organization_id=s.organization_id
  JOIN jobs j ON j.id=s.job_id AND j.operation_id=o.id AND j.organization_id=o.organization_id
  WHERE e.id=event AND e.event_type='REVIEW_SNAPSHOT_PUBLISHED' AND h.state='READY'
    AND o.kind='SUBMIT' AND o.state='SUCCEEDED' AND o.result_sha=h.head_sha
    AND j.kind='SUBMIT_REVIEW' AND j.state='SUCCEEDED'
    AND j.result=jsonb_build_object('phase','SNAPSHOT_READY','snapshot_id',h.id)
    AND e.payload=jsonb_build_object('snapshot_id',h.id,'operation_id',s.operation_id,'activated',x.activated,'pr_sync_required',true,'approvals_granted',false)
$$;
CREATE FUNCTION pr_sync_snapshot_current(org uuid,proposal uuid,snapshot uuid) RETURNS boolean LANGUAGE sql STABLE AS $$
  SELECT EXISTS(SELECT 1 FROM review_snapshots h JOIN proposals p ON p.active_review_snapshot_id=h.id
      AND p.organization_id=h.organization_id AND p.id=h.proposal_id
    JOIN repositories r ON r.id=p.repository_id AND r.organization_id=p.organization_id
    JOIN organizations g ON g.id=p.organization_id
    WHERE h.id=snapshot AND h.organization_id=org AND h.proposal_id=proposal AND h.state='READY'
      AND p.state IN ('IN_REVIEW','APPROVED','CHANGES_REQUESTED') AND p.repository_id=h.repository_id
      AND p.head_sha=h.head_sha AND p.target_sha=h.target_sha AND p.base_sha=h.recorded_base_sha AND p.branch=h.branch
      AND p.review_configuration_version=h.configuration_version AND g.policy_version=h.policy_version
      AND r.provider='GITHUB' AND r.default_branch='main' AND ltrim(r.remote_id,'0+')=h.remote_id
      AND ltrim(r.installation_id,'0+')=h.installation_id)
$$;
CREATE TABLE pr_sync_dispatch (
  event_id uuid PRIMARY KEY REFERENCES outbox(id),
  organization_id uuid NOT NULL REFERENCES organizations(id),
  proposal_id uuid NOT NULL,
  state text NOT NULL CHECK(state IN ('WAITING','ENQUEUED','SKIPPED','REJECTED')),
  reason text NOT NULL CHECK(reason~'^[A-Z0-9_]{1,100}$'),
  attempts integer NOT NULL CHECK(attempts>0),
  next_at timestamptz,
  operation_id uuid REFERENCES pr_sync_intents(operation_id),
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  CHECK((state='WAITING')=(next_at IS NOT NULL)),
  CHECK((state='ENQUEUED')=(operation_id IS NOT NULL))
);
CREATE INDEX pr_dispatch_retry ON pr_sync_dispatch(next_at,event_id) WHERE state='WAITING';
CREATE INDEX pr_dispatch_pending ON outbox(created_at,id)
  WHERE event_type='REVIEW_SNAPSHOT_PUBLISHED' AND sent_at IS NULL;
CREATE FUNCTION pr_dispatch_guard() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE e outbox%ROWTYPE;
BEGIN
  IF TG_OP='DELETE' THEN RAISE EXCEPTION 'PR dispatch history is durable'; END IF;
  IF TG_OP='INSERT' AND NEW.attempts<>1 THEN RAISE EXCEPTION 'PR dispatch starts with one attempt'; END IF;
  IF TG_OP='UPDATE' AND (OLD.state<>'WAITING'
      OR ROW(NEW.event_id,NEW.organization_id,NEW.proposal_id,NEW.created_at)
        IS DISTINCT FROM ROW(OLD.event_id,OLD.organization_id,OLD.proposal_id,OLD.created_at)
      OR NEW.attempts<>least(OLD.attempts::bigint+1,2147483647)
      OR NEW.updated_at<OLD.updated_at) THEN RAISE EXCEPTION 'PR dispatch scope and terminal outcomes are immutable'; END IF;
  SELECT * INTO e FROM outbox WHERE id=NEW.event_id FOR UPDATE;
  IF e.id IS NULL OR e.organization_id<>NEW.organization_id OR e.entity_id<>NEW.proposal_id
      OR e.event_type<>'REVIEW_SNAPSHOT_PUBLISHED' THEN RAISE EXCEPTION 'PR dispatch requires its exact publication envelope'; END IF;
  IF NEW.state='ENQUEUED' THEN
    IF NOT EXISTS(SELECT 1 FROM pr_sync_intents i WHERE i.operation_id=NEW.operation_id AND i.organization_id=NEW.organization_id
        AND i.proposal_id=NEW.proposal_id AND i.source_submission::text=e.payload->>'operation_id'
        AND pr_sync_publication_actor(e.id)=i.actor_id) THEN RAISE EXCEPTION 'PR delivery requires its backed frozen intent'; END IF;
  ELSIF NEW.state='SKIPPED' THEN
    IF pr_sync_publication_actor(e.id) IS NULL OR pr_sync_snapshot_current(NEW.organization_id,NEW.proposal_id,(e.payload->>'snapshot_id')::uuid) THEN
      RAISE EXCEPTION 'PR skip requires a superseded backed publication'; END IF;
  END IF;
  IF NEW.state<>'ENQUEUED' AND EXISTS(SELECT 1 FROM pr_sync_intents WHERE source_event=e.id) THEN
    RAISE EXCEPTION 'admitted PR work cannot be discarded by dispatch'; END IF;
  RETURN NEW;
END $$;
CREATE TRIGGER pr_dispatch_guard BEFORE INSERT OR UPDATE OR DELETE ON pr_sync_dispatch FOR EACH ROW EXECUTE FUNCTION pr_dispatch_guard();
CREATE TRIGGER pr_dispatch_no_truncate BEFORE TRUNCATE ON pr_sync_dispatch FOR EACH STATEMENT EXECUTE FUNCTION review_submission_immutable();
CREATE FUNCTION pr_dispatch_complete() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE event uuid; d pr_sync_dispatch%ROWTYPE; delivered boolean;
BEGIN
  IF TG_TABLE_NAME='outbox' THEN event:=NEW.id; ELSE event:=NEW.event_id; END IF;
  SELECT * INTO d FROM pr_sync_dispatch WHERE event_id=event;
  IF d.event_id IS NULL THEN RETURN NULL; END IF;
  SELECT sent_at IS NOT NULL INTO delivered FROM outbox WHERE id=event;
  IF delivered IS DISTINCT FROM (d.state<>'WAITING') OR NOT EXISTS(SELECT 1 FROM audit_events
      WHERE organization_id=d.organization_id AND entity_id=d.proposal_id AND action='PR_OUTBOX_DISPATCH'
      AND after_ref=event::text AND result=d.state||':'||d.reason AND operation_id IS NOT DISTINCT FROM d.operation_id) THEN
    RAISE EXCEPTION 'PR dispatch result, audit and delivery must commit together';
  END IF;
  RETURN NULL;
END $$;
CREATE CONSTRAINT TRIGGER pr_dispatch_complete AFTER INSERT OR UPDATE ON pr_sync_dispatch DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION pr_dispatch_complete();
CREATE CONSTRAINT TRIGGER pr_dispatch_event_complete AFTER UPDATE ON outbox DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION pr_dispatch_complete();
CREATE FUNCTION pr_dispatch_event_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF EXISTS(SELECT 1 FROM pr_sync_dispatch WHERE event_id=OLD.id) THEN
    IF TG_OP='DELETE' THEN RAISE EXCEPTION 'dispatched event is durable'; END IF;
    IF ROW(NEW.id,NEW.organization_id,NEW.entity_id,NEW.event_type,NEW.payload,NEW.created_at)
       IS DISTINCT FROM ROW(OLD.id,OLD.organization_id,OLD.entity_id,OLD.event_type,OLD.payload,OLD.created_at)
       OR (OLD.sent_at IS NOT NULL AND NEW.sent_at IS DISTINCT FROM OLD.sent_at) THEN
      RAISE EXCEPTION 'dispatched event scope is immutable';
    END IF;
  END IF;
  IF TG_OP='DELETE' THEN RETURN OLD; END IF; RETURN NEW;
END $$;
CREATE TRIGGER pr_dispatch_event_guard BEFORE UPDATE OR DELETE ON outbox FOR EACH ROW EXECUTE FUNCTION pr_dispatch_event_guard();
-- Existing journal delivery remains delivery. Never create a second operation
-- while upgrading deployments that already admitted publication events.
INSERT INTO pr_sync_dispatch(event_id,organization_id,proposal_id,state,reason,attempts,operation_id)
  SELECT source_event,organization_id,proposal_id,'ENQUEUED','EXISTING_INTENT',1,operation_id FROM pr_sync_intents;
INSERT INTO audit_events(id,organization_id,action,entity_id,after_ref,operation_id,result)
  SELECT gen_random_uuid(),organization_id,'PR_OUTBOX_DISPATCH',proposal_id,event_id::text,operation_id,'ENQUEUED:EXISTING_INTENT' FROM pr_sync_dispatch;
