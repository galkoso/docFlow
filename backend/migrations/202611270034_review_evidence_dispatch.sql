-- Only an exact immutable source record and its audit can authorize delivery.
-- Event payloads never supply publication authority or an evaluation result.
CREATE FUNCTION review_check_event_actor(event uuid) RETURNS uuid LANGUAGE sql STABLE AS $$
 SELECT pr_sync_publication_actor(event) FROM outbox e
 WHERE e.id=event AND e.event_type='REVIEW_SNAPSHOT_PUBLISHED'
 UNION ALL
 SELECT r.reviewer_id FROM outbox e JOIN official_reviews r ON r.event_id=e.id
 JOIN audit_events a ON a.id=r.audit_id
 WHERE e.id=event AND e.event_type='REVIEW_DECISION_RECORDED'
   AND e.organization_id=r.organization_id AND e.entity_id=r.proposal_id
   AND a.organization_id=r.organization_id AND a.actor_id=r.reviewer_id AND a.entity_id=r.proposal_id
   AND a.action='OFFICIAL_REVIEW_RECORDED' AND a.after_ref=r.id::text AND a.result=r.decision
   AND e.payload=jsonb_build_object('review_id',r.id,'snapshot_id',r.snapshot_id,'head_sha',r.head_sha,'target_sha',r.target_sha,'decision',r.decision)
 UNION ALL
 SELECT r.reviewer_id FROM outbox e JOIN review_unit_approvals r ON r.event_id=e.id
 JOIN audit_events a ON a.id=r.audit_id
 WHERE e.id=event AND e.event_type='REVIEW_UNIT_APPROVED'
   AND e.organization_id=r.organization_id AND e.entity_id=r.proposal_id
   AND a.organization_id=r.organization_id AND a.actor_id=r.reviewer_id AND a.entity_id=r.proposal_id
   AND a.action='REVIEW_UNIT_APPROVED' AND a.after_ref=r.id::text AND a.result='RECORDED'
   AND e.payload=jsonb_build_object('approval_id',r.id,'snapshot_id',r.snapshot_id,'unit_id',r.unit_id,'domain',r.domain)
 UNION ALL
 SELECT t.reviewer_id FROM outbox e JOIN review_source_visits v ON v.event_id=e.id
 JOIN review_source_deliveries d ON d.id=v.delivery_id
 JOIN review_source_attempts t ON t.id=d.attempt_id JOIN audit_events a ON a.id=v.audit_id
 WHERE e.id=event AND e.event_type='REVIEW_SOURCE_ACKNOWLEDGED'
   AND v.organization_id=t.organization_id
   AND e.organization_id=t.organization_id AND e.entity_id=t.proposal_id
   AND a.organization_id=t.organization_id AND a.actor_id=t.reviewer_id AND a.entity_id=t.proposal_id
   AND a.action='REVIEW_SOURCE_ACKNOWLEDGED' AND a.after_ref=v.id::text AND a.result='RECORDED'
   AND e.payload=jsonb_build_object('visit_id',v.id,'delivery_id',v.delivery_id,'snapshot_id',t.snapshot_id,'unit_id',t.unit_id,'domain',t.domain,'side',t.side)
$$;

CREATE INDEX review_check_evidence_events ON outbox(created_at,id)
 WHERE event_type IN ('REVIEW_DECISION_RECORDED','REVIEW_UNIT_APPROVED','REVIEW_SOURCE_ACKNOWLEDGED');

CREATE OR REPLACE FUNCTION review_check_source_insert() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF NEW.source_event IS NOT NULL AND NOT EXISTS(
   SELECT 1 FROM review_check_dispatch d JOIN outbox e ON e.id=d.event_id
   WHERE d.event_id=NEW.source_event AND d.organization_id=NEW.organization_id AND d.proposal_id=NEW.proposal_id
     AND d.state='RUNNING' AND d.lease_owner=NEW.dispatch_worker AND d.attempts=NEW.dispatch_attempt AND d.leased_until>clock_timestamp()
     AND review_check_event_actor(e.id)=NEW.actor_id AND e.payload->>'snapshot_id'=NEW.snapshot_id::text
   FOR SHARE OF d) THEN RAISE EXCEPTION 'check admission requires its live delivery fence and backed publication'; END IF;
 RETURN NEW;
END $$;

CREATE OR REPLACE FUNCTION review_check_dispatch_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF TG_OP='DELETE' THEN RAISE EXCEPTION 'check delivery is durable'; END IF;
 IF NOT EXISTS(SELECT 1 FROM outbox WHERE id=NEW.event_id AND organization_id=NEW.organization_id AND entity_id=NEW.proposal_id
   AND event_type IN ('REVIEW_SNAPSHOT_PUBLISHED','REVIEW_DECISION_RECORDED','REVIEW_UNIT_APPROVED','REVIEW_SOURCE_ACKNOWLEDGED')) THEN
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
 IF NEW.state='SKIPPED' AND (review_check_event_actor(NEW.event_id) IS NULL OR pr_sync_snapshot_current(NEW.organization_id,NEW.proposal_id,(SELECT (payload->>'snapshot_id')::uuid FROM outbox WHERE id=NEW.event_id))) THEN
   RAISE EXCEPTION 'check skip requires a superseded backed publication'; END IF;
 RETURN NEW;
END $$;
