-- The company the session chose to work in (the shell's company switcher),
-- or NULL for the account's own company through its affiliation. A choice,
-- not a grant: Tenancy keeps it on the scope only while the account still
-- reaches that company (`Scope::current_company`), and the session edge clears a
-- choice the account no longer reaches. Core Company adds the scoped key.
ALTER TABLE sessions ADD COLUMN company_id bigint;
