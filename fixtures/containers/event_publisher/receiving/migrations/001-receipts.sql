CREATE TABLE event_publisher_receipts(
  id bigserial PRIMARY KEY,
  tenant_id bigint NOT NULL,
  receipt_number text NOT NULL,
  supplier text NOT NULL,
  quantity numeric(20,4) NOT NULL,
  returned boolean NOT NULL DEFAULT false,
  UNIQUE (tenant_id, receipt_number)
);
