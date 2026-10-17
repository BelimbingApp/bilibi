-- Payables' own copy of what it needs from received goods, keyed by the
-- publisher's event key, plus the suppliers it has onboarded.
CREATE TABLE event_subscriber_suppliers(
  tenant_id bigint NOT NULL,
  code text NOT NULL,
  PRIMARY KEY (tenant_id, code)
);
CREATE TABLE event_subscriber_receipts(
  tenant_id bigint NOT NULL,
  receipt_number text NOT NULL,
  supplier text NOT NULL,
  quantity numeric(20,4) NOT NULL,
  received_event text NOT NULL UNIQUE,
  returned_event text UNIQUE,
  PRIMARY KEY (tenant_id, receipt_number)
);
