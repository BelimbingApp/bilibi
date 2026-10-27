-- The causes audit erasure may name (ADR 05), registered once with
-- base/audit: an erasure request that has run, or a sweep still running in
-- the tenant (started within the last day, so a stopped run cannot be reused).
-- Anything else, including a request that does not exist or is another
-- tenant's, is refused by the database.
CREATE FUNCTION base_privacy_erasure_cause(p_tenant bigint, p_request bigint) RETURNS boolean
LANGUAGE sql STABLE AS $$
  SELECT EXISTS (SELECT 1 FROM base_privacy_requests
                  WHERE tenant_id = p_tenant AND id = p_request AND kind = 'erasure'
                    AND status IN ('running', 'completed'))
$$;
CREATE FUNCTION base_privacy_sweep_cause(p_tenant bigint, p_sweep bigint) RETURNS boolean
LANGUAGE sql STABLE AS $$
  SELECT EXISTS (SELECT 1 FROM base_privacy_sweeps
                  WHERE tenant_id = p_tenant AND id = p_sweep AND finished_at IS NULL
                    AND started_at > clock_timestamp() - interval '1 day')
$$;
INSERT INTO base_audit_erasure_causes(cause, checker) VALUES
  ('request', 'base_privacy_erasure_cause(bigint,bigint)'::regprocedure),
  ('sweep', 'base_privacy_sweep_cause(bigint,bigint)'::regprocedure);

-- Cleanup an erasure still owes: written when the request completes while a
-- legal hold kept some of the person's records. The daily retention run
-- retries it, whatever the class settings say, until no hold keeps anything;
-- then it is done. One open row per request; rows are never removed.
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
