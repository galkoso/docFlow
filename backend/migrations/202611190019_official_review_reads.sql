-- Charge every authorized provider-read attempt, including failed reads. Exact
-- decision receipt replay bypasses IO; failed requests retain their body binding.
CREATE TABLE official_review_reads (
  id uuid PRIMARY KEY,
  organization_id uuid NOT NULL,
  proposal_id uuid NOT NULL,
  snapshot_id uuid NOT NULL,
  reviewer_id uuid NOT NULL REFERENCES users(id),
  request_id uuid NOT NULL,
  request_hash text NOT NULL CHECK(request_hash~'^[a-f0-9]{64}$'),
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  FOREIGN KEY(organization_id,proposal_id,snapshot_id) REFERENCES review_snapshots(organization_id,proposal_id,id)
);
CREATE INDEX official_read_quota ON official_review_reads(organization_id,created_at,reviewer_id);
CREATE INDEX official_read_request ON official_review_reads(organization_id,request_id,created_at);
CREATE TRIGGER official_read_immutable BEFORE UPDATE OR DELETE ON official_review_reads FOR EACH ROW EXECUTE FUNCTION review_submission_immutable();
CREATE TRIGGER official_read_no_truncate BEFORE TRUNCATE ON official_review_reads FOR EACH STATEMENT EXECUTE FUNCTION review_submission_immutable();
CREATE TABLE official_review_backoff (
  organization_id uuid NOT NULL,
  repository_id uuid NOT NULL,
  remote_id text NOT NULL,
  installation_id text NOT NULL,
  until_at timestamptz NOT NULL,
  PRIMARY KEY(organization_id,repository_id,remote_id,installation_id),
  FOREIGN KEY(organization_id,repository_id) REFERENCES repositories(organization_id,id)
);
