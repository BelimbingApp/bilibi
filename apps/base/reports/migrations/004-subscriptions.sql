-- Scheduled deliveries (BelimbingApp/bilibi#128, phase 2, ADR 06 decision 4).
-- A subscription is a stored, audited request to deliver one report on a
-- schedule to its subscriber and, optionally, to the holders of chosen
-- roles. Each delivery runs as its recipient (a Queue job whose actor Queue
-- re-proves on every attempt), so each gets the result their own access
-- allows; a recipient who has lost access gets nothing and the delivery says
-- why. Times are wall-clock in the subscription's zone.
CREATE TABLE base_report_subscriptions (
 id bigint GENERATED ALWAYS AS IDENTITY,
 tenant_id bigint NOT NULL,
 report_id bigint NOT NULL,
 owner_id bigint NOT NULL,
 frequency text NOT NULL CHECK (frequency IN ('daily','weekly','monthly')),
 at_minute smallint NOT NULL CHECK (at_minute BETWEEN 0 AND 1439),
 weekday smallint CHECK (weekday BETWEEN 1 AND 7),
 monthday smallint CHECK (monthday BETWEEN 1 AND 28),
 timezone text NOT NULL CHECK (length(timezone) BETWEEN 1 AND 64),
 format text NOT NULL CHECK (format IN ('csv','pdf')),
 include_owner boolean NOT NULL DEFAULT true,
 skip_empty boolean NOT NULL DEFAULT true,
 paused boolean NOT NULL DEFAULT false,
 next_run_at timestamptz NOT NULL,
 last_run_at timestamptz,
 version bigint NOT NULL DEFAULT 1 CHECK (version >= 1),
 created_at timestamptz NOT NULL DEFAULT now(),
 CONSTRAINT base_report_subscriptions_pkey PRIMARY KEY (id),
 CONSTRAINT base_report_subscriptions_tenant_id_key UNIQUE (tenant_id, id),
 CONSTRAINT base_report_subscriptions_report_fkey FOREIGN KEY (tenant_id, report_id) REFERENCES base_reports (tenant_id, id) ON DELETE CASCADE,
 CONSTRAINT base_report_subscriptions_day CHECK (
   (frequency = 'daily' AND weekday IS NULL AND monthday IS NULL)
   OR (frequency = 'weekly' AND weekday IS NOT NULL AND monthday IS NULL)
   OR (frequency = 'monthly' AND monthday IS NOT NULL AND weekday IS NULL))
);
CREATE INDEX base_report_subscriptions_due_idx ON base_report_subscriptions (tenant_id, next_run_at) WHERE NOT paused;
CREATE INDEX base_report_subscriptions_owner_idx ON base_report_subscriptions (tenant_id, owner_id);

-- The roles whose holders also receive a subscription's deliveries.
CREATE TABLE base_report_subscription_roles (
 tenant_id bigint NOT NULL,
 subscription_id bigint NOT NULL,
 role_id bigint NOT NULL,
 CONSTRAINT base_report_subscription_roles_pkey PRIMARY KEY (tenant_id, subscription_id, role_id),
 CONSTRAINT base_report_subscription_roles_subscription_fkey FOREIGN KEY (tenant_id, subscription_id) REFERENCES base_report_subscriptions (tenant_id, id) ON DELETE CASCADE,
 CONSTRAINT base_report_subscription_roles_role_fkey FOREIGN KEY (tenant_id, role_id) REFERENCES roles (tenant_id, id) ON DELETE CASCADE
);

-- One delivery: one recipient, one due time. `pending` waits to be queued
-- as its recipient; `queued` runs; `sent`, `skipped` (with its reason) and
-- `cancelled` are final. The unique key makes a repeated sweep harmless.
CREATE TABLE base_report_deliveries (
 id bigint GENERATED ALWAYS AS IDENTITY,
 tenant_id bigint NOT NULL,
 subscription_id bigint NOT NULL,
 recipient_id bigint NOT NULL,
 due_at timestamptz NOT NULL,
 state text NOT NULL DEFAULT 'pending' CHECK (state IN ('pending','queued','sent','skipped','cancelled')),
 reason text CHECK (length(reason) <= 500),
 export_id bigint,
 created_at timestamptz NOT NULL DEFAULT now(),
 finished_at timestamptz,
 CONSTRAINT base_report_deliveries_pkey PRIMARY KEY (id),
 CONSTRAINT base_report_deliveries_tenant_id_key UNIQUE (tenant_id, id),
 CONSTRAINT base_report_deliveries_once UNIQUE (tenant_id, subscription_id, recipient_id, due_at),
 CONSTRAINT base_report_deliveries_subscription_fkey FOREIGN KEY (tenant_id, subscription_id) REFERENCES base_report_subscriptions (tenant_id, id) ON DELETE CASCADE
);
CREATE INDEX base_report_deliveries_pending_idx ON base_report_deliveries (tenant_id, created_at) WHERE state = 'pending';
