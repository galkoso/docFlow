-- Metadata-only submission work. The provider mutation boundary remains in
-- git_operations.state; PENDING work may be read again after a lost worker.
CREATE UNIQUE INDEX review_submission_job ON jobs(operation_id) WHERE kind='SUBMIT_REVIEW';
CREATE INDEX review_submission_claim ON jobs(available_at,created_at,id)
  WHERE kind='SUBMIT_REVIEW' AND state IN ('QUEUED','RUNNING');
CREATE TABLE review_submissions (
  operation_id uuid PRIMARY KEY,
  job_id uuid NOT NULL UNIQUE REFERENCES jobs(id),
  organization_id uuid NOT NULL,
  proposal_id uuid NOT NULL,
  repository_id uuid NOT NULL,
  actor_id uuid NOT NULL REFERENCES users(id),
  request_hash text NOT NULL CHECK(request_hash~'^[a-f0-9]{64}$'),
  scope_hash text NOT NULL CHECK(scope_hash~'^[a-f0-9]{64}$'),
  expected_head text NOT NULL CHECK(expected_head~'^[a-f0-9]{40,128}$' AND length(expected_head)%2=0),
  expected_target text NOT NULL CHECK(expected_target~'^[a-f0-9]{40,128}$' AND length(expected_target)%2=0),
  expected_version bigint NOT NULL CHECK(expected_version>0),
  configuration_version bigint NOT NULL CHECK(configuration_version>0),
  policy_version bigint NOT NULL CHECK(policy_version>0),
  branch text NOT NULL CHECK(branch LIKE 'change/%' AND length(branch) BETWEEN 8 AND 100),
  remote_id text NOT NULL CHECK(remote_id~'^[1-9][0-9]{0,19}$'),
  installation_id text NOT NULL CHECK(installation_id~'^[1-9][0-9]{0,19}$'),
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  FOREIGN KEY(organization_id,operation_id) REFERENCES git_operations(organization_id,id),
  FOREIGN KEY(organization_id,proposal_id) REFERENCES proposals(organization_id,id),
  FOREIGN KEY(organization_id,repository_id) REFERENCES repositories(organization_id,id),
  FOREIGN KEY(organization_id,proposal_id,configuration_version)
    REFERENCES proposal_review_configurations(organization_id,proposal_id,version)
);
CREATE FUNCTION review_submission_insert_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF NOT EXISTS(SELECT 1 FROM git_operations o JOIN jobs j ON j.operation_id=o.id AND j.organization_id=o.organization_id
      JOIN proposals p ON p.id=o.proposal_id AND p.organization_id=o.organization_id
      JOIN repositories r ON r.id=p.repository_id AND r.organization_id=p.organization_id
      JOIN organizations g ON g.id=p.organization_id
      WHERE o.id=NEW.operation_id AND o.organization_id=NEW.organization_id AND o.proposal_id=NEW.proposal_id
      AND o.actor_id=NEW.actor_id AND o.kind='SUBMIT' AND o.state='PENDING'
      AND o.request_hash=NEW.request_hash AND o.expected_sha=NEW.expected_head
      AND j.id=NEW.job_id AND j.kind='SUBMIT_REVIEW' AND j.state='QUEUED' AND j.attempts=0
      AND j.payload=jsonb_build_object('operation_id',NEW.operation_id)
      AND p.repository_id=NEW.repository_id AND p.head_sha=NEW.expected_head AND p.version=NEW.expected_version
      AND p.review_configuration_version=NEW.configuration_version AND g.policy_version=NEW.policy_version
      AND p.state IN ('DRAFT','IN_REVIEW','CHANGES_REQUESTED','APPROVED') AND p.branch=NEW.branch
      AND ltrim(r.remote_id,'0+')=NEW.remote_id AND ltrim(r.installation_id,'0+')=NEW.installation_id AND r.default_branch='main'
      FOR SHARE OF o,j,p,r,g) THEN
    RAISE EXCEPTION 'review submission scope does not match its operation and job';
  END IF;
  RETURN NEW;
END $$;
CREATE TRIGGER review_submission_insert BEFORE INSERT ON review_submissions
  FOR EACH ROW EXECUTE FUNCTION review_submission_insert_guard();
CREATE FUNCTION review_submission_immutable() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN RAISE EXCEPTION 'review submission intent is immutable'; END $$;
CREATE TRIGGER review_submission_immutable BEFORE UPDATE OR DELETE ON review_submissions
  FOR EACH ROW EXECUTE FUNCTION review_submission_immutable();
CREATE TRIGGER review_submission_no_truncate BEFORE TRUNCATE ON review_submissions
  FOR EACH STATEMENT EXECUTE FUNCTION review_submission_immutable();
CREATE FUNCTION review_submission_job_immutable() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF OLD.kind='SUBMIT_REVIEW' AND ROW(NEW.id,NEW.organization_id,NEW.operation_id,NEW.kind,NEW.payload,NEW.max_attempts,NEW.created_at)
      IS DISTINCT FROM ROW(OLD.id,OLD.organization_id,OLD.operation_id,OLD.kind,OLD.payload,OLD.max_attempts,OLD.created_at) THEN
    RAISE EXCEPTION 'review submission job scope is immutable';
  END IF;
  RETURN NEW;
END $$;
CREATE TRIGGER review_submission_job_immutable BEFORE UPDATE ON jobs
  FOR EACH ROW EXECUTE FUNCTION review_submission_job_immutable();
CREATE FUNCTION review_submission_job_complete() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF NEW.kind='SUBMIT_REVIEW' AND NOT EXISTS(SELECT 1 FROM review_submissions s WHERE s.job_id=NEW.id
      AND s.organization_id=NEW.organization_id AND s.operation_id=NEW.operation_id) THEN
    RAISE EXCEPTION 'submission job requires its frozen intent';
  END IF;
  RETURN NULL;
END $$;
CREATE CONSTRAINT TRIGGER review_submission_job_complete AFTER INSERT OR UPDATE ON jobs
  DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION review_submission_job_complete();
CREATE FUNCTION review_submission_operation_immutable() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF EXISTS(SELECT 1 FROM review_submissions WHERE operation_id=OLD.id) AND
      ROW(NEW.id,NEW.organization_id,NEW.proposal_id,NEW.actor_id,NEW.kind,NEW.idempotency_key,NEW.request_hash,NEW.expected_sha,NEW.created_at)
      IS DISTINCT FROM ROW(OLD.id,OLD.organization_id,OLD.proposal_id,OLD.actor_id,OLD.kind,OLD.idempotency_key,OLD.request_hash,OLD.expected_sha,OLD.created_at) THEN
    RAISE EXCEPTION 'review submission operation scope is immutable';
  END IF;
  RETURN NEW;
END $$;
CREATE TRIGGER review_submission_operation_immutable BEFORE UPDATE ON git_operations
  FOR EACH ROW EXECUTE FUNCTION review_submission_operation_immutable();
