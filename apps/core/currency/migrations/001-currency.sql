-- Currencies and exchange rates (`core/currency`). `currencies` is the ISO
-- 4217 list, global and read-only to tenants: they enable and disable codes,
-- never add one or change its minor units. A code is withdrawn, never deleted,
-- so a record that holds it still reads. `currency_rates` holds one dated rate
-- per tenant and pair: 1 `from_code` = `rate` `to_code` from `effective_on`
-- until a later row.
CREATE TABLE currencies (
 code text PRIMARY KEY CHECK (code ~ '^[A-Z]{3}$'),
 numeric_code text NOT NULL CHECK (numeric_code ~ '^[0-9]{3}$'),
 name text NOT NULL,
 minor_units smallint NOT NULL CHECK (minor_units BETWEEN 0 AND 4),
 withdrawn boolean NOT NULL DEFAULT false
);
CREATE TABLE currency_tenant_enabled (
 id bigint GENERATED ALWAYS AS IDENTITY UNIQUE,
 tenant_id bigint NOT NULL REFERENCES tenants(id),
 code text NOT NULL REFERENCES currencies(code),
 enabled_at timestamptz NOT NULL DEFAULT now(),
 PRIMARY KEY (tenant_id, code)
);
CREATE TABLE currency_rates (
 id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
 tenant_id bigint NOT NULL REFERENCES tenants(id),
 from_code text NOT NULL REFERENCES currencies(code),
 to_code text NOT NULL REFERENCES currencies(code),
 effective_on date NOT NULL,
 rate numeric(30,12) NOT NULL CHECK (rate > 0),
 source_note text,
 version bigint NOT NULL DEFAULT 1,
 created_at timestamptz NOT NULL DEFAULT now(),
 updated_at timestamptz NOT NULL DEFAULT now(),
 CONSTRAINT currency_rates_pair_check CHECK (from_code <> to_code),
 CONSTRAINT currency_rates_key UNIQUE (tenant_id, from_code, to_code, effective_on)
);
