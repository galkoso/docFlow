-- Snapshot/domain coverage reads must not scan another reviewer's historic reads.
CREATE INDEX review_source_attempts_scope ON review_source_attempts
  (organization_id,proposal_id,snapshot_id,reviewer_id,domain,fingerprint,unit_id,side,id);
