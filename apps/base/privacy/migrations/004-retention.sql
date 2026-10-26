-- Retention sweeps (ADR 05): one append-only summary row per run of one
-- record class in one tenant: the company whose settings it followed, the
-- period and cutoff it used, and, for the whole run, how many records it
-- anonymised or purged and how many legal holds kept. The deferred audit
-- erasure (evidence kept for its legal minimum) writes a row of its own, with
-- no period. A sweep records its result once, when the run ends; a row never
-- finished marks a run that stopped. No row is removed. Company periods and
-- legal minimums are settings (`privacy.retention.<class>.*`).
CREATE TABLE base_privacy_sweeps (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    tenant_id bigint NOT NULL REFERENCES tenants(id) ON DELETE RESTRICT,
    class text NOT NULL CHECK (length(class) BETWEEN 1 AND 255),
    company_id bigint CHECK (company_id > 0),
    cutoff timestamptz,
    period_days integer CHECK (period_days > 0),
    acted bigint NOT NULL DEFAULT 0 CHECK (acted >= 0),
    kept bigint NOT NULL DEFAULT 0 CHECK (kept >= 0),
    batches integer NOT NULL DEFAULT 0 CHECK (batches >= 0),
    started_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    finished_at timestamptz,
    CHECK ((cutoff IS NULL) = (period_days IS NULL))
);
CREATE INDEX base_privacy_sweeps_class ON base_privacy_sweeps(tenant_id, class, id DESC);

CREATE FUNCTION base_privacy_sweeps_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF TG_OP = 'UPDATE' THEN
    IF OLD.finished_at IS NULL AND NEW.finished_at IS NOT NULL
       AND (NEW.id, NEW.tenant_id, NEW.class, NEW.company_id, NEW.cutoff, NEW.period_days, NEW.started_at)
           IS NOT DISTINCT FROM (OLD.id, OLD.tenant_id, OLD.class, OLD.company_id, OLD.cutoff, OLD.period_days, OLD.started_at) THEN
      NEW.finished_at := clock_timestamp();
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
