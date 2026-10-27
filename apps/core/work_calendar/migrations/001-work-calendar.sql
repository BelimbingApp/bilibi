-- Work calendars (issue 135, ADR 09): when a tenant does business. Each date
-- resolves to one of four day types (working, rest, off, holiday) from the
-- calendar's exceptions, then the holiday sets it includes, then the weekly
-- rule version in force; `src/resolve.rs` is the one definition. Opening
-- periods are wall-clock minutes in the rule version's zone, kept as
-- `[[start, end], ...]` and validated by the module.

CREATE TABLE core_work_calendars (
 id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
 tenant_id bigint NOT NULL REFERENCES tenants(id),
 code text NOT NULL CHECK (code ~ '^[a-z0-9][a-z0-9_-]{0,63}$'),
 name text NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 120),
 status text NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'retired')),
 -- Where a holiday on a weekly rest (or off) day moves: nowhere, or the next working day.
 substitute text NOT NULL DEFAULT 'none' CHECK (substitute IN ('none', 'rest', 'rest_and_off')),
 version bigint NOT NULL DEFAULT 1 CHECK (version >= 1),
 created_at timestamptz NOT NULL DEFAULT now(),
 updated_at timestamptz NOT NULL DEFAULT now(),
 UNIQUE (tenant_id, code),
 UNIQUE (tenant_id, id)
);

-- The week from `effective_from` until the next version: its zone and, for
-- Monday to Sunday, a day type and up to four opening periods.
CREATE TABLE core_work_calendar_rules (
 id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
 tenant_id bigint NOT NULL,
 calendar_id bigint NOT NULL,
 effective_from date NOT NULL,
 timezone text NOT NULL CHECK (length(timezone) BETWEEN 1 AND 64),
 week jsonb NOT NULL CHECK (jsonb_typeof(week) = 'array' AND jsonb_array_length(week) = 7),
 created_at timestamptz NOT NULL DEFAULT now(),
 UNIQUE (tenant_id, calendar_id, effective_from),
 FOREIGN KEY (tenant_id, calendar_id) REFERENCES core_work_calendars (tenant_id, id)
);

-- Whole-day holidays a tenant keeps, optionally for a country and division
-- (checked against core/geonames). Installation-wide sets are a later change.
CREATE TABLE core_work_calendar_holiday_sets (
 id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
 tenant_id bigint NOT NULL REFERENCES tenants(id),
 code text NOT NULL CHECK (code ~ '^[a-z0-9][a-z0-9_-]{0,63}$'),
 name text NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 120),
 country_iso text CHECK (country_iso ~ '^[A-Z]{2}$'),
 admin1_code text CHECK (length(admin1_code) BETWEEN 1 AND 20),
 version bigint NOT NULL DEFAULT 1 CHECK (version >= 1),
 created_at timestamptz NOT NULL DEFAULT now(),
 updated_at timestamptz NOT NULL DEFAULT now(),
 UNIQUE (tenant_id, code),
 UNIQUE (tenant_id, id),
 CHECK (admin1_code IS NULL OR country_iso IS NOT NULL)
);
CREATE TABLE core_work_calendar_holidays (
 id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
 tenant_id bigint NOT NULL,
 holiday_set_id bigint NOT NULL,
 date date NOT NULL,
 name text NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 120),
 UNIQUE (tenant_id, holiday_set_id, date),
 FOREIGN KEY (tenant_id, holiday_set_id) REFERENCES core_work_calendar_holiday_sets (tenant_id, id)
);
CREATE INDEX core_work_calendar_holidays_date_idx ON core_work_calendar_holidays (tenant_id, date);

CREATE TABLE core_work_calendar_includes (
 tenant_id bigint NOT NULL,
 calendar_id bigint NOT NULL,
 holiday_set_id bigint NOT NULL,
 PRIMARY KEY (tenant_id, calendar_id, holiday_set_id),
 FOREIGN KEY (tenant_id, calendar_id) REFERENCES core_work_calendars (tenant_id, id),
 FOREIGN KEY (tenant_id, holiday_set_id) REFERENCES core_work_calendar_holiday_sets (tenant_id, id)
);
CREATE INDEX core_work_calendar_includes_set_idx ON core_work_calendar_includes (tenant_id, holiday_set_id);

-- A date or range (at most 366 days) with its own day type: a shutdown, a
-- special working day, a half day. Ranges of one calendar never overlap; the
-- module checks that under the calendar's row lock.
CREATE TABLE core_work_calendar_exceptions (
 id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
 tenant_id bigint NOT NULL,
 calendar_id bigint NOT NULL,
 starts_on date NOT NULL,
 ends_on date NOT NULL,
 name text NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 120),
 day_type text NOT NULL CHECK (day_type IN ('working', 'rest', 'off', 'holiday')),
 periods jsonb NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(periods) = 'array'),
 created_at timestamptz NOT NULL DEFAULT now(),
 CHECK (ends_on >= starts_on AND ends_on - starts_on < 366),
 CHECK ((day_type = 'working') = (jsonb_array_length(periods) > 0)),
 FOREIGN KEY (tenant_id, calendar_id) REFERENCES core_work_calendars (tenant_id, id)
);
CREATE INDEX core_work_calendar_exceptions_range_idx ON core_work_calendar_exceptions (tenant_id, calendar_id, starts_on);
