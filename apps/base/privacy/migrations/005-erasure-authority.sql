-- Cleanup an erasure still owes (ADR 05): written when the request completes
-- while a store kept some of the person's records for a legal hold or a
-- retention date, or deferred its answer. The daily retention run retries it,
-- whatever the class settings say, from not_before (the earliest date a kept
-- record or deferral ends; none while a hold, which has no end, keeps one),
-- until nothing is kept for those reasons; then it is done. One row per
-- request; rows are never removed.
CREATE TABLE base_privacy_resumptions (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    tenant_id bigint NOT NULL REFERENCES tenants(id) ON DELETE RESTRICT,
    request_id bigint NOT NULL REFERENCES base_privacy_requests(id) ON DELETE RESTRICT,
    person_id bigint NOT NULL CHECK (person_id > 0),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    runs integer NOT NULL DEFAULT 0 CHECK (runs >= 0),
    last_run_at timestamptz,
    -- Counts of the last run only: never a personal value.
    last_result jsonb,
    not_before timestamptz,
    done_at timestamptz,
    UNIQUE (request_id)
);
CREATE INDEX base_privacy_resumptions_open ON base_privacy_resumptions(tenant_id, id) WHERE done_at IS NULL;

-- When an owed answer (a store's, or a cleanup run's) is worth retrying: the
-- earliest end of a record kept for a retention date or of a deferral. NULL,
-- meaning the next run, while a hold keeps a record or a date is missing or
-- unreadable.
CREATE FUNCTION base_privacy_owed_after(p_answer jsonb) RETURNS timestamptz
LANGUAGE plpgsql STABLE AS $$
DECLARE
  kept jsonb := CASE WHEN jsonb_typeof(p_answer->'kept') = 'array' THEN p_answer->'kept' ELSE '[]'::jsonb END;
BEGIN
  IF EXISTS (SELECT 1 FROM jsonb_array_elements(kept) k
              WHERE k->>'reason' = 'legal_hold' OR (k->>'reason' = 'retained_until' AND k->>'until' IS NULL))
     OR (jsonb_typeof(p_answer->'deferral') = 'object' AND p_answer->'deferral'->>'complete_by' IS NULL) THEN
    RETURN NULL;
  END IF;
  RETURN (SELECT min(t) FROM (
    SELECT (k->>'until')::timestamptz AS t FROM jsonb_array_elements(kept) k WHERE k->>'reason' = 'retained_until'
    UNION ALL
    SELECT (p_answer->'deferral'->>'complete_by')::timestamptz WHERE jsonb_typeof(p_answer->'deferral') = 'object') dates);
EXCEPTION WHEN invalid_datetime_format OR datetime_field_overflow THEN
  RETURN NULL;
END;
$$;

CREATE FUNCTION base_privacy_resumptions_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF TG_OP = 'UPDATE' AND OLD.done_at IS NULL
     AND (NEW.id, NEW.tenant_id, NEW.request_id, NEW.person_id, NEW.created_at)
         IS NOT DISTINCT FROM (OLD.id, OLD.tenant_id, OLD.request_id, OLD.person_id, OLD.created_at)
     AND NEW.runs >= OLD.runs THEN
    RETURN NEW;
  END IF;
  RAISE EXCEPTION 'owed erasure cleanup only records its runs, and is never removed' USING ERRCODE = '42501';
END;
$$;
CREATE TRIGGER base_privacy_resumptions_guard BEFORE UPDATE OR DELETE ON base_privacy_resumptions
    FOR EACH ROW EXECUTE FUNCTION base_privacy_resumptions_guard();
CREATE TRIGGER base_privacy_resumptions_truncate_guard BEFORE TRUNCATE ON base_privacy_resumptions
    FOR EACH STATEMENT EXECUTE FUNCTION base_privacy_resumptions_guard();

-- The causes audit erasure may name, registered once with base/audit. A
-- request authorises erasure only of its own person: while it runs, or once
-- completed only while it still owes cleanup. A sweep authorises while it is
-- still running in the tenant (started within the last day, so a stopped run
-- cannot be reused). Anything else, including another person, a request that
-- does not exist or is another tenant's, is refused by the database.
CREATE FUNCTION base_privacy_erasure_cause(p_tenant bigint, p_request bigint, p_person bigint) RETURNS boolean
LANGUAGE sql STABLE AS $$
  SELECT EXISTS (SELECT 1 FROM base_privacy_requests r
                  WHERE r.tenant_id = p_tenant AND r.id = p_request AND r.kind = 'erasure'
                    AND r.person_id = p_person
                    AND (r.status = 'running'
                         OR (r.status = 'completed' AND EXISTS (
                               SELECT 1 FROM base_privacy_resumptions o
                                WHERE o.tenant_id = p_tenant AND o.request_id = r.id AND o.done_at IS NULL))))
$$;
CREATE FUNCTION base_privacy_sweep_cause(p_tenant bigint, p_sweep bigint, p_person bigint) RETURNS boolean
LANGUAGE sql STABLE AS $$
  SELECT EXISTS (SELECT 1 FROM base_privacy_sweeps
                  WHERE tenant_id = p_tenant AND id = p_sweep AND finished_at IS NULL
                    AND started_at > clock_timestamp() - interval '1 day')
$$;
INSERT INTO base_audit_erasure_causes(cause, checker) VALUES
  ('request', 'base_privacy_erasure_cause(bigint,bigint,bigint)'::regprocedure),
  ('sweep', 'base_privacy_sweep_cause(bigint,bigint,bigint)'::regprocedure);
