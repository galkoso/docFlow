-- Official decisions are immutable. A comment never replaces the effective
-- approval/request-for-changes pointer for a reviewer/domain/snapshot.
CREATE TABLE official_reviews (
  id uuid PRIMARY KEY,
  revision bigint GENERATED ALWAYS AS IDENTITY UNIQUE,
  organization_id uuid NOT NULL,
  proposal_id uuid NOT NULL,
  snapshot_id uuid NOT NULL,
  reviewer_id uuid NOT NULL REFERENCES users(id),
  request_id uuid NOT NULL,
  request_hash text NOT NULL CHECK(request_hash~'^[a-f0-9]{64}$'),
  domain text NOT NULL CHECK(domain IN ('PRODUCT','DEVELOPMENT','DESIGN')),
  decision text NOT NULL CHECK(decision IN ('APPROVE_ALL','REQUEST_CHANGES','COMMENT')),
  explanation text NOT NULL CHECK(length(btrim(explanation)) BETWEEN 1 AND 10000),
  fingerprint text NOT NULL CHECK(fingerprint~'^[a-f0-9]{64}$'),
  head_sha text NOT NULL,
  target_sha text NOT NULL,
  policy_version bigint NOT NULL CHECK(policy_version>0),
  configuration_version bigint NOT NULL CHECK(configuration_version>0),
  audit_id uuid NOT NULL UNIQUE REFERENCES audit_events(id) DEFERRABLE INITIALLY DEFERRED,
  event_id uuid NOT NULL UNIQUE REFERENCES outbox(id) DEFERRABLE INITIALLY DEFERRED,
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  created_transaction bigint NOT NULL DEFAULT txid_current(),
  UNIQUE(organization_id,request_id),
  UNIQUE(organization_id,proposal_id,snapshot_id,reviewer_id,domain,id),
  FOREIGN KEY(organization_id,proposal_id,snapshot_id) REFERENCES review_snapshots(organization_id,proposal_id,id)
);
CREATE INDEX official_review_history ON official_reviews(organization_id,proposal_id,snapshot_id,revision DESC);
CREATE INDEX official_review_quota ON official_reviews(organization_id,created_at,reviewer_id);
CREATE TABLE official_review_effective (
  organization_id uuid NOT NULL,
  proposal_id uuid NOT NULL,
  snapshot_id uuid NOT NULL,
  reviewer_id uuid NOT NULL,
  domain text NOT NULL,
  review_id uuid NOT NULL,
  PRIMARY KEY(organization_id,proposal_id,snapshot_id,reviewer_id,domain),
  FOREIGN KEY(organization_id,proposal_id,snapshot_id,reviewer_id,domain,review_id)
    REFERENCES official_reviews(organization_id,proposal_id,snapshot_id,reviewer_id,domain,id)
);
CREATE FUNCTION official_review_insert() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF NEW.created_transaction<>txid_current() OR NOT EXISTS(
    SELECT 1 FROM review_snapshots h JOIN proposals p ON p.id=h.proposal_id AND p.organization_id=h.organization_id
      JOIN memberships m ON m.organization_id=p.organization_id AND m.user_id=NEW.reviewer_id
      JOIN users u ON u.id=m.user_id
    WHERE h.id=NEW.snapshot_id AND h.organization_id=NEW.organization_id AND h.proposal_id=NEW.proposal_id
      AND h.state='READY' AND h.fingerprint=NEW.fingerprint AND h.head_sha=NEW.head_sha AND h.target_sha=NEW.target_sha
      AND h.policy_version=NEW.policy_version AND h.configuration_version=NEW.configuration_version
      AND pr_sync_snapshot_current(h.organization_id,h.proposal_id,h.id)
      AND p.author_id<>NEW.reviewer_id AND h.metadata#>>'{context,author_id}'<>NEW.reviewer_id::text
      AND m.role IN ('REVIEWER','MAINTAINER','ADMIN') AND u.status='ACTIVE' AND NEW.domain=ANY(m.review_domains)
      AND EXISTS(SELECT 1 FROM jsonb_array_elements(h.metadata->'assignments') a WHERE a->>'reviewer_id'=NEW.reviewer_id::text AND a->>'domain'=NEW.domain)
      AND (NEW.decision<>'APPROVE_ALL' OR (h.recorded_base_sha=h.target_sha AND h.merge_base_sha=h.target_sha AND h.metadata#>>'{summary,ui_impact}' IN ('YES','NO')))
      FOR SHARE OF h,p,m,u
  ) THEN RAISE EXCEPTION 'official review requires current qualified assignment and snapshot'; END IF;
  RETURN NEW;
END $$;
CREATE TRIGGER official_review_insert BEFORE INSERT ON official_reviews FOR EACH ROW EXECUTE FUNCTION official_review_insert();
CREATE TRIGGER official_review_immutable BEFORE UPDATE OR DELETE ON official_reviews FOR EACH ROW EXECUTE FUNCTION review_submission_immutable();
CREATE TRIGGER official_review_no_truncate BEFORE TRUNCATE ON official_reviews FOR EACH STATEMENT EXECUTE FUNCTION review_submission_immutable();
CREATE FUNCTION official_review_pointer_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF TG_OP='DELETE' THEN RAISE EXCEPTION 'official review history is durable'; END IF;
  IF TG_OP='UPDATE' AND ROW(NEW.organization_id,NEW.proposal_id,NEW.snapshot_id,NEW.reviewer_id,NEW.domain)
    IS DISTINCT FROM ROW(OLD.organization_id,OLD.proposal_id,OLD.snapshot_id,OLD.reviewer_id,OLD.domain) THEN
    RAISE EXCEPTION 'official review scope is immutable'; END IF;
  IF NOT EXISTS(SELECT 1 FROM official_reviews r WHERE r.id=NEW.review_id AND r.organization_id=NEW.organization_id
      AND r.proposal_id=NEW.proposal_id AND r.snapshot_id=NEW.snapshot_id AND r.reviewer_id=NEW.reviewer_id AND r.domain=NEW.domain
      AND r.decision<>'COMMENT' AND r.created_transaction=txid_current()) THEN
    RAISE EXCEPTION 'effective decision requires a new official review'; END IF;
  IF TG_OP='UPDATE' AND (SELECT revision FROM official_reviews WHERE id=NEW.review_id)
      <=(SELECT revision FROM official_reviews WHERE id=OLD.review_id) THEN RAISE EXCEPTION 'official decision cannot move backwards'; END IF;
  RETURN NEW;
END $$;
CREATE TRIGGER official_review_pointer_guard BEFORE INSERT OR UPDATE OR DELETE ON official_review_effective FOR EACH ROW EXECUTE FUNCTION official_review_pointer_guard();
CREATE TRIGGER official_review_pointer_no_truncate BEFORE TRUNCATE ON official_review_effective FOR EACH STATEMENT EXECUTE FUNCTION review_submission_immutable();
CREATE FUNCTION official_review_complete() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF NOT EXISTS(SELECT 1 FROM audit_events a JOIN outbox e ON e.id=NEW.event_id
      WHERE a.id=NEW.audit_id AND a.organization_id=NEW.organization_id AND a.actor_id=NEW.reviewer_id
      AND a.entity_id=NEW.proposal_id AND a.action='OFFICIAL_REVIEW_RECORDED' AND a.after_ref=NEW.id::text AND a.result=NEW.decision
      AND e.organization_id=NEW.organization_id AND e.entity_id=NEW.proposal_id AND e.event_type='REVIEW_DECISION_RECORDED'
      AND e.payload=jsonb_build_object('review_id',NEW.id,'snapshot_id',NEW.snapshot_id,'head_sha',NEW.head_sha,'target_sha',NEW.target_sha,'decision',NEW.decision)
    ) OR (NEW.decision<>'COMMENT' AND NOT EXISTS(SELECT 1 FROM official_review_effective x
      WHERE x.organization_id=NEW.organization_id AND x.proposal_id=NEW.proposal_id AND x.snapshot_id=NEW.snapshot_id
      AND x.reviewer_id=NEW.reviewer_id AND x.domain=NEW.domain AND x.review_id=NEW.id)) THEN
    RAISE EXCEPTION 'official review requires atomic audit, event and effective decision'; END IF;
  RETURN NULL;
END $$;
CREATE CONSTRAINT TRIGGER official_review_complete AFTER INSERT ON official_reviews DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION official_review_complete();
CREATE FUNCTION official_review_event_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF EXISTS(SELECT 1 FROM official_reviews WHERE event_id=OLD.id) THEN
    IF TG_OP='DELETE' THEN RAISE EXCEPTION 'official review event is durable'; END IF;
    IF ROW(NEW.id,NEW.organization_id,NEW.event_type,NEW.entity_id,NEW.payload,NEW.created_at)
      IS DISTINCT FROM ROW(OLD.id,OLD.organization_id,OLD.event_type,OLD.entity_id,OLD.payload,OLD.created_at)
      OR (OLD.sent_at IS NOT NULL AND NEW.sent_at IS DISTINCT FROM OLD.sent_at) THEN RAISE EXCEPTION 'official review event scope is immutable'; END IF;
  END IF;
  IF TG_OP='DELETE' THEN RETURN OLD; END IF; RETURN NEW;
END $$;
CREATE TRIGGER official_review_event_guard BEFORE UPDATE OR DELETE ON outbox FOR EACH ROW EXECUTE FUNCTION official_review_event_guard();
