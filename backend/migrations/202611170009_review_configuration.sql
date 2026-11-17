-- Versioned review metadata only. No snapshot, decision, PR or approval is
-- manufactured by configuring reviewers; publication is a separate workflow.
ALTER TABLE proposals ADD COLUMN review_configuration_version bigint NOT NULL DEFAULT 1
  CHECK(review_configuration_version>0);
CREATE TABLE proposal_review_configurations (
  organization_id uuid NOT NULL,
  proposal_id uuid NOT NULL,
  version bigint NOT NULL CHECK(version>0),
  ui_impact text NOT NULL CHECK(ui_impact IN ('YES','NO','UNDECIDED')),
  ui_impact_reason text CHECK(length(ui_impact_reason) BETWEEN 1 AND 10000),
  assignments jsonb NOT NULL CHECK(jsonb_typeof(assignments)='array' AND jsonb_array_length(assignments)<=200 AND octet_length(assignments::text)<=65536),
  created_by uuid NOT NULL REFERENCES users(id),
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  created_transaction bigint NOT NULL DEFAULT txid_current(),
  PRIMARY KEY(organization_id,proposal_id,version),
  CHECK(ui_impact<>'NO' OR length(trim(coalesce(ui_impact_reason,'')))>0),
  FOREIGN KEY(organization_id,proposal_id) REFERENCES proposals(organization_id,id)
);
CREATE TABLE proposal_review_assignments (
  organization_id uuid NOT NULL,
  proposal_id uuid NOT NULL,
  configuration_version bigint NOT NULL,
  reviewer_id uuid NOT NULL REFERENCES users(id),
  domain text NOT NULL CHECK(domain IN ('PRODUCT','DEVELOPMENT','DESIGN')),
  required boolean NOT NULL,
  PRIMARY KEY(proposal_id,configuration_version,reviewer_id,domain),
  FOREIGN KEY(organization_id,proposal_id,configuration_version) REFERENCES proposal_review_configurations(organization_id,proposal_id,version)
);
CREATE FUNCTION review_configuration_insert_guard() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE current_version bigint;
BEGIN
  SELECT review_configuration_version INTO current_version FROM proposals
    WHERE organization_id=NEW.organization_id AND id=NEW.proposal_id FOR UPDATE;
  IF current_version IS NULL OR NOT (NEW.version=current_version+1 OR
      (current_version=1 AND NEW.version=1 AND NOT EXISTS(SELECT 1 FROM proposal_review_configurations WHERE proposal_id=NEW.proposal_id))) THEN
    RAISE EXCEPTION 'review configuration revision is not next';
  END IF;
  NEW.created_transaction:=txid_current();
  RETURN NEW;
END $$;
CREATE TRIGGER review_configuration_insert BEFORE INSERT ON proposal_review_configurations
  FOR EACH ROW EXECUTE FUNCTION review_configuration_insert_guard();
CREATE FUNCTION review_assignment_insert_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF NOT EXISTS(SELECT 1 FROM proposal_review_configurations WHERE organization_id=NEW.organization_id
      AND proposal_id=NEW.proposal_id AND version=NEW.configuration_version AND created_transaction=txid_current()) THEN
    RAISE EXCEPTION 'review assignments are immutable after their publication transaction';
  END IF;
  RETURN NEW;
END $$;
CREATE TRIGGER review_assignment_insert BEFORE INSERT ON proposal_review_assignments
  FOR EACH ROW EXECUTE FUNCTION review_assignment_insert_guard();
CREATE FUNCTION review_configuration_complete_guard() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE actual jsonb; expected jsonb; revision bigint;
BEGIN
  IF TG_TABLE_NAME='proposal_review_configurations' THEN revision:=NEW.version;
  ELSE revision:=NEW.configuration_version; END IF;
  SELECT assignments INTO expected FROM proposal_review_configurations WHERE organization_id=NEW.organization_id
    AND proposal_id=NEW.proposal_id AND version=revision;
  SELECT coalesce(jsonb_agg(jsonb_build_object('reviewer_id',reviewer_id,'domain',domain,'required',required)
      ORDER BY domain COLLATE "C",reviewer_id),'[]'::jsonb) INTO actual FROM proposal_review_assignments
    WHERE organization_id=NEW.organization_id AND proposal_id=NEW.proposal_id AND configuration_version=revision;
  IF NOT EXISTS(SELECT 1 FROM proposals WHERE organization_id=NEW.organization_id AND id=NEW.proposal_id AND review_configuration_version>=revision) THEN
    RAISE EXCEPTION 'review configuration revision was not selected';
  END IF;
  IF expected IS NULL OR actual<>expected THEN RAISE EXCEPTION 'incomplete review assignment revision'; END IF;
  RETURN NULL;
END $$;
CREATE CONSTRAINT TRIGGER review_configuration_complete AFTER INSERT ON proposal_review_configurations
  DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION review_configuration_complete_guard();
CREATE CONSTRAINT TRIGGER review_assignment_complete AFTER INSERT ON proposal_review_assignments
  DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION review_configuration_complete_guard();
CREATE FUNCTION review_configuration_immutable() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN RAISE EXCEPTION 'review configuration history is immutable'; END $$;
CREATE TRIGGER review_config_immutable BEFORE UPDATE OR DELETE ON proposal_review_configurations
  FOR EACH ROW EXECUTE FUNCTION review_configuration_immutable();
CREATE TRIGGER review_config_no_truncate BEFORE TRUNCATE ON proposal_review_configurations
  FOR EACH STATEMENT EXECUTE FUNCTION review_configuration_immutable();
CREATE TRIGGER review_assignment_immutable BEFORE UPDATE OR DELETE ON proposal_review_assignments
  FOR EACH ROW EXECUTE FUNCTION review_configuration_immutable();
CREATE TRIGGER review_assignment_no_truncate BEFORE TRUNCATE ON proposal_review_assignments
  FOR EACH STATEMENT EXECUTE FUNCTION review_configuration_immutable();
INSERT INTO proposal_review_configurations(organization_id,proposal_id,version,ui_impact,ui_impact_reason,assignments,created_by)
 SELECT organization_id,id,1,ui_impact,ui_impact_reason,'[]',author_id FROM proposals;
CREATE FUNCTION review_configuration_initial() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  INSERT INTO proposal_review_configurations(organization_id,proposal_id,version,ui_impact,ui_impact_reason,assignments,created_by)
    VALUES(NEW.organization_id,NEW.id,1,NEW.ui_impact,NEW.ui_impact_reason,'[]',NEW.author_id);
  RETURN NEW;
END $$;
CREATE TRIGGER proposal_review_initial AFTER INSERT ON proposals FOR EACH ROW EXECUTE FUNCTION review_configuration_initial();
ALTER TABLE proposals ADD CONSTRAINT proposal_review_current FOREIGN KEY(organization_id,id,review_configuration_version)
  REFERENCES proposal_review_configurations(organization_id,proposal_id,version) DEFERRABLE INITIALLY DEFERRED;
CREATE FUNCTION review_configuration_pointer_guard() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE impact text; reason text;
BEGIN
  IF NEW.review_configuration_version IS DISTINCT FROM OLD.review_configuration_version THEN
    IF NEW.review_configuration_version<>OLD.review_configuration_version+1 OR NEW.version<>OLD.version+1
        OR OLD.state NOT IN ('DRAFT','IN_REVIEW','CHANGES_REQUESTED','APPROVED')
        OR (OLD.state='APPROVED' AND NEW.state<>'IN_REVIEW') THEN
      RAISE EXCEPTION 'invalid review configuration transition';
    END IF;
  ELSIF NEW.ui_impact IS DISTINCT FROM OLD.ui_impact OR NEW.ui_impact_reason IS DISTINCT FROM OLD.ui_impact_reason THEN
    RAISE EXCEPTION 'UI impact changes require a new review configuration';
  ELSE RETURN NEW;
  END IF;
  SELECT ui_impact,ui_impact_reason INTO impact,reason FROM proposal_review_configurations
    WHERE organization_id=NEW.organization_id AND proposal_id=NEW.id AND version=NEW.review_configuration_version;
  IF impact IS NULL OR impact<>NEW.ui_impact OR reason IS DISTINCT FROM NEW.ui_impact_reason THEN
    RAISE EXCEPTION 'review configuration does not match proposal metadata';
  END IF;
  RETURN NEW;
END $$;
CREATE TRIGGER proposal_review_pointer BEFORE UPDATE ON proposals FOR EACH ROW EXECUTE FUNCTION review_configuration_pointer_guard();
CREATE TABLE review_configuration_operations (
  organization_id uuid NOT NULL,
  proposal_id uuid NOT NULL,
  actor_id uuid NOT NULL REFERENCES users(id),
  request_id uuid NOT NULL,
  request_hash text NOT NULL CHECK(request_hash~'^[a-f0-9]{64}$'),
  applied_proposal_version bigint NOT NULL CHECK(applied_proposal_version>0),
  applied_configuration_version bigint NOT NULL,
  changed boolean NOT NULL,
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY(organization_id,proposal_id,actor_id,request_id),
  FOREIGN KEY(organization_id,proposal_id,applied_configuration_version)
    REFERENCES proposal_review_configurations(organization_id,proposal_id,version)
);
CREATE TRIGGER review_config_operation_immutable BEFORE UPDATE OR DELETE ON review_configuration_operations
  FOR EACH ROW EXECUTE FUNCTION review_configuration_immutable();
CREATE TRIGGER review_config_operation_no_truncate BEFORE TRUNCATE ON review_configuration_operations
  FOR EACH STATEMENT EXECUTE FUNCTION review_configuration_immutable();
