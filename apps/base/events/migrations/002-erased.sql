-- When a personal-data erasure changed an event's payload (ADR 05). A replay
-- creates no delivery for an erased event: its payload no longer holds the
-- facts a subscriber reads.
ALTER TABLE base_events ADD COLUMN erased_at timestamptz;
