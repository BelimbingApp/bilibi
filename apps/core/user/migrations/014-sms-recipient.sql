-- The number each text-message code was sent to. A code counts, and the
-- first correct one verifies the account's number, only while the account
-- still has this number; a change of number in the moment a code is being
-- sent can therefore not verify the new one with a code the old one got.
-- Codes outstanding when this runs name no number: they are voided.
ALTER TABLE user_two_factor_sms_codes ADD COLUMN sent_to text;
UPDATE user_two_factor_sms_codes SET voided_at=now() WHERE sent_to IS NULL AND used_at IS NULL AND voided_at IS NULL;
