-- Cleanup an erasure still owes (ADR 05): written when the request completes
-- while a store kept some of the person's records for a legal hold or a
-- retention date, or deferred its answer. The daily retention run retries it,
-- whatever the class settings say, until nothing is kept for those reasons;
-- then it is done. One row per request; rows are never removed.
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
    done_at timestamptz,
    UNIQUE (request_id)
);
CREATE INDEX base_privacy_resumptions_open ON base_privacy_resumptions(tenant_id, id) WHERE done_at IS NULL;

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
