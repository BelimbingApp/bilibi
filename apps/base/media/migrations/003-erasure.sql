-- Attachment erasure (ADR 05): a submitted attachment of an erased person
-- becomes a tombstone. The row keeps who uploaded what for which record and
-- when; the asset row goes in the same transaction and the bytes are removed
-- after commit (`erased_disk`/`erased_key` name them until then). Only a
-- privacy erasure transaction may erase, and an erased row never changes again.
ALTER TABLE base_media_attachments
    DROP CONSTRAINT base_media_attachments_state_check,
    -- 002's unnamed "submitted iff submitted_at" rule; an erased row keeps the
    -- submitted_at it had, so the rule is restated per state below.
    DROP CONSTRAINT base_media_attachments_check,
    ALTER COLUMN media_asset_id DROP NOT NULL,
    ADD COLUMN erased_at timestamptz,
    ADD COLUMN erased_by bigint CHECK (erased_by > 0),
    ADD COLUMN erased_disk text CHECK (length(erased_disk) BETWEEN 1 AND 255),
    ADD COLUMN erased_key text CHECK (length(erased_key) BETWEEN 1 AND 1024),
    ADD COLUMN bytes_removed_at timestamptz;
ALTER TABLE base_media_attachments
    ADD CONSTRAINT base_media_attachments_state_check CHECK (state IN ('draft', 'submitted', 'erased')),
    ADD CONSTRAINT base_media_attachments_submitted_shape CHECK (
        (state <> 'submitted' OR submitted_at IS NOT NULL) AND (state <> 'draft' OR submitted_at IS NULL)),
    ADD CONSTRAINT base_media_attachments_erased_shape CHECK (
        (state = 'erased') = (erased_at IS NOT NULL)
        AND (state = 'erased') = (media_asset_id IS NULL)
        AND (state <> 'erased' OR (erased_by IS NOT NULL AND erased_disk IS NOT NULL AND erased_key IS NOT NULL))
        AND (bytes_removed_at IS NULL OR state = 'erased'));
-- The erased-bytes sweep finds pending files per tenant.
CREATE INDEX base_media_attachments_erased_pending ON base_media_attachments (tenant_id, id)
    WHERE state = 'erased' AND bytes_removed_at IS NULL;

-- Replaces 002's guard. A draft or submitted row may become erased inside a
-- privacy erasure transaction, binding unchanged; otherwise draft rules are
-- unchanged and submitted rows stay immutable. An erased row may only record
-- that its bytes were removed, once.
CREATE OR REPLACE FUNCTION base_media_attachments_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF TG_OP = 'DELETE' THEN
  IF OLD.state <> 'draft' THEN
   RAISE EXCEPTION 'submitted attachments are immutable' USING ERRCODE = 'restrict_violation';
  END IF;
  RETURN OLD;
 END IF;
 IF NEW.public_id IS DISTINCT FROM OLD.public_id
  OR NEW.tenant_id IS DISTINCT FROM OLD.tenant_id
  OR NEW.subject_type IS DISTINCT FROM OLD.subject_type
  OR NEW.subject_id IS DISTINCT FROM OLD.subject_id
  OR NEW.uploaded_by_type IS DISTINCT FROM OLD.uploaded_by_type
  OR NEW.uploaded_by_id IS DISTINCT FROM OLD.uploaded_by_id
  OR NEW.created_at IS DISTINCT FROM OLD.created_at THEN
  RAISE EXCEPTION 'attachment is immutable' USING ERRCODE = 'restrict_violation';
 END IF;
 IF OLD.state = 'erased' THEN
  IF OLD.bytes_removed_at IS NULL AND NEW.bytes_removed_at IS NOT NULL
     AND (NEW.state, NEW.media_asset_id, NEW.submitted_at, NEW.retained_at, NEW.erased_at, NEW.erased_by, NEW.erased_disk, NEW.erased_key)
         IS NOT DISTINCT FROM
         (OLD.state, OLD.media_asset_id, OLD.submitted_at, OLD.retained_at, OLD.erased_at, OLD.erased_by, OLD.erased_disk, OLD.erased_key) THEN
   RETURN NEW;
  END IF;
  RAISE EXCEPTION 'erased attachment is immutable' USING ERRCODE = 'restrict_violation';
 END IF;
 IF NEW.state = 'erased'
    AND coalesce(current_setting('bilibi.privacy_erasure', true), '') <> ''
    AND NEW.submitted_at IS NOT DISTINCT FROM OLD.submitted_at
    AND NEW.retained_at IS NOT DISTINCT FROM OLD.retained_at
    AND NEW.bytes_removed_at IS NULL THEN
  RETURN NEW;
 END IF;
 IF OLD.state = 'submitted' THEN
  RAISE EXCEPTION 'submitted attachments are immutable' USING ERRCODE = 'restrict_violation';
 END IF;
 IF NEW.media_asset_id IS DISTINCT FROM OLD.media_asset_id
  OR NEW.state = 'erased'
  OR (OLD.retained_at IS NOT NULL AND NEW.retained_at IS DISTINCT FROM OLD.retained_at) THEN
  RAISE EXCEPTION 'attachment is immutable' USING ERRCODE = 'restrict_violation';
 END IF;
 RETURN NEW;
END $$;
