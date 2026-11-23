-- Individual approval counts are real stored actions. Approve-all remains a
-- separate explicit snapshot decision; it never fabricates individual records.
CREATE FUNCTION review_unit_approval_coverage(org uuid,p uuid,h uuid,actor uuid,area text,fp text)
RETURNS TABLE(available boolean,total_units integer,approved_units integer)
LANGUAGE sql STABLE AS $$
 WITH approved AS (
  SELECT DISTINCT unit_id FROM review_unit_approvals
  WHERE organization_id=org AND proposal_id=p AND snapshot_id=h AND reviewer_id=actor AND domain=area AND fingerprint=fp
 )
 SELECT s.review_units_version=1 AND s.unit_count>0 AND count(u.unit_id)=s.unit_count,
   s.unit_count,count(a.unit_id)::integer
 FROM review_snapshots s LEFT JOIN review_snapshot_units u ON u.snapshot_id=s.id AND u.organization_id=s.organization_id AND u.proposal_id=s.proposal_id
 LEFT JOIN approved a ON a.unit_id=u.unit_id
 WHERE s.organization_id=org AND s.proposal_id=p AND s.id=h AND s.fingerprint=fp AND s.state='READY'
 GROUP BY s.review_units_version,s.unit_count
$$;
ALTER TABLE official_reviews DROP CONSTRAINT official_reviews_decision_check;
ALTER TABLE official_reviews ADD CONSTRAINT official_reviews_decision_check CHECK(decision IN ('APPROVE','APPROVE_ALL','REQUEST_CHANGES','COMMENT'));
ALTER TABLE official_reviews DROP CONSTRAINT official_reviews_explanation_check;
ALTER TABLE official_reviews ADD CONSTRAINT official_reviews_explanation_check CHECK(length(btrim(explanation))<=10000 AND (decision='APPROVE' OR length(btrim(explanation))>=1));
CREATE OR REPLACE FUNCTION official_review_insert() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF NEW.created_transaction<>txid_current() OR NOT EXISTS(
    SELECT 1 FROM review_snapshots h JOIN proposals p ON p.id=h.proposal_id AND p.organization_id=h.organization_id
      JOIN memberships m ON m.organization_id=p.organization_id AND m.user_id=NEW.reviewer_id
      JOIN users u ON u.id=m.user_id
    WHERE h.id=NEW.snapshot_id AND h.organization_id=NEW.organization_id AND h.proposal_id=NEW.proposal_id
      AND h.state='READY' AND h.fingerprint=NEW.fingerprint AND h.head_sha=NEW.head_sha AND h.target_sha=NEW.target_sha
      AND h.policy_version=NEW.policy_version AND h.configuration_version=NEW.configuration_version
      AND pr_sync_snapshot_current(h.organization_id,h.proposal_id,h.id)
      AND p.author_id<>NEW.reviewer_id AND h.metadata#>>'{context,author_id}'<>NEW.reviewer_id::text
      AND m.role IN ('REVIEWER','MAINTAINER','ADMIN') AND u.status='ACTIVE' AND NEW.domain=ANY(m.review_domains)
      AND EXISTS(SELECT 1 FROM jsonb_array_elements(h.metadata->'assignments') a WHERE a->>'reviewer_id'=NEW.reviewer_id::text AND a->>'domain'=NEW.domain)
      AND (NEW.decision NOT IN ('APPROVE','APPROVE_ALL') OR (h.recorded_base_sha=h.target_sha AND h.merge_base_sha=h.target_sha AND h.metadata#>>'{summary,ui_impact}' IN ('YES','NO')))
      AND (NEW.decision<>'APPROVE' OR EXISTS(SELECT 1 FROM review_unit_approval_coverage(NEW.organization_id,NEW.proposal_id,NEW.snapshot_id,NEW.reviewer_id,NEW.domain,NEW.fingerprint) c WHERE c.available AND c.total_units>0 AND c.approved_units=c.total_units))
      FOR SHARE OF h,p,m,u
  ) THEN RAISE EXCEPTION 'official review requires current qualified assignment and snapshot'; END IF;
  RETURN NEW;
END $$;
