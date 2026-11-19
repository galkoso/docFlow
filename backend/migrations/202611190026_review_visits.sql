-- A delivery is verified server materialization. Only explicit acknowledgement
-- creates a visit; neither operation creates section or proposal approval.
CREATE TABLE review_source_attempts (
  id uuid PRIMARY KEY,
  organization_id uuid NOT NULL,
  proposal_id uuid NOT NULL,
  snapshot_id uuid NOT NULL,
  reviewer_id uuid NOT NULL REFERENCES users(id),
  domain text NOT NULL CHECK(domain IN ('PRODUCT','DEVELOPMENT','DESIGN')),
  fingerprint text NOT NULL CHECK(fingerprint~'^[a-f0-9]{64}$'),
  unit_id text NOT NULL,
  side text NOT NULL CHECK(side IN ('before','after')),
  start_offset integer NOT NULL CHECK(start_offset BETWEEN 0 AND 2097152),
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  expires_at timestamptz NOT NULL DEFAULT (clock_timestamp()+interval '3 minutes') CHECK(expires_at>created_at AND expires_at<=created_at+interval '181 seconds'),
  created_transaction bigint NOT NULL DEFAULT txid_current(),
  FOREIGN KEY(organization_id,proposal_id,snapshot_id) REFERENCES review_snapshots(organization_id,proposal_id,id),
  FOREIGN KEY(snapshot_id,unit_id) REFERENCES review_snapshot_units(snapshot_id,unit_id)
);
CREATE INDEX review_source_quota ON review_source_attempts(organization_id,created_at,reviewer_id);
CREATE TABLE review_source_deliveries (
  id uuid PRIMARY KEY,
  attempt_id uuid NOT NULL UNIQUE REFERENCES review_source_attempts(id),
  source jsonb NOT NULL CHECK(jsonb_typeof(source)='object' AND octet_length(source::text)<=8192),
  start_byte integer NOT NULL CHECK(start_byte>=0),
  end_byte integer NOT NULL CHECK(end_byte>=start_byte AND end_byte-start_byte<=65536),
  window_sha256 text NOT NULL CHECK(window_sha256~'^[a-f0-9]{64}$'),
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  expires_at timestamptz NOT NULL DEFAULT (clock_timestamp()+interval '30 minutes') CHECK(expires_at>created_at AND expires_at<=created_at+interval '1801 seconds'),
  created_transaction bigint NOT NULL DEFAULT txid_current()
);
CREATE TABLE review_source_visits (
  id uuid PRIMARY KEY,
  organization_id uuid NOT NULL REFERENCES organizations(id),
  delivery_id uuid NOT NULL UNIQUE REFERENCES review_source_deliveries(id),
  request_id uuid NOT NULL,
  request_hash text NOT NULL CHECK(request_hash~'^[a-f0-9]{64}$'),
  audit_id uuid NOT NULL UNIQUE REFERENCES audit_events(id) DEFERRABLE INITIALLY DEFERRED,
  event_id uuid NOT NULL UNIQUE REFERENCES outbox(id) DEFERRABLE INITIALLY DEFERRED,
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  created_transaction bigint NOT NULL DEFAULT txid_current(),
  UNIQUE(organization_id,request_id)
);
CREATE FUNCTION review_visit_scope_current(org uuid,p uuid,h uuid,actor uuid,area text,fp text) RETURNS boolean LANGUAGE sql AS $$
 SELECT EXISTS(SELECT 1 FROM review_snapshots s JOIN proposals x ON x.id=s.proposal_id AND x.organization_id=s.organization_id
   JOIN memberships m ON m.organization_id=x.organization_id AND m.user_id=actor JOIN users u ON u.id=m.user_id
   WHERE s.organization_id=org AND s.proposal_id=p AND s.id=h AND s.state='READY' AND s.review_units_version=1 AND s.fingerprint=fp
     AND pr_sync_snapshot_current(org,p,h) AND x.author_id<>actor AND s.metadata#>>'{context,author_id}'<>actor::text
     AND m.role IN ('REVIEWER','MAINTAINER','ADMIN') AND u.status='ACTIVE' AND area=ANY(m.review_domains)
     AND EXISTS(SELECT 1 FROM jsonb_array_elements(s.metadata->'assignments') a WHERE a->>'reviewer_id'=actor::text AND a->>'domain'=area)
     FOR SHARE OF s,x,m,u)
$$;
CREATE FUNCTION review_source_attempt_guard() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE selected jsonb;
BEGIN
 IF NEW.created_transaction<>txid_current() OR NEW.created_at NOT BETWEEN clock_timestamp()-interval '5 seconds' AND clock_timestamp()+interval '1 second' OR NOT review_visit_scope_current(NEW.organization_id,NEW.proposal_id,NEW.snapshot_id,NEW.reviewer_id,NEW.domain,NEW.fingerprint)
 THEN RAISE EXCEPTION 'source attempt requires current assigned reviewer'; END IF;
 SELECT unit->NEW.side INTO selected FROM review_snapshot_units WHERE snapshot_id=NEW.snapshot_id AND unit_id=NEW.unit_id;
 IF selected IS NULL OR selected='null'::jsonb OR selected->'range'='null'::jsonb
   OR NEW.start_offset>(selected#>>'{range,end_byte}')::integer-(selected#>>'{range,start_byte}')::integer
 THEN RAISE EXCEPTION 'source attempt requires a bounded text unit side'; END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER review_source_attempt_guard BEFORE INSERT ON review_source_attempts FOR EACH ROW EXECUTE FUNCTION review_source_attempt_guard();
CREATE FUNCTION review_source_delivery_guard() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE a review_source_attempts; selected jsonb;
BEGIN
 SELECT * INTO a FROM review_source_attempts WHERE id=NEW.attempt_id FOR SHARE;
 SELECT unit->a.side INTO selected FROM review_snapshot_units WHERE snapshot_id=a.snapshot_id AND unit_id=a.unit_id;
 IF NEW.created_transaction<>txid_current() OR NEW.created_at NOT BETWEEN clock_timestamp()-interval '5 seconds' AND clock_timestamp()+interval '1 second' OR a.expires_at<=clock_timestamp()
   OR NOT review_visit_scope_current(a.organization_id,a.proposal_id,a.snapshot_id,a.reviewer_id,a.domain,a.fingerprint)
   OR selected IS DISTINCT FROM NEW.source OR NEW.start_byte<>(selected#>>'{range,start_byte}')::integer+a.start_offset
   OR NEW.end_byte>(selected#>>'{range,end_byte}')::integer
 THEN RAISE EXCEPTION 'delivery requires current exact admitted source evidence'; END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER review_source_delivery_guard BEFORE INSERT ON review_source_deliveries FOR EACH ROW EXECUTE FUNCTION review_source_delivery_guard();
CREATE FUNCTION review_source_visit_guard() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE a review_source_attempts; d review_source_deliveries;
BEGIN
 SELECT * INTO d FROM review_source_deliveries WHERE id=NEW.delivery_id FOR SHARE;
 SELECT * INTO a FROM review_source_attempts WHERE id=d.attempt_id FOR SHARE;
 IF NEW.created_transaction<>txid_current() OR NEW.created_at NOT BETWEEN clock_timestamp()-interval '5 seconds' AND clock_timestamp()+interval '1 second' OR NEW.organization_id<>a.organization_id OR d.expires_at<=clock_timestamp()
   OR NOT review_visit_scope_current(a.organization_id,a.proposal_id,a.snapshot_id,a.reviewer_id,a.domain,a.fingerprint)
 THEN RAISE EXCEPTION 'visit requires a current unexpired reviewer delivery'; END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER review_source_visit_guard BEFORE INSERT ON review_source_visits FOR EACH ROW EXECUTE FUNCTION review_source_visit_guard();
CREATE FUNCTION review_source_visit_complete() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE a review_source_attempts;
BEGIN
 SELECT t.* INTO a FROM review_source_attempts t JOIN review_source_deliveries d ON d.attempt_id=t.id WHERE d.id=NEW.delivery_id;
 IF NOT EXISTS(SELECT 1 FROM audit_events e JOIN outbox o ON o.id=NEW.event_id WHERE e.id=NEW.audit_id
   AND e.organization_id=a.organization_id AND e.actor_id=a.reviewer_id AND e.entity_id=a.proposal_id
   AND e.action='REVIEW_SOURCE_ACKNOWLEDGED' AND e.after_ref=NEW.id::text AND e.result='RECORDED'
   AND o.organization_id=a.organization_id AND o.entity_id=a.proposal_id AND o.event_type='REVIEW_SOURCE_ACKNOWLEDGED'
   AND o.payload=jsonb_build_object('visit_id',NEW.id,'delivery_id',NEW.delivery_id,'snapshot_id',a.snapshot_id,'unit_id',a.unit_id,'domain',a.domain,'side',a.side))
 THEN RAISE EXCEPTION 'visit requires atomic audit and event'; END IF;
 RETURN NULL;
END $$;
CREATE CONSTRAINT TRIGGER review_source_visit_complete AFTER INSERT ON review_source_visits DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION review_source_visit_complete();
DO $$ DECLARE t text; BEGIN FOREACH t IN ARRAY ARRAY['review_source_attempts','review_source_deliveries','review_source_visits'] LOOP
 EXECUTE format('CREATE TRIGGER %I BEFORE UPDATE OR DELETE ON %I FOR EACH ROW EXECUTE FUNCTION review_submission_immutable()',t||'_immutable',t);
 EXECUTE format('CREATE TRIGGER %I BEFORE TRUNCATE ON %I FOR EACH STATEMENT EXECUTE FUNCTION review_submission_immutable()',t||'_no_truncate',t);
END LOOP; END $$;
CREATE FUNCTION review_visit_event_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF EXISTS(SELECT 1 FROM review_source_visits WHERE event_id=OLD.id) THEN
  IF TG_OP='DELETE' THEN RAISE EXCEPTION 'visit event is durable'; END IF;
  IF ROW(NEW.id,NEW.organization_id,NEW.event_type,NEW.entity_id,NEW.payload,NEW.created_at)
    IS DISTINCT FROM ROW(OLD.id,OLD.organization_id,OLD.event_type,OLD.entity_id,OLD.payload,OLD.created_at)
    OR (OLD.sent_at IS NOT NULL AND NEW.sent_at IS DISTINCT FROM OLD.sent_at) THEN RAISE EXCEPTION 'visit event scope is immutable'; END IF;
 END IF;
 IF TG_OP='DELETE' THEN RETURN OLD; END IF; RETURN NEW;
END $$;
CREATE TRIGGER review_visit_event_guard BEFORE UPDATE OR DELETE ON outbox FOR EACH ROW EXECUTE FUNCTION review_visit_event_guard();
