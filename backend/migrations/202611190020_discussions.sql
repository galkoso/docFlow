-- Discussions are separate from official decisions. All changes, including
-- message tombstones and thread state, are immutable revisions.
CREATE TABLE discussion_attempts (
  id uuid PRIMARY KEY,
  organization_id uuid NOT NULL,
  proposal_id uuid NOT NULL,
  actor_id uuid NOT NULL REFERENCES users(id),
  request_id uuid NOT NULL,
  request_hash text NOT NULL CHECK(request_hash~'^[a-f0-9]{64}$'),
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  FOREIGN KEY(organization_id,proposal_id) REFERENCES proposals(organization_id,id)
);
CREATE INDEX discussion_attempt_quota ON discussion_attempts(organization_id,created_at,actor_id);
CREATE INDEX discussion_attempt_request ON discussion_attempts(organization_id,request_id);

CREATE TABLE discussion_threads (
  id uuid PRIMARY KEY,
  sequence bigint GENERATED ALWAYS AS IDENTITY UNIQUE,
  organization_id uuid NOT NULL,
  proposal_id uuid NOT NULL,
  creator_id uuid NOT NULL REFERENCES users(id),
  snapshot_id uuid,
  fingerprint text CHECK(fingerprint~'^[a-f0-9]{64}$'),
  repository_id uuid NOT NULL,
  remote_id text NOT NULL,
  installation_id text NOT NULL,
  branch text NOT NULL,
  head_sha text NOT NULL,
  target_sha text NOT NULL,
  merge_base_sha text,
  proposal_version bigint NOT NULL CHECK(proposal_version>0),
  anchor jsonb NOT NULL CHECK(jsonb_typeof(anchor)='object'),
  opening_revision uuid NOT NULL UNIQUE,
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  created_transaction bigint NOT NULL DEFAULT txid_current(),
  UNIQUE(organization_id,proposal_id,id),
  FOREIGN KEY(organization_id,proposal_id) REFERENCES proposals(organization_id,id),
  FOREIGN KEY(organization_id,repository_id) REFERENCES repositories(organization_id,id),
  FOREIGN KEY(organization_id,proposal_id,snapshot_id) REFERENCES review_snapshots(organization_id,proposal_id,id),
  CHECK((snapshot_id IS NULL AND fingerprint IS NULL AND merge_base_sha IS NULL AND anchor='{"kind":"proposal"}')
    OR (snapshot_id IS NOT NULL AND fingerprint IS NOT NULL AND merge_base_sha IS NOT NULL))
);
CREATE INDEX discussion_thread_history ON discussion_threads(organization_id,proposal_id,sequence DESC);

CREATE TABLE discussion_revisions (
  id uuid PRIMARY KEY,
  organization_id uuid NOT NULL,
  proposal_id uuid NOT NULL,
  thread_id uuid NOT NULL,
  version bigint NOT NULL CHECK(version>0),
  actor_id uuid NOT NULL REFERENCES users(id),
  request_id uuid NOT NULL,
  request_hash text NOT NULL CHECK(request_hash~'^[a-f0-9]{64}$'),
  attempt_id uuid NOT NULL REFERENCES discussion_attempts(id),
  kind text NOT NULL CHECK(kind IN ('OPEN','REPLY','EDIT','TOMBSTONE','RESOLVE','REOPEN','SET_BLOCKING')),
  blocking boolean NOT NULL,
  resolved boolean NOT NULL,
  independent_requirement boolean NOT NULL,
  comment_id uuid,
  comment_version bigint,
  comment_author_id uuid REFERENCES users(id),
  reply_to_id uuid,
  body text CHECK(length(btrim(body)) BETWEEN 1 AND 10000),
  audit_id uuid NOT NULL UNIQUE REFERENCES audit_events(id) DEFERRABLE INITIALLY DEFERRED,
  event_id uuid NOT NULL UNIQUE REFERENCES outbox(id) DEFERRABLE INITIALLY DEFERRED,
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  created_transaction bigint NOT NULL DEFAULT txid_current(),
  UNIQUE(organization_id,request_id),
  UNIQUE(organization_id,proposal_id,thread_id,version),
  FOREIGN KEY(organization_id,proposal_id,thread_id) REFERENCES discussion_threads(organization_id,proposal_id,id),
  CHECK((kind IN ('OPEN','REPLY','EDIT','TOMBSTONE') AND comment_id IS NOT NULL AND comment_version IS NOT NULL AND comment_version>0 AND comment_author_id IS NOT NULL
      AND ((kind='TOMBSTONE' AND body IS NULL) OR (kind<>'TOMBSTONE' AND body IS NOT NULL)))
    OR (kind IN ('RESOLVE','REOPEN','SET_BLOCKING') AND comment_id IS NULL AND comment_version IS NULL
      AND comment_author_id IS NULL AND reply_to_id IS NULL AND body IS NULL))
);
CREATE UNIQUE INDEX discussion_comment_identity ON discussion_revisions(comment_id) WHERE kind IN ('OPEN','REPLY');
CREATE INDEX discussion_latest_state ON discussion_revisions(organization_id,proposal_id,thread_id,version DESC);
CREATE INDEX discussion_comment_history ON discussion_revisions(thread_id,comment_id,comment_version DESC) WHERE comment_id IS NOT NULL;
ALTER TABLE discussion_threads ADD FOREIGN KEY(opening_revision) REFERENCES discussion_revisions(id) DEFERRABLE INITIALLY DEFERRED;

CREATE FUNCTION discussion_attempt_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF NOT EXISTS(SELECT 1 FROM proposals p JOIN memberships m ON m.organization_id=p.organization_id
      JOIN users u ON u.id=m.user_id WHERE p.organization_id=NEW.organization_id AND p.id=NEW.proposal_id
      AND u.id=NEW.actor_id AND u.status='ACTIVE' AND m.role<>'READER' FOR SHARE OF p,m,u) THEN
    RAISE EXCEPTION 'discussion permission required'; END IF;
  PERFORM pg_advisory_xact_lock(hashtextextended('discussion-quota:'||NEW.organization_id::text,0));
  IF EXISTS(SELECT 1 FROM discussion_attempts a WHERE a.organization_id=NEW.organization_id AND a.request_id=NEW.request_id
      AND ROW(a.proposal_id,a.actor_id,a.request_hash) IS DISTINCT FROM ROW(NEW.proposal_id,NEW.actor_id,NEW.request_hash)) THEN
    RAISE EXCEPTION 'discussion request mismatch'; END IF;
  IF (SELECT count(*) FROM discussion_attempts WHERE organization_id=NEW.organization_id AND actor_id=NEW.actor_id AND created_at>clock_timestamp()-interval '10 minutes')>=60
    OR (SELECT count(*) FROM discussion_attempts WHERE organization_id=NEW.organization_id AND created_at>clock_timestamp()-interval '10 minutes')>=600 THEN
    RAISE EXCEPTION 'discussion quota exceeded'; END IF;
  NEW.created_at=clock_timestamp();
  RETURN NEW;
END $$;
CREATE TRIGGER discussion_attempt_guard BEFORE INSERT ON discussion_attempts FOR EACH ROW EXECUTE FUNCTION discussion_attempt_guard();

CREATE FUNCTION discussion_origin_guard() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE p record; h record; f jsonb; s jsonb; t jsonb;
BEGIN
  SELECT x.*,r.remote_id,r.installation_id INTO p FROM proposals x JOIN repositories r ON r.id=x.repository_id AND r.organization_id=x.organization_id
    JOIN memberships m ON m.organization_id=x.organization_id JOIN users u ON u.id=m.user_id
    WHERE x.organization_id=NEW.organization_id AND x.id=NEW.proposal_id AND u.id=NEW.creator_id AND u.status='ACTIVE' AND m.role<>'READER'
    AND r.provider='GITHUB' AND r.default_branch='main' FOR UPDATE OF x FOR SHARE OF r,m,u;
  IF NOT FOUND OR NEW.created_transaction<>txid_current() OR
    ROW(NEW.repository_id,NEW.remote_id,NEW.installation_id,NEW.branch,NEW.proposal_version)
      IS DISTINCT FROM ROW(p.repository_id,p.remote_id,p.installation_id,p.branch,p.version) THEN
    RAISE EXCEPTION 'discussion origin scope changed'; END IF;
  IF NEW.snapshot_id IS NULL THEN
    IF ROW(NEW.head_sha,NEW.target_sha) IS DISTINCT FROM ROW(p.head_sha,p.target_sha) THEN
      RAISE EXCEPTION 'discussion proposal context changed'; END IF;
  ELSE
    SELECT * INTO h FROM review_snapshots WHERE organization_id=NEW.organization_id AND proposal_id=NEW.proposal_id AND id=NEW.snapshot_id AND state='READY' FOR SHARE;
    IF NOT FOUND OR ROW(NEW.repository_id,NEW.remote_id,NEW.installation_id,NEW.branch,NEW.head_sha,NEW.target_sha,NEW.merge_base_sha,NEW.fingerprint)
      IS DISTINCT FROM ROW(h.repository_id,h.remote_id,h.installation_id,h.branch,h.head_sha,h.target_sha,h.merge_base_sha,h.fingerprint) THEN
      RAISE EXCEPTION 'discussion snapshot context changed'; END IF;
    IF NEW.anchor<>'{"kind":"proposal"}' THEN
      IF coalesce(NEW.anchor->>'kind','')<>'file' OR coalesce(NEW.anchor->>'side','') NOT IN ('before','after') THEN
        RAISE EXCEPTION 'unsupported discussion anchor'; END IF;
      SELECT change INTO f FROM review_snapshot_files WHERE organization_id=NEW.organization_id AND proposal_id=NEW.proposal_id
        AND snapshot_id=NEW.snapshot_id AND file_id=NEW.anchor->>'file_id';
      s=f->(NEW.anchor->>'side'); t=NEW.anchor->'text';
      IF f IS NULL OR s IS NULL OR s='null' OR s IS DISTINCT FROM NEW.anchor->'source' OR f->>'kind' NOT IN ('document','catalog')
        OR jsonb_typeof(t) IS DISTINCT FROM 'object' OR t->>'source_sha256' IS DISTINCT FROM s->>'sha256'
        OR coalesce(t->>'range_sha256','')!~'^[a-f0-9]{64}$' OR coalesce(length(t->>'quote'),1001)>1000
        OR jsonb_typeof(t->'quote_truncated') IS DISTINCT FROM 'boolean' OR jsonb_typeof(t->'quote') IS DISTINCT FROM 'string'
        OR coalesce((t->>'start_byte')::bigint,-1)<0 OR coalesce((t->>'end_byte')::bigint,-1)<(t->>'start_byte')::bigint
        OR (t->>'end_byte')::bigint>(s->>'bytes')::bigint OR coalesce((t->>'start_line')::bigint,0)<1
        OR coalesce((t->>'end_line')::bigint,0)<(t->>'start_line')::bigint
        OR coalesce(t#>>'{selection,kind}','') NOT IN ('document','section','lines')
        OR (t#>>'{selection,kind}'<>'lines' AND (f->>'kind'<>'document' OR t#>>'{selection,document_id}' IS DISTINCT FROM s->>'document_id'))
        OR (t#>>'{selection,kind}'='section' AND coalesce(t#>>'{selection,section_id}','')!~'^[A-Z0-9-]{1,120}$')
        OR (t#>>'{selection,kind}'='lines' AND (t->>'end_line')::bigint-(t->>'start_line')::bigint>=500) THEN
        RAISE EXCEPTION 'discussion anchor evidence mismatch'; END IF;
    END IF;
  END IF;
  NEW.created_at=clock_timestamp();
  RETURN NEW;
END $$;
CREATE TRIGGER discussion_origin_guard BEFORE INSERT ON discussion_threads FOR EACH ROW EXECUTE FUNCTION discussion_origin_guard();

CREATE FUNCTION discussion_revision_guard() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE role text; author uuid; t discussion_threads; previous discussion_revisions; message discussion_revisions; independent boolean;
BEGIN
  SELECT m.role,p.author_id INTO role,author FROM proposals p JOIN memberships m ON m.organization_id=p.organization_id JOIN users u ON u.id=m.user_id
    WHERE p.organization_id=NEW.organization_id AND p.id=NEW.proposal_id AND u.id=NEW.actor_id AND u.status='ACTIVE' AND m.role<>'READER'
    FOR UPDATE OF p FOR SHARE OF m,u;
  IF NOT FOUND OR NEW.created_transaction<>txid_current() OR NOT EXISTS(SELECT 1 FROM discussion_attempts a WHERE a.id=NEW.attempt_id
    AND ROW(a.organization_id,a.proposal_id,a.actor_id,a.request_id,a.request_hash)=ROW(NEW.organization_id,NEW.proposal_id,NEW.actor_id,NEW.request_id,NEW.request_hash)) THEN
    RAISE EXCEPTION 'discussion revision permission or admission missing'; END IF;
  SELECT * INTO t FROM discussion_threads WHERE organization_id=NEW.organization_id AND proposal_id=NEW.proposal_id AND id=NEW.thread_id FOR SHARE;
  IF NOT FOUND THEN RAISE EXCEPTION 'discussion thread missing'; END IF;
  SELECT * INTO previous FROM discussion_revisions WHERE thread_id=NEW.thread_id ORDER BY version DESC LIMIT 1;
  IF NEW.version<>coalesce(previous.version,0)+1 THEN RAISE EXCEPTION 'discussion version changed'; END IF;
  independent=NEW.actor_id<>author AND role IN ('REVIEWER','MAINTAINER','ADMIN');
  IF NEW.kind='OPEN' THEN
    IF previous.id IS NOT NULL OR t.opening_revision<>NEW.id OR t.creator_id<>NEW.actor_id OR NEW.resolved
      OR NEW.independent_requirement<>(NEW.blocking AND independent) OR NEW.comment_author_id<>NEW.actor_id OR NEW.comment_version<>1 OR NEW.reply_to_id IS NOT NULL THEN
      RAISE EXCEPTION 'invalid discussion opening'; END IF;
  ELSE
    IF previous.id IS NULL THEN RAISE EXCEPTION 'discussion opening missing'; END IF;
    IF NEW.kind IN ('REPLY','EDIT','TOMBSTONE') THEN
      IF ROW(NEW.blocking,NEW.resolved,NEW.independent_requirement) IS DISTINCT FROM ROW(previous.blocking,previous.resolved,previous.independent_requirement) THEN
        RAISE EXCEPTION 'comment cannot alter discussion state'; END IF;
      IF NEW.kind='REPLY' THEN
        IF NEW.comment_author_id<>NEW.actor_id OR NEW.comment_version<>1 OR (NEW.reply_to_id IS NOT NULL AND NOT EXISTS(
          SELECT 1 FROM discussion_revisions WHERE thread_id=NEW.thread_id AND comment_id=NEW.reply_to_id AND kind IN ('OPEN','REPLY'))) THEN
          RAISE EXCEPTION 'invalid discussion reply'; END IF;
      ELSE
        SELECT * INTO message FROM discussion_revisions WHERE thread_id=NEW.thread_id AND comment_id=NEW.comment_id ORDER BY comment_version DESC LIMIT 1;
        IF NOT FOUND OR message.kind='TOMBSTONE' OR NEW.comment_version<>message.comment_version+1 OR NEW.actor_id<>message.comment_author_id
          OR NEW.comment_author_id<>message.comment_author_id OR NEW.reply_to_id IS DISTINCT FROM message.reply_to_id THEN
          RAISE EXCEPTION 'discussion message author or version changed'; END IF;
      END IF;
    ELSE
      IF NEW.kind IN ('RESOLVE','REOPEN') THEN
        IF (NEW.actor_id<>t.creator_id AND role NOT IN ('REVIEWER','MAINTAINER','ADMIN'))
          OR (NEW.kind='RESOLVE' AND previous.independent_requirement AND NEW.actor_id=author)
          OR NEW.blocking<>previous.blocking OR NEW.independent_requirement<>previous.independent_requirement
          OR NEW.resolved<>(NEW.kind='RESOLVE') THEN RAISE EXCEPTION 'discussion resolution forbidden'; END IF;
      ELSE
        IF (NEW.actor_id<>t.creator_id AND role NOT IN ('MAINTAINER','ADMIN'))
          OR (NOT NEW.blocking AND previous.independent_requirement AND NEW.actor_id=author)
          OR NEW.resolved<>previous.resolved OR NEW.independent_requirement<>(previous.independent_requirement OR (NEW.blocking AND independent)) THEN
          RAISE EXCEPTION 'discussion blocking change forbidden'; END IF;
      END IF;
    END IF;
  END IF;
  NEW.created_at=clock_timestamp();
  RETURN NEW;
END $$;
CREATE TRIGGER discussion_revision_guard BEFORE INSERT ON discussion_revisions FOR EACH ROW EXECUTE FUNCTION discussion_revision_guard();

CREATE FUNCTION discussion_complete() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF NOT EXISTS(SELECT 1 FROM audit_events a JOIN outbox e ON e.id=NEW.event_id WHERE a.id=NEW.audit_id AND a.organization_id=NEW.organization_id
      AND a.actor_id=NEW.actor_id AND a.entity_id=NEW.thread_id AND a.action='DISCUSSION_'||NEW.kind AND a.after_ref=NEW.id::text AND a.result='RECORDED'
      AND e.organization_id=NEW.organization_id AND e.entity_id=NEW.proposal_id AND e.event_type='DISCUSSION_CHANGED'
      AND e.payload=jsonb_build_object('thread_id',NEW.thread_id,'revision_id',NEW.id,'version',NEW.version,'kind',NEW.kind)) THEN
    RAISE EXCEPTION 'discussion requires atomic audit and event'; END IF;
  RETURN NULL;
END $$;
CREATE CONSTRAINT TRIGGER discussion_complete AFTER INSERT ON discussion_revisions DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION discussion_complete();
CREATE FUNCTION discussion_opening_complete() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF NOT EXISTS(SELECT 1 FROM discussion_revisions WHERE id=NEW.opening_revision AND organization_id=NEW.organization_id AND proposal_id=NEW.proposal_id
    AND thread_id=NEW.id AND version=1 AND kind='OPEN' AND actor_id=NEW.creator_id) THEN RAISE EXCEPTION 'discussion opening revision required'; END IF;
  RETURN NULL;
END $$;
CREATE CONSTRAINT TRIGGER discussion_opening_complete AFTER INSERT ON discussion_threads DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION discussion_opening_complete();

CREATE TRIGGER discussion_attempt_immutable BEFORE UPDATE OR DELETE ON discussion_attempts FOR EACH ROW EXECUTE FUNCTION review_submission_immutable();
CREATE TRIGGER discussion_attempt_no_truncate BEFORE TRUNCATE ON discussion_attempts FOR EACH STATEMENT EXECUTE FUNCTION review_submission_immutable();
CREATE TRIGGER discussion_origin_immutable BEFORE UPDATE OR DELETE ON discussion_threads FOR EACH ROW EXECUTE FUNCTION review_submission_immutable();
CREATE TRIGGER discussion_origin_no_truncate BEFORE TRUNCATE ON discussion_threads FOR EACH STATEMENT EXECUTE FUNCTION review_submission_immutable();
CREATE TRIGGER discussion_revision_immutable BEFORE UPDATE OR DELETE ON discussion_revisions FOR EACH ROW EXECUTE FUNCTION review_submission_immutable();
CREATE TRIGGER discussion_revision_no_truncate BEFORE TRUNCATE ON discussion_revisions FOR EACH STATEMENT EXECUTE FUNCTION review_submission_immutable();
CREATE FUNCTION discussion_event_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF EXISTS(SELECT 1 FROM discussion_revisions WHERE event_id=OLD.id) THEN
    IF TG_OP='DELETE' THEN RAISE EXCEPTION 'discussion event is durable'; END IF;
    IF ROW(NEW.id,NEW.organization_id,NEW.event_type,NEW.entity_id,NEW.payload,NEW.created_at)
      IS DISTINCT FROM ROW(OLD.id,OLD.organization_id,OLD.event_type,OLD.entity_id,OLD.payload,OLD.created_at)
      OR (OLD.sent_at IS NOT NULL AND NEW.sent_at IS DISTINCT FROM OLD.sent_at) THEN RAISE EXCEPTION 'discussion event scope is immutable'; END IF;
  END IF;
  IF TG_OP='DELETE' THEN RETURN OLD; END IF; RETURN NEW;
END $$;
CREATE TRIGGER discussion_event_guard BEFORE UPDATE OR DELETE ON outbox FOR EACH ROW EXECUTE FUNCTION discussion_event_guard();
