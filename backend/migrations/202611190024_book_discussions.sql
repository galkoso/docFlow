-- Complete-book origins retain their sealed head descriptor; no synthetic diff
-- entry or current repository source can stand in for the original manifest.
CREATE OR REPLACE FUNCTION discussion_origin_guard() RETURNS trigger LANGUAGE plpgsql AS $$
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
      IF NEW.anchor->>'kind'='file' THEN
        IF coalesce(NEW.anchor->>'side','') NOT IN ('before','after') THEN
          RAISE EXCEPTION 'unsupported discussion anchor'; END IF;
        SELECT change INTO f FROM review_snapshot_files WHERE organization_id=NEW.organization_id AND proposal_id=NEW.proposal_id
          AND snapshot_id=NEW.snapshot_id AND file_id=NEW.anchor->>'file_id';
        s=f->(NEW.anchor->>'side');
      ELSIF NEW.anchor->>'kind'='book_document' THEN
        IF NOT(NEW.anchor ?& ARRAY['kind','entry_id','source','text'])
          OR NEW.anchor-ARRAY['kind','entry_id','source','text']<>'{}'::jsonb
          OR coalesce(NEW.anchor->>'entry_id','')!~'^[a-f0-9]{64}$' OR h.book_manifest_version<>1 THEN
          RAISE EXCEPTION 'unsupported book discussion anchor'; END IF;
        SELECT d->'source' INTO s FROM review_snapshot_books b,jsonb_array_elements(b.documents) d
          WHERE b.organization_id=NEW.organization_id AND b.proposal_id=NEW.proposal_id AND b.snapshot_id=NEW.snapshot_id
          AND d->>'id'=NEW.anchor->>'entry_id';
        f=jsonb_build_object('kind','document');
      ELSE RAISE EXCEPTION 'unsupported discussion anchor';
      END IF;
      t=NEW.anchor->'text';
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
