CREATE INDEX pr_status_publications ON review_submission_results
  (organization_id,proposal_id,snapshot_id,created_at DESC,operation_id DESC);
CREATE INDEX pr_status_events ON outbox(organization_id,entity_id,created_at,id)
  WHERE event_type='REVIEW_SNAPSHOT_PUBLISHED';
CREATE INDEX pr_status_intents ON pr_sync_intents(organization_id,proposal_id,created_at DESC,operation_id DESC);
