-- Thread IDs are globally unique. These indexes also serve the revision guard's
-- serialized latest-version lookup without scanning unrelated organizations.
CREATE INDEX discussion_thread_revision ON discussion_revisions(thread_id,version DESC)
  INCLUDE(blocking,resolved);
CREATE INDEX discussion_message_order ON discussion_revisions(thread_id,version)
  WHERE kind IN ('OPEN','REPLY');
CREATE INDEX discussion_resolution_order ON discussion_revisions(thread_id,version DESC)
  INCLUDE(actor_id,kind,created_at) WHERE kind IN ('RESOLVE','REOPEN');
