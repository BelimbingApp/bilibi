-- The company the session chose to work in (the shell's company switcher),
-- or NULL for the account's own company through its affiliation. A choice,
-- not a grant: Tenancy keeps it on the scope only while the account still
-- reaches that company (`Scope::current_company`), and the session edge clears a
-- choice the account no longer reaches. Core Company adds the scoped key.
-- `held_company_id` keeps an operator's own choice while the session views as
-- another account, so leaving that view restores it.
ALTER TABLE sessions ADD COLUMN company_id bigint;
ALTER TABLE sessions ADD COLUMN held_company_id bigint;
