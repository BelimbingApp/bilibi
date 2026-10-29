-- Erasure approval through Workflow's shared approval case (ADR 05, ADR 15;
-- owner decision of 2026-10-29). Privacy keeps what only it knows: the
-- operator who impersonated whoever received or verified a request (both are
-- excluded from approving it), and the request's latest approval round.
ALTER TABLE base_privacy_requests
    ADD COLUMN received_impersonator bigint CHECK (received_impersonator > 0),
    ADD COLUMN verified_impersonator bigint CHECK (verified_impersonator > 0),
    -- The latest Workflow approval request of an erasure, the prior of the
    -- next round once it is recalled; awaiting while that round is open.
    ADD COLUMN approval_request bigint CHECK (approval_request > 0),
    ADD COLUMN approval_awaiting boolean NOT NULL DEFAULT false,
    ADD CHECK (NOT approval_awaiting OR (kind = 'erasure' AND status = 'verified' AND approval_request IS NOT NULL));
