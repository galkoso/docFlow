-- Bounded public submission admission; preserve immutable prior migrations.
CREATE INDEX review_submission_recent_admission ON review_submissions(organization_id,created_at DESC);
CREATE INDEX review_submission_active_admission ON git_operations(organization_id,actor_id,id)
  WHERE kind='SUBMIT' AND state IN ('PENDING','RUNNING','UNKNOWN');
