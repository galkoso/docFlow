-- Check publication is a separate side-effect lane. An absent setting disables
-- admission; the configured numeric App must match provider observations.
ALTER TABLE repositories ADD COLUMN check_app_id bigint CHECK(check_app_id>0);
ALTER TABLE git_operations DROP CONSTRAINT git_operations_kind_check;
ALTER TABLE git_operations ADD CONSTRAINT git_operations_kind_check
 CHECK(kind IN ('CREATE','COMMIT','SUBMIT','EXPORT','IMPORT_APPLY','REFRESH_BASE','MERGE','REVERT','PR_SYNC','REVIEW_CHECK'));
CREATE UNIQUE INDEX proposal_active_review_check ON git_operations(organization_id,proposal_id)
 WHERE kind='REVIEW_CHECK' AND state IN ('PENDING','RUNNING','UNKNOWN');
CREATE UNIQUE INDEX review_check_job ON jobs(operation_id) WHERE kind='PUBLISH_REVIEW_CHECK';

CREATE TABLE review_check_intents (
 operation_id uuid PRIMARY KEY,
 job_id uuid NOT NULL UNIQUE REFERENCES jobs(id),
 organization_id uuid NOT NULL,
 proposal_id uuid NOT NULL,
 repository_id uuid NOT NULL,
 actor_id uuid NOT NULL REFERENCES users(id),
 snapshot_id uuid NOT NULL,
 remote_id bigint NOT NULL CHECK(remote_id>0),
 installation_id bigint NOT NULL CHECK(installation_id>0),
 app_id bigint NOT NULL CHECK(app_id>0),
 branch text NOT NULL,
 head_sha text NOT NULL CHECK(head_sha~'^[a-f0-9]{40,128}$' AND length(head_sha)%2=0),
 target_sha text NOT NULL CHECK(target_sha~'^[a-f0-9]{40,128}$' AND length(target_sha)%2=0),
 merge_base_sha text NOT NULL CHECK(merge_base_sha~'^[a-f0-9]{40,128}$' AND length(merge_base_sha)%2=0),
 proposal_version bigint NOT NULL CHECK(proposal_version>0),
 configuration_version bigint NOT NULL CHECK(configuration_version>0),
 policy_version bigint NOT NULL CHECK(policy_version>0),
 fingerprint text NOT NULL CHECK(fingerprint~'^[a-f0-9]{64}$'),
 evidence_digest text NOT NULL CHECK(evidence_digest~'^[a-f0-9]{64}$'),
 authority_digest text NOT NULL CHECK(authority_digest~'^[a-f0-9]{64}$'),
 request_hash text NOT NULL CHECK(request_hash~'^[a-f0-9]{64}$'),
 purpose text NOT NULL CHECK(purpose IN ('EVALUATION','INVALIDATION')),
 result text NOT NULL CHECK(result IN ('PENDING','FAILURE','SUCCESS')),
 report_digest text CHECK(report_digest~'^[a-f0-9]{64}$'),
 invalidation_reason text CHECK(invalidation_reason IN ('HEAD_CHANGED','TARGET_CHANGED','POLICY_CHANGED','AUTHORITY_CHANGED','SNAPSHOT_CHANGED','BLOCKERS_CHANGED','SUPERSEDED')),
 target_run_id bigint CHECK(target_run_id>0),
 target_operation uuid,
 audit_id uuid NOT NULL UNIQUE REFERENCES audit_events(id) DEFERRABLE INITIALLY DEFERRED,
 event_id uuid NOT NULL UNIQUE REFERENCES outbox(id) DEFERRABLE INITIALLY DEFERRED,
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 created_transaction bigint NOT NULL DEFAULT txid_current(),
 FOREIGN KEY(organization_id,operation_id) REFERENCES git_operations(organization_id,id),
 FOREIGN KEY(organization_id,proposal_id) REFERENCES proposals(organization_id,id),
 FOREIGN KEY(organization_id,repository_id) REFERENCES repositories(organization_id,id),
 FOREIGN KEY(organization_id,proposal_id,snapshot_id) REFERENCES review_snapshots(organization_id,proposal_id,id),
 CHECK((purpose='EVALUATION' AND report_digest IS NOT NULL AND invalidation_reason IS NULL AND target_run_id IS NULL AND target_operation IS NULL)
    OR (purpose='INVALIDATION' AND result='FAILURE' AND report_digest IS NULL AND invalidation_reason IS NOT NULL AND target_run_id IS NOT NULL
      AND target_operation IS NOT NULL AND target_operation<>operation_id))
);
CREATE TABLE review_check_writes (
 operation_id uuid NOT NULL REFERENCES review_check_intents(operation_id),
 phase text NOT NULL CHECK(phase IN ('CREATE','UPDATE')),
 run_id bigint CHECK(run_id>0),
 check_operation uuid NOT NULL,
 result text NOT NULL CHECK(result IN ('PENDING','FAILURE','SUCCESS')),
 fence_worker uuid NOT NULL,
 fence_attempt integer NOT NULL CHECK(fence_attempt>0),
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 created_transaction bigint NOT NULL DEFAULT txid_current(),
 PRIMARY KEY(operation_id,phase),
 CHECK((phase='CREATE' AND run_id IS NULL AND result='PENDING') OR (phase='UPDATE' AND run_id IS NOT NULL))
);
CREATE TABLE review_check_runs (
 operation_id uuid PRIMARY KEY REFERENCES review_check_intents(operation_id),
 organization_id uuid NOT NULL,
 proposal_id uuid NOT NULL,
 repository_id uuid NOT NULL,
 remote_id bigint NOT NULL CHECK(remote_id>0),
 app_id bigint NOT NULL CHECK(app_id>0),
 head_sha text NOT NULL,
 run_id bigint NOT NULL CHECK(run_id>0),
 fence_worker uuid NOT NULL,
 fence_attempt integer NOT NULL CHECK(fence_attempt>0),
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 UNIQUE(repository_id,app_id,run_id),
 UNIQUE(operation_id,run_id),
 FOREIGN KEY(organization_id,proposal_id) REFERENCES proposals(organization_id,id),
 FOREIGN KEY(organization_id,repository_id) REFERENCES repositories(organization_id,id)
);
ALTER TABLE review_check_intents ADD FOREIGN KEY(target_operation,target_run_id) REFERENCES review_check_runs(operation_id,run_id);
CREATE TABLE review_check_results (
 operation_id uuid PRIMARY KEY REFERENCES review_check_intents(operation_id),
 run_id bigint NOT NULL CHECK(run_id>0),
 check_operation uuid NOT NULL,
 result text NOT NULL CHECK(result IN ('PENDING','FAILURE','SUCCESS')),
 request_hash text NOT NULL CHECK(request_hash~'^[a-f0-9]{64}$'),
 fence_worker uuid NOT NULL,
 fence_attempt integer NOT NULL CHECK(fence_attempt>0),
 audit_id uuid NOT NULL UNIQUE REFERENCES audit_events(id) DEFERRABLE INITIALLY DEFERRED,
 event_id uuid NOT NULL UNIQUE REFERENCES outbox(id) DEFERRABLE INITIALLY DEFERRED,
 created_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
CREATE INDEX review_check_claim ON jobs(available_at,created_at,id)
 WHERE kind='PUBLISH_REVIEW_CHECK' AND state IN ('QUEUED','RUNNING');
CREATE INDEX review_check_admission ON review_check_intents(organization_id,created_at,actor_id);

-- Receipt adoption can survive changed approvals. New provider writes and final
-- publication additionally need current actor authority and snapshot metadata.
CREATE FUNCTION review_check_lock_scope(op uuid, current_required boolean) RETURNS boolean LANGUAGE plpgsql AS $$
BEGIN
 RETURN EXISTS(SELECT 1 FROM review_check_intents i
  JOIN proposals p ON p.id=i.proposal_id AND p.organization_id=i.organization_id
  JOIN repositories r ON r.id=p.repository_id AND r.organization_id=p.organization_id
  JOIN organizations g ON g.id=p.organization_id
  WHERE i.operation_id=op AND p.repository_id=i.repository_id AND p.branch=i.branch
    AND r.provider='GITHUB' AND r.default_branch='main' AND r.check_app_id=i.app_id
    AND ltrim(r.remote_id,'0+')=i.remote_id::text AND ltrim(r.installation_id,'0+')=i.installation_id::text
    AND (NOT current_required OR (
      EXISTS(SELECT 1 FROM memberships m JOIN users u ON u.id=m.user_id
        WHERE m.organization_id=i.organization_id AND m.user_id=i.actor_id AND u.status='ACTIVE'
          AND ((i.purpose='EVALUATION' AND m.role IN ('CONTRIBUTOR','REVIEWER','MAINTAINER','ADMIN'))
            OR (i.purpose='INVALIDATION' AND m.role IN ('MAINTAINER','ADMIN'))) FOR SHARE OF m,u)
      AND (i.purpose='INVALIDATION' OR (p.version=i.proposal_version AND p.head_sha=i.head_sha AND p.target_sha=i.target_sha
        AND p.review_configuration_version=i.configuration_version AND g.policy_version=i.policy_version
        AND p.state IN ('IN_REVIEW','CHANGES_REQUESTED','APPROVED')
        AND pr_sync_snapshot_current(i.organization_id,i.proposal_id,i.snapshot_id)))))
  FOR SHARE OF i,p,r,g);
END $$;
CREATE FUNCTION review_check_lease(op uuid, worker uuid, attempt integer) RETURNS boolean LANGUAGE sql AS $$
 SELECT EXISTS(SELECT 1 FROM review_check_intents i JOIN jobs j ON j.id=i.job_id
   JOIN git_operations o ON o.id=i.operation_id AND o.organization_id=i.organization_id
   WHERE i.operation_id=op AND j.kind='PUBLISH_REVIEW_CHECK' AND j.operation_id=op AND j.organization_id=i.organization_id
     AND j.state='RUNNING' AND j.lease_owner=worker AND j.attempts=attempt AND j.leased_until>clock_timestamp()
     AND j.payload=jsonb_build_object('operation_id',op) AND o.kind='REVIEW_CHECK' AND o.state IN ('PENDING','RUNNING','UNKNOWN')
     AND o.request_hash=i.request_hash AND o.expected_sha=i.head_sha
   FOR SHARE OF j,o)
$$;
CREATE FUNCTION review_check_intent_insert() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF NEW.created_transaction<>txid_current() OR NOT EXISTS(SELECT 1 FROM git_operations o
  JOIN jobs j ON j.operation_id=o.id AND j.organization_id=o.organization_id
  JOIN proposals p ON p.id=o.proposal_id AND p.organization_id=o.organization_id
  JOIN repositories r ON r.id=p.repository_id AND r.organization_id=p.organization_id
  JOIN organizations g ON g.id=p.organization_id
  JOIN review_snapshots h ON h.id=NEW.snapshot_id AND h.organization_id=p.organization_id AND h.proposal_id=p.id
  JOIN memberships m ON m.organization_id=p.organization_id AND m.user_id=NEW.actor_id
  JOIN users u ON u.id=m.user_id
  WHERE o.id=NEW.operation_id AND o.organization_id=NEW.organization_id AND o.proposal_id=NEW.proposal_id AND o.actor_id=NEW.actor_id
    AND o.kind='REVIEW_CHECK' AND o.state='PENDING' AND o.idempotency_key=NEW.operation_id::text
    AND o.request_hash=NEW.request_hash AND o.expected_sha=NEW.head_sha
    AND j.id=NEW.job_id AND j.kind='PUBLISH_REVIEW_CHECK' AND j.state='QUEUED' AND j.attempts=0
    AND j.payload=jsonb_build_object('operation_id',NEW.operation_id) AND j.max_attempts BETWEEN 1 AND 32
    AND p.repository_id=NEW.repository_id AND p.branch=NEW.branch AND r.provider='GITHUB' AND r.default_branch='main'
    AND r.check_app_id=NEW.app_id AND ltrim(r.remote_id,'0+')=NEW.remote_id::text AND ltrim(r.installation_id,'0+')=NEW.installation_id::text
    AND h.state='READY' AND h.repository_id=NEW.repository_id AND h.branch=NEW.branch AND h.head_sha=NEW.head_sha
    AND h.target_sha=NEW.target_sha AND h.merge_base_sha=NEW.merge_base_sha AND h.fingerprint=NEW.fingerprint
    AND h.evidence_digest=NEW.evidence_digest AND h.configuration_version=NEW.configuration_version AND h.policy_version=NEW.policy_version
    AND ltrim(h.remote_id,'0+')=NEW.remote_id::text AND ltrim(h.installation_id,'0+')=NEW.installation_id::text
    AND u.status='ACTIVE' AND ((NEW.purpose='EVALUATION' AND m.role IN ('CONTRIBUTOR','REVIEWER','MAINTAINER','ADMIN')
      AND p.version=NEW.proposal_version AND pr_sync_snapshot_current(NEW.organization_id,NEW.proposal_id,NEW.snapshot_id)
      AND g.policy_version=NEW.policy_version AND p.state IN ('IN_REVIEW','CHANGES_REQUESTED','APPROVED'))
     OR (NEW.purpose='INVALIDATION' AND m.role IN ('MAINTAINER','ADMIN') AND EXISTS(SELECT 1 FROM review_check_runs b
       WHERE b.operation_id=NEW.target_operation AND b.run_id=NEW.target_run_id AND b.organization_id=NEW.organization_id
        AND b.proposal_id=NEW.proposal_id AND b.repository_id=NEW.repository_id AND b.remote_id=NEW.remote_id AND b.app_id=NEW.app_id AND b.head_sha=NEW.head_sha)))
    FOR SHARE OF o,j,p,r,g,h,m,u) THEN RAISE EXCEPTION 'check intent requires current scoped authority, snapshot, App and exact queued work'; END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER review_check_intent_insert BEFORE INSERT ON review_check_intents FOR EACH ROW EXECUTE FUNCTION review_check_intent_insert();
CREATE FUNCTION review_check_write_insert() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE i review_check_intents;
BEGIN
 SELECT * INTO i FROM review_check_intents WHERE operation_id=NEW.operation_id FOR SHARE;
 IF NEW.created_transaction<>txid_current() OR NOT review_check_lease(NEW.operation_id,NEW.fence_worker,NEW.fence_attempt)
   OR NOT review_check_lock_scope(NEW.operation_id,true) THEN RAISE EXCEPTION 'check write requires current lease and authority'; END IF;
 IF NEW.phase='CREATE' THEN
   IF i.purpose<>'EVALUATION' OR NEW.check_operation<>i.operation_id OR NEW.result<>'PENDING'
     OR EXISTS(SELECT 1 FROM review_check_writes WHERE operation_id=i.operation_id)
     OR NOT EXISTS(SELECT 1 FROM git_operations WHERE id=i.operation_id AND state='PENDING') THEN
     RAISE EXCEPTION 'check creation requires its original unstarted intent'; END IF;
 ELSE
   IF NEW.result<>i.result OR (i.purpose='EVALUATION' AND (NEW.check_operation<>i.operation_id
       OR NOT EXISTS(SELECT 1 FROM review_check_runs b WHERE b.operation_id=i.operation_id AND b.run_id=NEW.run_id)))
     OR (i.purpose='INVALIDATION' AND (NEW.check_operation<>i.target_operation OR NEW.run_id<>i.target_run_id)) THEN
     RAISE EXCEPTION 'check update requires its exact bound run and result'; END IF;
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER review_check_write_insert BEFORE INSERT ON review_check_writes FOR EACH ROW EXECUTE FUNCTION review_check_write_insert();
CREATE FUNCTION review_check_run_insert() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF NOT review_check_lease(NEW.operation_id,NEW.fence_worker,NEW.fence_attempt) OR NOT review_check_lock_scope(NEW.operation_id,false)
   OR NOT EXISTS(SELECT 1 FROM review_check_intents i JOIN review_check_writes w ON w.operation_id=i.operation_id AND w.phase='CREATE'
     WHERE i.operation_id=NEW.operation_id AND i.purpose='EVALUATION' AND i.organization_id=NEW.organization_id
       AND i.proposal_id=NEW.proposal_id AND i.repository_id=NEW.repository_id AND i.remote_id=NEW.remote_id
       AND i.app_id=NEW.app_id AND i.head_sha=NEW.head_sha) THEN RAISE EXCEPTION 'check run requires exact creation journal and current receipt fence'; END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER review_check_run_insert BEFORE INSERT ON review_check_runs FOR EACH ROW EXECUTE FUNCTION review_check_run_insert();
CREATE FUNCTION review_check_result_insert() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF NOT review_check_lease(NEW.operation_id,NEW.fence_worker,NEW.fence_attempt) OR NOT review_check_lock_scope(NEW.operation_id,true)
   OR NOT EXISTS(SELECT 1 FROM review_check_intents i JOIN review_check_writes w ON w.operation_id=i.operation_id AND w.phase='UPDATE'
     WHERE i.operation_id=NEW.operation_id AND i.request_hash=NEW.request_hash AND i.result=NEW.result
       AND w.run_id=NEW.run_id AND w.check_operation=NEW.check_operation AND w.result=NEW.result)
 THEN RAISE EXCEPTION 'check result requires exact update receipt, current authority and live fence'; END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER review_check_result_insert BEFORE INSERT ON review_check_results FOR EACH ROW EXECUTE FUNCTION review_check_result_insert();

CREATE FUNCTION review_check_scope_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF TG_TABLE_NAME='jobs' AND (OLD.kind='PUBLISH_REVIEW_CHECK' OR (TG_OP='UPDATE' AND NEW.kind='PUBLISH_REVIEW_CHECK')) THEN
   IF TG_OP='DELETE' THEN RAISE EXCEPTION 'check job is durable'; END IF;
   IF ROW(NEW.id,NEW.organization_id,NEW.operation_id,NEW.kind,NEW.payload,NEW.max_attempts,NEW.created_at)
     IS DISTINCT FROM ROW(OLD.id,OLD.organization_id,OLD.operation_id,OLD.kind,OLD.payload,OLD.max_attempts,OLD.created_at)
     OR (NEW.state='SUCCEEDED' AND (OLD.state<>'RUNNING' OR OLD.leased_until<=clock_timestamp()
       OR NOT EXISTS(SELECT 1 FROM review_check_results x WHERE x.operation_id=OLD.operation_id AND x.fence_worker=OLD.lease_owner AND x.fence_attempt=OLD.attempts)))
     OR (OLD.state IN ('SUCCEEDED','FAILED','CANCELLED') AND NEW.state<>OLD.state)
     OR NEW.attempts<OLD.attempts OR NEW.attempts>OLD.attempts+1 OR NEW.attempts>NEW.max_attempts
     OR (NEW.state='RUNNING' AND ((NEW.attempts=OLD.attempts AND (OLD.state<>'RUNNING' OR OLD.lease_owner<>NEW.lease_owner OR OLD.leased_until<=clock_timestamp()))
       OR (NEW.attempts=OLD.attempts+1 AND OLD.state='RUNNING' AND OLD.leased_until>clock_timestamp())
       OR NEW.leased_until>clock_timestamp()+interval '5 minutes')) THEN RAISE EXCEPTION 'check job requires dedicated live fencing'; END IF;
 ELSIF TG_TABLE_NAME='git_operations' AND (OLD.kind='REVIEW_CHECK' OR (TG_OP='UPDATE' AND NEW.kind='REVIEW_CHECK')) THEN
   IF TG_OP='DELETE' THEN RAISE EXCEPTION 'check operation is durable'; END IF;
   IF ROW(NEW.id,NEW.organization_id,NEW.proposal_id,NEW.actor_id,NEW.kind,NEW.idempotency_key,NEW.request_hash,NEW.expected_sha,NEW.created_at)
     IS DISTINCT FROM ROW(OLD.id,OLD.organization_id,OLD.proposal_id,OLD.actor_id,OLD.kind,OLD.idempotency_key,OLD.request_hash,OLD.expected_sha,OLD.created_at)
     OR (OLD.state IN ('SUCCEEDED','FAILED') AND NEW.state<>OLD.state)
     OR (NEW.state IN ('PENDING','FAILED') AND EXISTS(SELECT 1 FROM review_check_writes WHERE operation_id=OLD.id))
     OR (NEW.state IN ('RUNNING','UNKNOWN') AND NOT EXISTS(SELECT 1 FROM review_check_writes WHERE operation_id=OLD.id))
     OR (NEW.state='SUCCEEDED' AND NOT EXISTS(SELECT 1 FROM review_check_results WHERE operation_id=OLD.id)) THEN
     RAISE EXCEPTION 'check operation requires its durable write/result evidence'; END IF;
 END IF;
 IF TG_OP='DELETE' THEN RETURN OLD; END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER review_check_job_scope BEFORE UPDATE OR DELETE ON jobs FOR EACH ROW EXECUTE FUNCTION review_check_scope_guard();
CREATE TRIGGER review_check_operation_scope BEFORE UPDATE OR DELETE ON git_operations FOR EACH ROW EXECUTE FUNCTION review_check_scope_guard();
CREATE FUNCTION review_check_complete() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE op uuid; i review_check_intents; j jobs; o git_operations; x review_check_results;
BEGIN
 IF TG_TABLE_NAME='jobs' THEN IF NEW.kind<>'PUBLISH_REVIEW_CHECK' THEN RETURN NULL; END IF; op:=NEW.operation_id;
 ELSIF TG_TABLE_NAME='git_operations' THEN IF NEW.kind<>'REVIEW_CHECK' THEN RETURN NULL; END IF; op:=NEW.id;
 ELSE op:=NEW.operation_id; END IF;
 SELECT * INTO i FROM review_check_intents WHERE operation_id=op;
 SELECT * INTO j FROM jobs WHERE id=i.job_id;
 SELECT * INTO o FROM git_operations WHERE id=op;
 SELECT * INTO x FROM review_check_results WHERE operation_id=op;
 IF i.operation_id IS NULL OR j.id IS NULL OR o.id IS NULL OR o.kind<>'REVIEW_CHECK' OR j.kind<>'PUBLISH_REVIEW_CHECK'
   OR j.operation_id<>op OR j.organization_id<>i.organization_id OR o.organization_id<>i.organization_id
   OR o.actor_id<>i.actor_id OR o.proposal_id<>i.proposal_id OR o.request_hash<>i.request_hash OR o.expected_sha<>i.head_sha
   OR j.payload<>jsonb_build_object('operation_id',op)
   OR NOT EXISTS(SELECT 1 FROM audit_events a JOIN outbox e ON e.id=i.event_id
      WHERE a.id=i.audit_id AND a.organization_id=i.organization_id AND a.entity_id=i.proposal_id AND a.actor_id=i.actor_id
       AND a.operation_id=op AND a.action='REVIEW_CHECK_RESERVED' AND a.after_ref=i.snapshot_id::text AND a.result='QUEUED'
       AND e.organization_id=i.organization_id AND e.entity_id=i.proposal_id AND e.event_type='REVIEW_CHECK_QUEUED'
       AND e.payload=jsonb_build_object('operation_id',op,'snapshot_id',i.snapshot_id,'purpose',i.purpose,'result',i.result))
   OR (EXISTS(SELECT 1 FROM review_check_writes WHERE operation_id=op) AND o.state NOT IN ('RUNNING','UNKNOWN','SUCCEEDED'))
   OR (o.state='FAILED' AND j.state NOT IN ('FAILED','CANCELLED'))
   OR (o.state='PENDING' AND j.state NOT IN ('QUEUED','RUNNING'))
   OR (o.state='RUNNING' AND j.state<>'RUNNING')
   OR (j.state IN ('FAILED','CANCELLED') AND
     ((EXISTS(SELECT 1 FROM review_check_writes WHERE operation_id=op) AND (j.state<>'FAILED' OR o.state<>'UNKNOWN'))
       OR (NOT EXISTS(SELECT 1 FROM review_check_writes WHERE operation_id=op) AND o.state<>'FAILED')))
   OR (x.operation_id IS NOT NULL AND (j.state<>'SUCCEEDED' OR o.state<>'SUCCEEDED' OR o.result_sha IS DISTINCT FROM i.head_sha
      OR j.result IS DISTINCT FROM jsonb_build_object('phase','REVIEW_CHECK_PUBLISHED','run_id',x.run_id,'result',x.result)
      OR NOT EXISTS(SELECT 1 FROM audit_events a JOIN outbox e ON e.id=x.event_id
        WHERE a.id=x.audit_id AND a.organization_id=i.organization_id AND a.entity_id=i.proposal_id AND a.actor_id=i.actor_id
          AND a.operation_id=op AND a.action='REVIEW_CHECK_PUBLISHED' AND a.after_ref=x.run_id::text AND a.result='PUBLISHED'
          AND e.organization_id=i.organization_id AND e.entity_id=i.proposal_id AND e.event_type='REVIEW_CHECK_PUBLISHED'
          AND e.payload=jsonb_build_object('operation_id',op,'snapshot_id',i.snapshot_id,'run_id',x.run_id,'result',x.result))))
   OR ((j.state='SUCCEEDED' OR o.state='SUCCEEDED') AND x.operation_id IS NULL) THEN
   RAISE EXCEPTION 'check journal is incomplete'; END IF;
 RETURN NULL;
END $$;
CREATE CONSTRAINT TRIGGER review_check_job_complete AFTER INSERT OR UPDATE ON jobs DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION review_check_complete();
CREATE CONSTRAINT TRIGGER review_check_operation_complete AFTER INSERT OR UPDATE ON git_operations DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION review_check_complete();
CREATE CONSTRAINT TRIGGER review_check_intent_complete AFTER INSERT ON review_check_intents DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION review_check_complete();
CREATE CONSTRAINT TRIGGER review_check_write_complete AFTER INSERT ON review_check_writes DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION review_check_complete();
CREATE CONSTRAINT TRIGGER review_check_result_complete AFTER INSERT ON review_check_results DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION review_check_complete();
CREATE FUNCTION review_check_event_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF EXISTS(SELECT 1 FROM review_check_intents WHERE event_id=OLD.id) OR EXISTS(SELECT 1 FROM review_check_results WHERE event_id=OLD.id) THEN
   IF TG_OP='DELETE' THEN RAISE EXCEPTION 'check event is durable'; END IF;
   IF ROW(NEW.id,NEW.organization_id,NEW.event_type,NEW.entity_id,NEW.payload,NEW.created_at)
     IS DISTINCT FROM ROW(OLD.id,OLD.organization_id,OLD.event_type,OLD.entity_id,OLD.payload,OLD.created_at)
     OR (OLD.sent_at IS NOT NULL AND NEW.sent_at IS DISTINCT FROM OLD.sent_at) THEN RAISE EXCEPTION 'check event is immutable'; END IF;
 END IF;
 IF TG_OP='DELETE' THEN RETURN OLD; END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER review_check_event_scope BEFORE UPDATE OR DELETE ON outbox FOR EACH ROW EXECUTE FUNCTION review_check_event_guard();
DO $$ DECLARE t text; BEGIN
 FOREACH t IN ARRAY ARRAY['review_check_intents','review_check_writes','review_check_runs','review_check_results'] LOOP
  EXECUTE format('CREATE TRIGGER %I BEFORE UPDATE OR DELETE ON %I FOR EACH ROW EXECUTE FUNCTION review_submission_immutable()',t||'_immutable',t);
  EXECUTE format('CREATE TRIGGER %I BEFORE TRUNCATE ON %I FOR EACH STATEMENT EXECUTE FUNCTION review_submission_immutable()',t||'_no_truncate',t);
 END LOOP;
END $$;
