ALTER TABLE base_agent_api_plans
ADD COLUMN company_id bigint CHECK(company_id > 0);
