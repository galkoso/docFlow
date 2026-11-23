-- One coverage predicate is shared by reads, warnings and approval guards.
CREATE FUNCTION review_unit_coverage(org uuid,p uuid,h uuid,actor uuid,area text,fp text,wanted text DEFAULT NULL)
RETURNS TABLE(ordinal integer,unit_id text,before_state text,after_state text,viewed boolean,unsupported boolean)
LANGUAGE sql STABLE AS $$
WITH acknowledged AS (
 SELECT a.unit_id,a.side,range_agg(int8range(d.start_byte::bigint,d.end_byte::bigint,'[)')) AS covered,count(*) AS seen
 FROM review_source_attempts a JOIN review_source_deliveries d ON d.attempt_id=a.id
 JOIN review_source_visits v ON v.delivery_id=d.id AND v.organization_id=a.organization_id
 JOIN review_snapshot_units u ON u.snapshot_id=a.snapshot_id AND u.unit_id=a.unit_id AND u.organization_id=a.organization_id AND u.proposal_id=a.proposal_id
 WHERE a.organization_id=org AND a.proposal_id=p AND a.snapshot_id=h AND a.reviewer_id=actor AND a.domain=area AND a.fingerprint=fp
   AND d.source=u.unit->a.side AND (wanted IS NULL OR u.unit_id=wanted)
 GROUP BY a.unit_id,a.side
), sides AS (
 SELECT u.ordinal,u.unit_id,s.side,
 CASE WHEN s.source IS NULL OR s.source='null'::jsonb THEN 'absent'
  WHEN s.source->'range'='null'::jsonb THEN 'unsupported'
  WHEN (s.source#>>'{range,start_byte}')::bigint=(s.source#>>'{range,end_byte}')::bigint AND coalesce(a.seen,0)>0 THEN 'viewed'
  WHEN (s.source#>>'{range,start_byte}')::bigint<(s.source#>>'{range,end_byte}')::bigint
    AND coalesce(a.covered @> int8range((s.source#>>'{range,start_byte}')::bigint,(s.source#>>'{range,end_byte}')::bigint,'[)'),false) THEN 'viewed'
  WHEN coalesce(a.covered && int8range((s.source#>>'{range,start_byte}')::bigint,(s.source#>>'{range,end_byte}')::bigint,'[)'),false) THEN 'partial'
  ELSE 'unviewed' END AS state
 FROM review_snapshot_units u CROSS JOIN LATERAL (VALUES ('before',u.unit->'before'),('after',u.unit->'after')) s(side,source)
 LEFT JOIN acknowledged a ON a.unit_id=u.unit_id AND a.side=s.side
 WHERE u.organization_id=org AND u.proposal_id=p AND u.snapshot_id=h AND (wanted IS NULL OR u.unit_id=wanted)
), units AS (
 SELECT ordinal,unit_id,max(state) FILTER(WHERE side='before') AS before_state,max(state) FILTER(WHERE side='after') AS after_state,
   bool_and(state IN ('absent','viewed')) AND bool_or(state<>'absent') AS viewed,bool_or(state='unsupported') AS unsupported
 FROM sides GROUP BY ordinal,unit_id
)
SELECT ordinal,unit_id,before_state,after_state,viewed,unsupported FROM units ORDER BY ordinal
$$;

-- Explicit individual approvals never update Git, proposal state or official decisions.
CREATE TABLE review_unit_approvals (
 id uuid PRIMARY KEY,
 organization_id uuid NOT NULL,
 proposal_id uuid NOT NULL,
 snapshot_id uuid NOT NULL,
 reviewer_id uuid NOT NULL REFERENCES users(id),
 domain text NOT NULL CHECK(domain IN ('PRODUCT','DEVELOPMENT','DESIGN')),
 fingerprint text NOT NULL CHECK(fingerprint~'^[a-f0-9]{64}$'),
 unit_id text NOT NULL,
 request_id uuid NOT NULL CHECK(request_id<>'00000000-0000-0000-0000-000000000000'::uuid),
 request_hash text NOT NULL CHECK(request_hash~'^[a-f0-9]{64}$'),
 audit_id uuid NOT NULL UNIQUE REFERENCES audit_events(id) DEFERRABLE INITIALLY DEFERRED,
 event_id uuid NOT NULL UNIQUE REFERENCES outbox(id) DEFERRABLE INITIALLY DEFERRED,
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 created_transaction bigint NOT NULL DEFAULT txid_current(),
 UNIQUE(organization_id,request_id),
 FOREIGN KEY(organization_id,proposal_id,snapshot_id) REFERENCES review_snapshots(organization_id,proposal_id,id),
 FOREIGN KEY(snapshot_id,unit_id) REFERENCES review_snapshot_units(snapshot_id,unit_id)
);
CREATE INDEX review_unit_approval_scope ON review_unit_approvals(organization_id,proposal_id,snapshot_id,reviewer_id,domain,fingerprint,unit_id,created_at DESC,id);
CREATE INDEX review_unit_approval_quota ON review_unit_approvals(organization_id,created_at,reviewer_id);
CREATE FUNCTION review_unit_approval_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF NEW.created_transaction<>txid_current()
   OR NEW.created_at NOT BETWEEN clock_timestamp()-interval '5 seconds' AND clock_timestamp()+interval '1 second'
   OR NOT review_visit_scope_current(NEW.organization_id,NEW.proposal_id,NEW.snapshot_id,NEW.reviewer_id,NEW.domain,NEW.fingerprint)
   OR NOT EXISTS(SELECT 1 FROM review_unit_coverage(NEW.organization_id,NEW.proposal_id,NEW.snapshot_id,NEW.reviewer_id,NEW.domain,NEW.fingerprint,NEW.unit_id) c WHERE c.viewed)
 THEN RAISE EXCEPTION 'individual approval requires current assigned reviewer and complete acknowledged source'; END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER review_unit_approval_guard BEFORE INSERT ON review_unit_approvals FOR EACH ROW EXECUTE FUNCTION review_unit_approval_guard();
CREATE FUNCTION review_unit_approval_complete() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF NOT EXISTS(SELECT 1 FROM audit_events e JOIN outbox o ON o.id=NEW.event_id WHERE e.id=NEW.audit_id
   AND e.organization_id=NEW.organization_id AND e.actor_id=NEW.reviewer_id AND e.entity_id=NEW.proposal_id
   AND e.action='REVIEW_UNIT_APPROVED' AND e.after_ref=NEW.id::text AND e.result='RECORDED'
   AND o.organization_id=NEW.organization_id AND o.entity_id=NEW.proposal_id AND o.event_type='REVIEW_UNIT_APPROVED'
   AND o.payload=jsonb_build_object('approval_id',NEW.id,'snapshot_id',NEW.snapshot_id,'unit_id',NEW.unit_id,'domain',NEW.domain))
 THEN RAISE EXCEPTION 'individual approval requires atomic audit and event'; END IF;
 RETURN NULL;
END $$;
CREATE CONSTRAINT TRIGGER review_unit_approval_complete AFTER INSERT ON review_unit_approvals DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION review_unit_approval_complete();
CREATE TRIGGER review_unit_approvals_immutable BEFORE UPDATE OR DELETE ON review_unit_approvals FOR EACH ROW EXECUTE FUNCTION review_submission_immutable();
CREATE TRIGGER review_unit_approvals_no_truncate BEFORE TRUNCATE ON review_unit_approvals FOR EACH STATEMENT EXECUTE FUNCTION review_submission_immutable();
CREATE FUNCTION review_unit_approval_event_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF EXISTS(SELECT 1 FROM review_unit_approvals WHERE event_id=OLD.id) THEN
  IF TG_OP='DELETE' THEN RAISE EXCEPTION 'individual approval event is durable'; END IF;
  IF ROW(NEW.id,NEW.organization_id,NEW.event_type,NEW.entity_id,NEW.payload,NEW.created_at)
    IS DISTINCT FROM ROW(OLD.id,OLD.organization_id,OLD.event_type,OLD.entity_id,OLD.payload,OLD.created_at)
    OR (OLD.sent_at IS NOT NULL AND NEW.sent_at IS DISTINCT FROM OLD.sent_at) THEN RAISE EXCEPTION 'individual approval event scope is immutable'; END IF;
 END IF;
 IF TG_OP='DELETE' THEN RETURN OLD; END IF; RETURN NEW;
END $$;
CREATE TRIGGER review_unit_approval_event_guard BEFORE UPDATE OR DELETE ON outbox FOR EACH ROW EXECUTE FUNCTION review_unit_approval_event_guard();
