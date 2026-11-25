-- Proposal-level product questions. Imported answers are proposals, not product
-- decisions. Every state change is an immutable, audited revision.
CREATE FUNCTION product_question_text_valid(v text) RETURNS boolean LANGUAGE sql IMMUTABLE AS $$
 SELECT v IS NOT NULL AND char_length(v)<=10000
   AND char_length(btrim(v,E' \t\r\n\013\f'||U&'\0085\00A0\1680\2000\2001\2002\2003\2004\2005\2006\2007\2008\2009\200A\2028\2029\202F\205F\3000'))>0
$$;
CREATE TABLE product_questions (
 id uuid PRIMARY KEY,
 sequence bigint GENERATED ALWAYS AS IDENTITY UNIQUE,
 organization_id uuid NOT NULL,
 proposal_id uuid NOT NULL,
 creator_id uuid NOT NULL REFERENCES users(id),
 source text NOT NULL CHECK(source IN ('HUMAN','IMPORTED')),
 -- Provenance locator only, never proof of a verified import or decision.
 source_reference uuid,
 repository_id uuid NOT NULL,
 branch text NOT NULL,
 head_sha text NOT NULL CHECK(head_sha~'^[a-f0-9]{40,128}$' AND length(head_sha)%2=0),
 target_sha text NOT NULL CHECK(target_sha~'^[a-f0-9]{40,128}$' AND length(target_sha)%2=0),
 proposal_version bigint NOT NULL CHECK(proposal_version>0),
 opening_revision uuid NOT NULL UNIQUE,
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 created_transaction bigint NOT NULL DEFAULT txid_current(),
 UNIQUE(organization_id,proposal_id,id),
 FOREIGN KEY(organization_id,proposal_id) REFERENCES proposals(organization_id,id),
 FOREIGN KEY(organization_id,repository_id) REFERENCES repositories(organization_id,id),
 CHECK((source='HUMAN' AND source_reference IS NULL) OR (source='IMPORTED' AND source_reference IS NOT NULL))
);
CREATE INDEX product_question_history ON product_questions(organization_id,proposal_id,sequence DESC);
CREATE TABLE product_question_revisions (
 id uuid PRIMARY KEY,
 organization_id uuid NOT NULL,
 proposal_id uuid NOT NULL,
 question_id uuid NOT NULL,
 version bigint NOT NULL CHECK(version>0),
 actor_id uuid NOT NULL REFERENCES users(id),
 request_id uuid NOT NULL,
 request_hash text NOT NULL CHECK(request_hash~'^[a-f0-9]{64}$'),
 kind text NOT NULL CHECK(kind IN ('OPEN','PROPOSE','DECIDE','REOPEN','SET_BLOCKING')),
 question_text text NOT NULL CHECK(product_question_text_valid(question_text)),
 blocking boolean NOT NULL,
 state text NOT NULL CHECK(state IN ('OPEN','PROPOSED','DECIDED')),
 answer text CHECK(answer IS NULL OR product_question_text_valid(answer)),
 decided_by uuid REFERENCES users(id),
 decided_at timestamptz,
 audit_id uuid NOT NULL UNIQUE REFERENCES audit_events(id) DEFERRABLE INITIALLY DEFERRED,
 event_id uuid NOT NULL UNIQUE REFERENCES outbox(id) DEFERRABLE INITIALLY DEFERRED,
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 created_transaction bigint NOT NULL DEFAULT txid_current(),
 UNIQUE(organization_id,request_id),
 UNIQUE(organization_id,proposal_id,question_id,version),
 FOREIGN KEY(organization_id,proposal_id,question_id) REFERENCES product_questions(organization_id,proposal_id,id),
 CHECK((state='OPEN' AND answer IS NULL AND decided_by IS NULL AND decided_at IS NULL)
   OR (state='PROPOSED' AND answer IS NOT NULL AND decided_by IS NULL AND decided_at IS NULL)
   OR (state='DECIDED' AND answer IS NOT NULL AND decided_by IS NOT NULL AND decided_at IS NOT NULL))
);
ALTER TABLE product_questions ADD FOREIGN KEY(opening_revision) REFERENCES product_question_revisions(id) DEFERRABLE INITIALLY DEFERRED;
CREATE INDEX product_question_latest ON product_question_revisions(organization_id,proposal_id,question_id,version DESC);
CREATE INDEX product_question_quota ON product_question_revisions(organization_id,created_at,actor_id);

CREATE FUNCTION product_question_origin_guard() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE p record;
BEGIN
 SELECT x.* INTO p FROM proposals x JOIN memberships m ON m.organization_id=x.organization_id JOIN users u ON u.id=m.user_id
 WHERE x.organization_id=NEW.organization_id AND x.id=NEW.proposal_id AND u.id=NEW.creator_id
   AND u.status='ACTIVE' AND m.role<>'READER' FOR UPDATE OF x FOR SHARE OF m,u;
 IF NOT FOUND OR p.state IN ('PROVISIONING','CLOSED','MERGED') OR NEW.created_transaction<>txid_current()
   OR ROW(NEW.repository_id,NEW.branch,NEW.head_sha,NEW.target_sha,NEW.proposal_version)
     IS DISTINCT FROM ROW(p.repository_id,p.branch,p.head_sha,p.target_sha,p.version) THEN
   RAISE EXCEPTION 'product question origin scope changed'; END IF;
 NEW.created_at=clock_timestamp();
 RETURN NEW;
END $$;
CREATE TRIGGER product_question_origin_guard BEFORE INSERT ON product_questions FOR EACH ROW EXECUTE FUNCTION product_question_origin_guard();

CREATE FUNCTION product_question_revision_guard() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE p record; q record; prior record; product boolean; creator boolean; was_blocking boolean=false;
BEGIN
 SELECT x.state,m.role,m.review_domains INTO p FROM proposals x JOIN memberships m ON m.organization_id=x.organization_id
 JOIN users u ON u.id=m.user_id WHERE x.organization_id=NEW.organization_id AND x.id=NEW.proposal_id
   AND u.id=NEW.actor_id AND u.status='ACTIVE' AND m.role<>'READER' FOR UPDATE OF x FOR SHARE OF m,u;
 IF NOT FOUND OR p.state IN ('PROVISIONING','CLOSED','MERGED') OR NEW.created_transaction<>txid_current() THEN
   RAISE EXCEPTION 'product question write permission required'; END IF;
 SELECT * INTO q FROM product_questions WHERE organization_id=NEW.organization_id AND proposal_id=NEW.proposal_id AND id=NEW.question_id;
 IF NOT FOUND THEN RAISE EXCEPTION 'product question scope mismatch'; END IF;
 product=p.role IN ('REVIEWER','MAINTAINER','ADMIN') AND 'PRODUCT'=ANY(p.review_domains);
 creator=q.creator_id=NEW.actor_id;
 NEW.created_at=clock_timestamp();
 SELECT * INTO prior FROM product_question_revisions WHERE organization_id=NEW.organization_id AND proposal_id=NEW.proposal_id
   AND question_id=NEW.question_id ORDER BY version DESC LIMIT 1;
 IF NOT FOUND THEN
   IF NEW.version<>1 OR NEW.kind<>'OPEN' OR NOT creator OR q.opening_revision<>NEW.id OR q.created_transaction<>txid_current()
     OR NEW.state IS DISTINCT FROM (CASE WHEN NEW.answer IS NULL THEN 'OPEN' ELSE 'PROPOSED' END)
     OR NEW.decided_by IS NOT NULL OR NEW.decided_at IS NOT NULL THEN
     RAISE EXCEPTION 'product question opening must remain undecided'; END IF;
 ELSE
   IF NEW.version<>prior.version+1 OR NEW.kind='OPEN' OR NEW.question_text IS DISTINCT FROM prior.question_text THEN
     RAISE EXCEPTION 'product question version or text changed'; END IF;
   was_blocking=prior.blocking AND prior.state<>'DECIDED';
   IF NEW.kind='PROPOSE' THEN
     IF prior.state='DECIDED' OR NEW.state<>'PROPOSED' OR NEW.blocking<>prior.blocking OR NEW.answer IS NULL
       OR NEW.decided_by IS NOT NULL OR NEW.decided_at IS NOT NULL THEN
       RAISE EXCEPTION 'product question answer requires proposal state'; END IF;
   ELSIF NEW.kind='DECIDE' THEN
     IF NOT product OR NEW.state<>'DECIDED' OR NEW.blocking<>prior.blocking OR NEW.answer IS NULL OR NEW.decided_by IS DISTINCT FROM NEW.actor_id THEN
       RAISE EXCEPTION 'qualified product decision required'; END IF;
     NEW.decided_at=NEW.created_at;
   ELSIF NEW.kind='REOPEN' THEN
     IF NOT (creator OR product) OR prior.state='OPEN' OR NEW.state<>'OPEN' OR NEW.answer IS NOT NULL
       OR NEW.decided_by IS NOT NULL OR NEW.decided_at IS NOT NULL OR NEW.blocking<>prior.blocking THEN
       RAISE EXCEPTION 'product question reopen forbidden'; END IF;
   ELSIF NEW.kind='SET_BLOCKING' THEN
     IF NEW.blocking=prior.blocking OR (NOT NEW.blocking AND NOT product) OR (NEW.blocking AND NOT (creator OR product))
       OR ROW(NEW.state,NEW.answer,NEW.decided_by,NEW.decided_at) IS DISTINCT FROM ROW(prior.state,prior.answer,prior.decided_by,prior.decided_at) THEN
       RAISE EXCEPTION 'product question blocking change forbidden'; END IF;
   END IF;
 END IF;
 IF p.state='MERGING' AND NEW.blocking AND NEW.state<>'DECIDED' AND NOT was_blocking THEN
   RAISE EXCEPTION 'new product question blocker forbidden during merge'; END IF;
 PERFORM pg_advisory_xact_lock(hashtextextended('product-question-quota:'||NEW.organization_id::text,0));
 IF (SELECT count(*) FROM product_question_revisions WHERE organization_id=NEW.organization_id AND actor_id=NEW.actor_id AND created_at>clock_timestamp()-interval '10 minutes')>=60
   OR (SELECT count(*) FROM product_question_revisions WHERE organization_id=NEW.organization_id AND created_at>clock_timestamp()-interval '10 minutes')>=600 THEN
   RAISE EXCEPTION 'product question quota exceeded'; END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER product_question_revision_guard BEFORE INSERT ON product_question_revisions FOR EACH ROW EXECUTE FUNCTION product_question_revision_guard();

CREATE FUNCTION product_question_complete() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF NOT EXISTS(SELECT 1 FROM audit_events a JOIN outbox e ON e.id=NEW.event_id WHERE a.id=NEW.audit_id
   AND a.organization_id=NEW.organization_id AND a.actor_id=NEW.actor_id AND a.action='PRODUCT_QUESTION_CHANGED'
   AND a.entity_id=NEW.question_id AND a.after_ref=NEW.id::text AND a.result=NEW.kind
   AND e.organization_id=NEW.organization_id AND e.entity_id=NEW.proposal_id AND e.event_type='PRODUCT_QUESTION_CHANGED'
   AND e.payload=jsonb_build_object('question_id',NEW.question_id,'revision_id',NEW.id,'version',NEW.version,'kind',NEW.kind)) THEN
   RAISE EXCEPTION 'product question requires atomic audit and event'; END IF;
 RETURN NULL;
END $$;
CREATE CONSTRAINT TRIGGER product_question_complete AFTER INSERT ON product_question_revisions DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION product_question_complete();
CREATE FUNCTION product_question_opening_complete() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF NOT EXISTS(SELECT 1 FROM product_question_revisions WHERE id=NEW.opening_revision AND organization_id=NEW.organization_id
   AND proposal_id=NEW.proposal_id AND question_id=NEW.id AND version=1 AND kind='OPEN' AND actor_id=NEW.creator_id) THEN
   RAISE EXCEPTION 'product question opening revision required'; END IF;
 RETURN NULL;
END $$;
CREATE CONSTRAINT TRIGGER product_question_opening_complete AFTER INSERT ON product_questions DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION product_question_opening_complete();
CREATE TRIGGER product_question_origin_immutable BEFORE UPDATE OR DELETE ON product_questions FOR EACH ROW EXECUTE FUNCTION review_submission_immutable();
CREATE TRIGGER product_question_origin_no_truncate BEFORE TRUNCATE ON product_questions FOR EACH STATEMENT EXECUTE FUNCTION review_submission_immutable();
CREATE TRIGGER product_question_revision_immutable BEFORE UPDATE OR DELETE ON product_question_revisions FOR EACH ROW EXECUTE FUNCTION review_submission_immutable();
CREATE TRIGGER product_question_revision_no_truncate BEFORE TRUNCATE ON product_question_revisions FOR EACH STATEMENT EXECUTE FUNCTION review_submission_immutable();
CREATE FUNCTION product_question_event_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF EXISTS(SELECT 1 FROM product_question_revisions WHERE event_id=OLD.id) THEN
   IF TG_OP='DELETE' THEN RAISE EXCEPTION 'product question event is durable'; END IF;
   IF ROW(NEW.id,NEW.organization_id,NEW.event_type,NEW.entity_id,NEW.payload,NEW.created_at)
     IS DISTINCT FROM ROW(OLD.id,OLD.organization_id,OLD.event_type,OLD.entity_id,OLD.payload,OLD.created_at)
     OR (OLD.sent_at IS NOT NULL AND NEW.sent_at IS DISTINCT FROM OLD.sent_at) THEN
     RAISE EXCEPTION 'product question event scope is immutable'; END IF;
 END IF;
 IF TG_OP='DELETE' THEN RETURN OLD; END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER product_question_event_guard BEFORE UPDATE OR DELETE ON outbox FOR EACH ROW EXECUTE FUNCTION product_question_event_guard();
