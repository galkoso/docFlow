-- Coverage must refer to a sealed, complete source requirement set. Existing
-- snapshots retain version 0; no review/visit evidence is synthesized for them.
ALTER TABLE review_snapshots ADD COLUMN review_units_version integer NOT NULL DEFAULT 0 CHECK(review_units_version IN (0,1));
ALTER TABLE review_snapshots ALTER COLUMN review_units_version SET DEFAULT 1;
ALTER TABLE review_snapshots ADD COLUMN unit_count integer NOT NULL DEFAULT 0 CHECK(unit_count BETWEEN 0 AND 40000);
ALTER TABLE review_snapshots ADD COLUMN unit_ids jsonb NOT NULL DEFAULT '[]'::jsonb CHECK(jsonb_typeof(unit_ids)='array' AND jsonb_array_length(unit_ids)=unit_count AND octet_length(unit_ids::text)<=4194304);

CREATE TABLE review_snapshot_units (
  organization_id uuid NOT NULL,
  proposal_id uuid NOT NULL,
  snapshot_id uuid NOT NULL,
  ordinal integer NOT NULL CHECK(ordinal BETWEEN 0 AND 39999),
  unit_id text NOT NULL CHECK(unit_id~'^[a-f0-9]{64}$'),
  file_id text NOT NULL CHECK(file_id~'^[a-f0-9]{64}$'),
  unit jsonb NOT NULL CHECK(jsonb_typeof(unit)='object' AND octet_length(unit::text)<=8192
    AND unit ?& ARRAY['id','file_id','requirement','before','after'] AND unit-ARRAY['id','file_id','requirement','before','after']='{}'::jsonb
    AND unit->>'id' IS NOT DISTINCT FROM unit_id AND unit->>'file_id' IS NOT DISTINCT FROM file_id),
  PRIMARY KEY(snapshot_id,ordinal), UNIQUE(snapshot_id,unit_id),
  FOREIGN KEY(organization_id,proposal_id,snapshot_id) REFERENCES review_snapshots(organization_id,proposal_id,id),
  FOREIGN KEY(snapshot_id,file_id) REFERENCES review_snapshot_files(snapshot_id,file_id)
);
CREATE UNIQUE INDEX review_unit_scope ON review_snapshot_units(snapshot_id,file_id,(unit->'requirement'));
CREATE TRIGGER review_snapshot_units_immutable BEFORE INSERT OR UPDATE OR DELETE ON review_snapshot_units
  FOR EACH ROW EXECUTE FUNCTION review_snapshot_child_guard();
CREATE TRIGGER review_snapshot_units_no_truncate BEFORE TRUNCATE ON review_snapshot_units
  FOR EACH STATEMENT EXECUTE FUNCTION review_submission_immutable();

CREATE FUNCTION review_unit_evidence_guard() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE f jsonb; r jsonb; selected jsonb; source jsonb; span jsonb; side text; kind text;
BEGIN
  IF NOT EXISTS(SELECT 1 FROM review_snapshots WHERE id=NEW.snapshot_id AND review_units_version=1) THEN
    RAISE EXCEPTION 'review unit manifest version missing'; END IF;
  SELECT change INTO f FROM review_snapshot_files WHERE snapshot_id=NEW.snapshot_id AND file_id=NEW.file_id;
  r=NEW.unit->'requirement'; kind=r->>'kind';
  IF jsonb_typeof(r) IS DISTINCT FROM 'object' OR coalesce(kind,'') NOT IN ('file','section')
    OR (kind='section' AND (f->>'kind'<>'document' OR NOT(r ?& ARRAY['kind','document_id','section_id'])
      OR r-ARRAY['kind','document_id','section_id']<>'{}'::jsonb OR jsonb_typeof(r->'document_id') IS DISTINCT FROM 'string'
      OR jsonb_typeof(r->'section_id') IS DISTINCT FROM 'string' OR coalesce(r->>'document_id','')!~'^[A-Z0-9-]{1,120}$'
      OR coalesce(r->>'section_id','')!~'^[A-Z0-9-]{1,120}$'))
    OR (kind='file' AND (r-ARRAY['kind','reason']<>'{}'::jsonb OR coalesce(r->>'reason','') NOT IN
      ('non_document','non_section','layout','unstructured','section_limit','manifest_limit')))
    OR (NEW.unit->'before'='null'::jsonb AND NEW.unit->'after'='null'::jsonb) THEN
    RAISE EXCEPTION 'invalid review unit requirement'; END IF;
  FOREACH side IN ARRAY ARRAY['before','after'] LOOP
    selected=NEW.unit->side;
    IF selected='null'::jsonb THEN
      IF kind='file' AND f->side IS DISTINCT FROM 'null'::jsonb THEN RAISE EXCEPTION 'review unit omits file side'; END IF;
      CONTINUE;
    END IF;
    source=selected->'source'; span=selected->'range';
    IF jsonb_typeof(selected) IS DISTINCT FROM 'object' OR NOT(selected ?& ARRAY['source','range'])
      OR selected-ARRAY['source','range']<>'{}'::jsonb OR source IS DISTINCT FROM f->side
      OR source IS NULL OR source='null'::jsonb THEN RAISE EXCEPTION 'review unit source mismatch'; END IF;
    IF kind='section' AND (r->>'document_id' IS DISTINCT FROM source->>'document_id' OR span='null'::jsonb) THEN
      RAISE EXCEPTION 'review section source identity mismatch'; END IF;
    IF span='null'::jsonb THEN
      IF f->>'kind' IN ('document','catalog') THEN RAISE EXCEPTION 'review text unit needs source range'; END IF;
      CONTINUE;
    END IF;
    IF jsonb_typeof(span) IS DISTINCT FROM 'object' OR NOT(span ?& ARRAY['start_byte','end_byte','start_line','end_line','sha256'])
      OR span-ARRAY['start_byte','end_byte','start_line','end_line','sha256']<>'{}'::jsonb
      OR jsonb_typeof(span->'sha256') IS DISTINCT FROM 'string' OR coalesce(span->>'sha256','')!~'^[a-f0-9]{64}$'
      OR jsonb_typeof(span->'start_byte') IS DISTINCT FROM 'number' OR jsonb_typeof(span->'end_byte') IS DISTINCT FROM 'number'
      OR jsonb_typeof(span->'start_line') IS DISTINCT FROM 'number' OR jsonb_typeof(span->'end_line') IS DISTINCT FROM 'number'
      OR coalesce((span->>'start_byte')::bigint,-1)<0 OR coalesce((span->>'end_byte')::bigint,-1)<(span->>'start_byte')::bigint
      OR (span->>'end_byte')::bigint>(source->>'bytes')::bigint OR coalesce((span->>'start_line')::bigint,0)<1
      OR coalesce((span->>'end_line')::bigint,0)<(span->>'start_line')::bigint
      OR (span->>'start_line')::bigint>(span->>'start_byte')::bigint+1 OR (span->>'end_line')::bigint>(span->>'end_byte')::bigint+1 THEN
      RAISE EXCEPTION 'review unit source range mismatch'; END IF;
  END LOOP;
  RETURN NEW;
END $$;
CREATE TRIGGER review_unit_evidence_guard BEFORE INSERT ON review_snapshot_units FOR EACH ROW EXECUTE FUNCTION review_unit_evidence_guard();

CREATE FUNCTION review_unit_snapshot_guard() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE ids jsonb; count_units bigint; first_unit integer; last_unit integer;
BEGIN
  IF TG_OP='INSERT' THEN
    IF NEW.review_units_version<>1 OR NEW.unit_count<1 THEN RAISE EXCEPTION 'new review requires mandatory units'; END IF;
  ELSIF NEW.state='READY' AND NEW.review_units_version=1 THEN
    SELECT jsonb_agg(unit_id ORDER BY ordinal),count(*),min(ordinal),max(ordinal) INTO ids,count_units,first_unit,last_unit
      FROM review_snapshot_units WHERE snapshot_id=NEW.id;
    IF ids IS DISTINCT FROM NEW.unit_ids OR count_units<>NEW.unit_count OR first_unit<>0 OR last_unit<>NEW.unit_count-1
      OR EXISTS(SELECT 1 FROM review_snapshot_files f WHERE f.snapshot_id=NEW.id AND NOT EXISTS(
        SELECT 1 FROM review_snapshot_units u WHERE u.snapshot_id=f.snapshot_id AND u.file_id=f.file_id)) THEN
      RAISE EXCEPTION 'review unit manifest incomplete'; END IF;
  END IF;
  RETURN NEW;
END $$;
CREATE TRIGGER review_unit_snapshot_guard BEFORE INSERT OR UPDATE ON review_snapshots FOR EACH ROW EXECUTE FUNCTION review_unit_snapshot_guard();
