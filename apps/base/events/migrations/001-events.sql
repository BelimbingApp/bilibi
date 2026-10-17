-- One row per recorded domain event, written in the publisher's own
-- transaction. The publisher's key is its idempotency key: recording the same
-- key again in the same tenant answers the event already recorded.
CREATE TABLE base_events (
 id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
 tenant_id bigint NOT NULL CHECK (tenant_id > 0),
 publisher text NOT NULL,
 name varchar(200) NOT NULL,
 version integer NOT NULL CHECK (version BETWEEN 1 AND 65535),
 subject varchar(200) NOT NULL CHECK (subject <> ''),
 event_key varchar(200) NOT NULL CHECK (event_key <> ''),
 payload jsonb NOT NULL CHECK (jsonb_typeof(payload) = 'object' AND octet_length(payload::text) <= 131072),
 trace_id text NOT NULL,
 recorded_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 UNIQUE (tenant_id, publisher, event_key)
);
-- Delivery walks one subject's events in recording order.
CREATE INDEX base_events_subject ON base_events (tenant_id, publisher, subject, id);
CREATE INDEX base_events_name ON base_events (tenant_id, name, id);
-- One row per (event, subscriber): created for every mounted subscriber when
-- the event is recorded, or by a replay. `delivered_at` is the receipt; it
-- commits with the subscriber's effects and audit, so a delivered row is never
-- handed to that subscriber again.
CREATE TABLE base_events_deliveries (
 event_id bigint NOT NULL REFERENCES base_events(id),
 subscriber varchar(200) NOT NULL,
 attempts integer NOT NULL DEFAULT 0 CHECK (attempts >= 0),
 last_error varchar(64) CHECK (last_error ~ '^[a-z][a-z0-9_]{0,63}$'),
 failed_at timestamptz,
 delivered_at timestamptz,
 PRIMARY KEY (event_id, subscriber)
);
CREATE INDEX base_events_pending ON base_events_deliveries (subscriber, event_id) WHERE delivered_at IS NULL;
