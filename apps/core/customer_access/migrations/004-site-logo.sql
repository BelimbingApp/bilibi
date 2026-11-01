-- Issue 137, behaviour 10: the portal's logo, a public image stored through
-- Media. Media's tables belong to another module, so there is no foreign key;
-- the facade checks the asset is the tenant's own image when it is set.
ALTER TABLE customer_portal_sites ADD COLUMN logo_asset_id bigint;
