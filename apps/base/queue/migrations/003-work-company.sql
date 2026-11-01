-- Historical work context, not an authority or a Core foreign key.
ALTER TABLE base_queue_jobs ADD COLUMN work_company_id bigint CHECK(work_company_id > 0);
