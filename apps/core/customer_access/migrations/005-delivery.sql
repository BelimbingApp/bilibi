-- Issue 137, behaviour 13: whether an invitation's email arrived. A relay that
-- refuses the message marks it `undelivered`; a bounce a mail adapter reports
-- later marks it `bounced`. Either way the invitation stays open (it can be
-- revoked, or replaced by inviting the same or another address), and the
-- inviter is notified once.
ALTER TABLE customer_invitations
 ADD COLUMN delivery text CHECK (delivery IN ('undelivered','bounced')),
 ADD COLUMN delivery_at timestamptz,
 ADD CONSTRAINT customer_invitations_delivery_pair CHECK ((delivery IS NULL) = (delivery_at IS NULL));
