-- A scheduled report keeps the workplace where it was subscribed.
ALTER TABLE base_report_subscriptions
 ADD COLUMN company_id bigint NOT NULL CHECK (company_id > 0);
