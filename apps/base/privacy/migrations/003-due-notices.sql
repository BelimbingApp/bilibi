-- Due notices (ADR 05): when the people handling an open request were told
-- its due date is near. A request is notified once.
ALTER TABLE base_privacy_requests ADD COLUMN due_notified_at timestamptz;
CREATE INDEX base_privacy_requests_open_due ON base_privacy_requests(tenant_id, due_at)
    WHERE status NOT IN ('completed', 'refused') AND due_notified_at IS NULL;
