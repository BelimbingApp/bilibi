-- Retention sweeps (ADR 05): one row per sweep transaction of one record
-- class in one tenant, with the cutoff it used and what it did. A sweep
-- records its result once; nothing else about it changes, and no row is
-- removed. Tenant periods live in the `privacy.retention.periods` setting.
CREATE TABLE base_privacy_sweeps (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    tenant_id bigint NOT NULL REFERENCES tenants(id) ON DELETE RESTRICT,
    class text NOT NULL CHECK (length(class) BETWEEN 1 AND 255),
    cutoff timestamptz NOT NULL,
    period_days integer NOT NULL CHECK (period_days BETWEEN 1 AND 36500),
    acted bigint NOT NULL DEFAULT 0 CHECK (acted >= 0),
    kept bigint NOT NULL DEFAULT 0 CHECK (kept >= 0),
    started_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    finished_at timestamptz
);
CREATE INDEX base_privacy_sweeps_class ON base_privacy_sweeps(tenant_id, class, id DESC);

CREATE FUNCTION base_privacy_sweeps_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF TG_OP = 'UPDATE' THEN
    IF OLD.finished_at IS NULL AND NEW.finished_at IS NOT NULL
       AND (NEW.id, NEW.tenant_id, NEW.class, NEW.cutoff, NEW.period_days, NEW.started_at)
           IS NOT DISTINCT FROM (OLD.id, OLD.tenant_id, OLD.class, OLD.cutoff, OLD.period_days, OLD.started_at) THEN
      RETURN NEW;
    END IF;
    RAISE EXCEPTION 'a sweep records its result once' USING ERRCODE = '42501';
  END IF;
  RAISE EXCEPTION 'base_privacy_sweeps rows are never removed' USING ERRCODE = '42501';
END;
$$;
CREATE TRIGGER base_privacy_sweeps_guard BEFORE UPDATE OR DELETE ON base_privacy_sweeps
    FOR EACH ROW EXECUTE FUNCTION base_privacy_sweeps_guard();
CREATE TRIGGER base_privacy_sweeps_truncate_guard BEFORE TRUNCATE ON base_privacy_sweeps
    FOR EACH STATEMENT EXECUTE FUNCTION base_privacy_sweeps_guard();
