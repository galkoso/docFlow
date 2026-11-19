-- Merge authorization and discussion transitions share the proposal row lock.
-- After entering MERGING, comments cannot introduce a newly unresolved blocker.
CREATE FUNCTION discussion_merge_barrier() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE phase text; previous discussion_revisions;
BEGIN
  SELECT state INTO phase FROM proposals WHERE organization_id=NEW.organization_id AND id=NEW.proposal_id FOR UPDATE;
  IF phase='MERGING' AND NEW.blocking AND NOT NEW.resolved THEN
    SELECT * INTO previous FROM discussion_revisions WHERE organization_id=NEW.organization_id
      AND proposal_id=NEW.proposal_id AND thread_id=NEW.thread_id ORDER BY version DESC LIMIT 1;
    IF NOT FOUND OR NOT previous.blocking OR previous.resolved THEN
      RAISE EXCEPTION 'new discussion blocker cannot enter an active merge';
    END IF;
  END IF;
  RETURN NEW;
END $$;
CREATE TRIGGER discussion_merge_barrier BEFORE INSERT ON discussion_revisions FOR EACH ROW EXECUTE FUNCTION discussion_merge_barrier();
