-- Existing snapshots keep their original evidence. New publications seal the
-- complete head-book manifest as well as the changed-file inventory.
ALTER TABLE review_snapshots ADD COLUMN book_manifest_version integer NOT NULL DEFAULT 0 CHECK(book_manifest_version IN (0,1));
ALTER TABLE review_snapshots ALTER COLUMN book_manifest_version SET DEFAULT 1;

CREATE TABLE review_snapshot_books (
  organization_id uuid NOT NULL,
  proposal_id uuid NOT NULL,
  snapshot_id uuid PRIMARY KEY,
  documents jsonb NOT NULL CHECK(jsonb_typeof(documents)='array' AND jsonb_array_length(documents)<=1000 AND octet_length(documents::text)<=1048576),
  FOREIGN KEY(organization_id,proposal_id,snapshot_id) REFERENCES review_snapshots(organization_id,proposal_id,id)
);
CREATE TRIGGER review_snapshot_book_immutable BEFORE INSERT OR UPDATE OR DELETE ON review_snapshot_books
  FOR EACH ROW EXECUTE FUNCTION review_snapshot_child_guard();
CREATE TRIGGER review_snapshot_book_no_truncate BEFORE TRUNCATE ON review_snapshot_books
  FOR EACH STATEMENT EXECUTE FUNCTION review_submission_immutable();

CREATE FUNCTION review_book_manifest_guard() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE d jsonb; s jsonb; ordinal bigint; total_bytes bigint:=0; count_documents bigint;
BEGIN
  IF NOT EXISTS(SELECT 1 FROM review_snapshots WHERE id=NEW.snapshot_id AND book_manifest_version=1) THEN
    RAISE EXCEPTION 'book manifest version missing'; END IF;
  count_documents=jsonb_array_length(NEW.documents);
  IF (SELECT count(DISTINCT x->>'id') FROM jsonb_array_elements(NEW.documents) x)<>count_documents
    OR (SELECT count(DISTINCT x#>>'{source,document_id}') FROM jsonb_array_elements(NEW.documents) x)<>count_documents
    OR (SELECT count(DISTINCT x#>>'{source,path}') FROM jsonb_array_elements(NEW.documents) x)<>count_documents THEN
    RAISE EXCEPTION 'book identities must be unique'; END IF;
  FOR d,ordinal IN SELECT value,ordinality-1 FROM jsonb_array_elements(NEW.documents) WITH ORDINALITY LOOP
    s=d->'source';
    IF jsonb_typeof(d) IS DISTINCT FROM 'object' OR NOT(d ?& ARRAY['id','source']) OR d-ARRAY['id','source']<>'{}'::jsonb
      OR jsonb_typeof(d->'id') IS DISTINCT FROM 'string' OR coalesce(d->>'id','')!~'^[a-f0-9]{64}$' OR jsonb_typeof(s) IS DISTINCT FROM 'object'
      OR NOT(s ?& ARRAY['path','blob_oid','bytes','executable','sha256','document_id','catalog_position'])
      OR s-ARRAY['path','blob_oid','bytes','executable','sha256','document_id','catalog_position']<>'{}'::jsonb
      OR EXISTS(SELECT 1 FROM unnest(ARRAY['path','blob_oid','sha256','document_id']) k WHERE jsonb_typeof(s->k) IS DISTINCT FROM 'string')
      OR coalesce(s->>'path','')!~'^docs/[^[:cntrl:]\\:]+\.md$' OR length(s->>'path')>240
      OR s->>'path'~'(^|/)\.{1,2}(/|$)' OR s->>'path'~*'(^|/)\.git(/|$)' OR s->>'path' LIKE '%//%'
      OR coalesce(s->>'blob_oid','')!~'^([a-f0-9]{40}|[a-f0-9]{64})$'
      OR coalesce(s->>'sha256','')!~'^[a-f0-9]{64}$' OR s->'executable' IS DISTINCT FROM 'false'::jsonb
      OR coalesce(s->>'document_id','')!~'^[A-Z0-9-]{1,120}$'
      OR jsonb_typeof(s->'bytes') IS DISTINCT FROM 'number' OR coalesce(s->>'bytes','')!~'^[0-9]+$'
      OR (s->>'bytes')::bigint NOT BETWEEN 0 AND 2097152
      OR s->'catalog_position' IS DISTINCT FROM to_jsonb(ordinal) THEN
      RAISE EXCEPTION 'invalid book document evidence'; END IF;
    total_bytes=total_bytes+(s->>'bytes')::bigint;
  END LOOP;
  IF total_bytes>20971520 THEN RAISE EXCEPTION 'book manifest exceeds byte quota'; END IF;
  RETURN NEW;
END $$;
CREATE TRIGGER review_book_manifest_guard BEFORE INSERT ON review_snapshot_books
  FOR EACH ROW EXECUTE FUNCTION review_book_manifest_guard();

CREATE FUNCTION review_book_snapshot_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF TG_OP='INSERT' THEN
    IF NEW.book_manifest_version<>1 THEN RAISE EXCEPTION 'new review requires book manifest'; END IF;
  ELSIF NEW.state='READY' AND NEW.book_manifest_version=1 AND NOT EXISTS(
    SELECT 1 FROM review_snapshot_books WHERE snapshot_id=NEW.id AND organization_id=NEW.organization_id AND proposal_id=NEW.proposal_id) THEN
    RAISE EXCEPTION 'review book manifest missing';
  END IF;
  IF TG_OP='UPDATE' AND NEW.state='READY' AND NEW.book_manifest_version=1 AND EXISTS(
    SELECT 1 FROM review_snapshot_files f WHERE f.snapshot_id=NEW.id AND f.change->>'kind'='document'
      AND f.change->'after' IS DISTINCT FROM 'null'::jsonb AND NOT EXISTS(
        SELECT 1 FROM review_snapshot_books b,jsonb_array_elements(b.documents) d
        WHERE b.snapshot_id=NEW.id AND d->'source'=f.change->'after')) THEN
    RAISE EXCEPTION 'book manifest omits changed document evidence'; END IF;
  RETURN NEW;
END $$;
CREATE TRIGGER review_book_snapshot_guard BEFORE INSERT OR UPDATE ON review_snapshots
  FOR EACH ROW EXECUTE FUNCTION review_book_snapshot_guard();
