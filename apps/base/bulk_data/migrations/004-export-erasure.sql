-- ID-only cleanup authority for cached exports invalidated by an owner's
-- erasure. Keep the artifact reference until Artifacts has removed its bytes.
ALTER TABLE base_bulk_data_exports ADD COLUMN erased_for bigint[] NOT NULL DEFAULT '{}';
