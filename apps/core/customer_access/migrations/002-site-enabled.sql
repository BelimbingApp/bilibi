-- Issue 137, behaviour 1: a tenant's portal is off until staff turn it on.
-- While off, its anonymous pages answer 404, its customers' portal pages
-- answer 404 and no invitation is sent or redeemed.
ALTER TABLE customer_portal_sites ADD COLUMN enabled boolean NOT NULL DEFAULT false;
