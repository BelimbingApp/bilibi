-- The pairing key: what an instance presents when it downloads releases
-- from its source, and what the source compares it with. One per server
-- (the platform operator's tenant), sealed with the installation's settings
-- key (Settings::seal_owned), so it is never readable in the database and
-- never shown back. apps/base/software/README.md, "Pairing".
CREATE TABLE base_software_pairing (
 tenant_id bigint PRIMARY KEY,
 sealed jsonb NOT NULL,
 set_by bigint NOT NULL,
 set_at timestamptz NOT NULL DEFAULT now()
);
