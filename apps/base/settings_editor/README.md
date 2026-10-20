# Settings editor (P3)

`/system/settings` is the operator page for contributed settings (Bilimbi `Settings.Web.GroupLive` + `Settings.Form` + the host's `SecretReveal`). It is its own module because it needs Authz, and Authz depends on `base/settings`: storage stays below Authz, the screen sits above it.

## Table

Settings appear in the shared `FlexTable`, with Setting, Value and Area columns. The company picker uses the same table; per-person settings do not use this module.

- Search matches a setting's name, key, description and current non-secret value as you type. Secret values are never searched or decrypted for the table.
- Click a heading to sort; click again to reverse. `ListState` keeps search, sort, company and pagination in the URL.
- Click a Value cell to open only its editor. Save or Enter saves; Cancel or Escape discards the draft. A successful save returns to reading. Validation and 2FA refusals stay beside the value.
- Protected values use the shared `StepUp` form, with setup guidance before enrollment and an inline authenticator code afterwards.

## Fields

A field is every `base/settings.definitions` contribution with `editor` metadata, a concrete key (no wildcard) and the global scope, shown as rows in the table. Declaring such a setting is all a module does to appear here. A field whose `editor.capability` the actor does not hold right now is withheld: it is not rendered and a forged submission naming it is ignored. When every field is withheld the page names the missing capabilities instead of claiming nothing was contributed.

Values are read with `Settings::resolve_masked`, so an encrypted value is never decrypted to draw the page; a stored secret shows the keep-current mask. Without a valid `BILIBI_SETTINGS_KEY` an encrypted field is read-only (it can still be cleared by Restore Defaults, which needs no key).

## Writes

Route access is `OperatorCapability(base.settings.global.manage)`. Every Save and Restore re-asks `authorize_operator` (logged) and rebuilds the field plan from live grants before acting; a refusal flashes and redraws, it never clears the page. A Save is planned in full, then applied in one `MutationTransaction` (shared mutation audit): a blank clears this scope's override, the mask keeps a secret, and a value equal to what the field already shows is not written. That last rule is a deliberate difference from Bilimbi, whose Save pins every submitted value, including inherited defaults and every checkbox, as an override. Messages say what happened ("1 setting updated, 1 override cleared."). Restore confirms through the shared dialog, which counts every changed setting in the current scope, including rows hidden by search or on other pages, and deletes only those that are still overridden. Changing the scope to Global from the URL or Back drops a pending restore.

A setting that a Base Step-up operation guards (`Operation::guards_setting`, declared `.always()`; today the Software pairing key, `base.software.pairing_key.change`) asks for the holder's authenticator code every time before Save or Restore touches it. The row asks for a code and keeps a refused save open; while viewing as someone else it says the setting cannot be changed instead. Restore uses the host confirmation page. Other fields ask for nothing.

## Stored-secret reveal

"Show stored value" appears only for an actor holding `base.settings.secret.view` by name (`authz::explicitly_granted`; `grant_all` does not count) when the key is configured. Showing it is the Base Step-up operation `base.settings_editor.secret.reveal` ("Show a stored secret setting"), on the installation floor; it replaced the password prompt. When the session owes a check, the Value cell asks for the code inline and the person submits again; the authenticator locks repeated failures. Every attempt, including a forged request, records a retained `settings.secret.reveal` audit action without the value, and a reveal whose audit row cannot be written shows nothing. The value reaches the browser only as a `LiveBody.events` push to the `SecretStored` hook (`bilibi:secret:reveal`), which re-masks it after the definition's `reveal_duration_ms`.

## Company scope (K15)

Accounts with `base.settings.company.manage` see the scope picker: Global plus the companies Tenancy's organisation directory (`PartyProofs::selectable_organisations`, bound by Core Company) says this actor may manage with that capability, their own company or every company of the tenant with `admin.company.tenant-wide.manage`. Others see Bilimbi's "You do not have permission to manage company settings."; an actor the directory names no company for is told no company is available.

A picked company is an id, never a proof: `switch_scope` reproves it (`organisation_target`) and every Save and Clear reproves it again plus the company's standing (`PartyProofs::organisation`). A denied, missing or cross-tenant company is refused with "You do not have permission to manage this company's settings."; an archived one with "This company is archived and read-only, so its settings were not changed." and is never offered by the picker, a deliberate narrowing of Bilimbi, which opens an archived company read-only. Under a company the page lists the definitions with the company scope (Datetime's `localization.timezone`, gated by `admin.company.update`), marks overrides "Set here", notes "Inherited from global/tenant/the default", writes under `Target::Company` as `settings.company.save`/`restore` and labels the restore button "Clear overrides". The K15 walkthrough (`apps/core/company/examples/k15-owner-binders.rs`) exercises the same seam.

## Manual walkthrough

On a disposable migrated database, install the walkthrough fixture (`cargo run -p bilibi-walkthrough-fixture`; tenant 1 is its operator), set `BILIBI_SETTINGS_KEY` to 64 hex characters, run the app and sign in as a tenant 1 account with `base.settings.global.manage` (an admin role with `grant_all`). Then:

1. Open **Administration → System → Settings**. Each Value cell says "Set here" or names the inherited source.
2. Open a number's Value cell and Save: one setting updated. Blank it and Save: one override cleared. Type `abc` into a number: "<Label>: must be a whole number", nothing stored.
3. Save two rows, press Restore Defaults: the dialog counts two changed settings; Restore clears them.
4. Grant the account `base.settings.secret.view` directly and store an encrypted editable setting (the walkthrough in `apps/web/examples` or any module's encrypted definition). "Show stored value" asks for 2FA; a refused check shows nothing, an accepted check shows the value briefly, and `base_audit_actions` holds one `settings.secret.reveal` row per attempt.
5. Deny one field's capability (`principal_capabilities ... is_allowed=false`) and reload: that row is gone; deny all and the page names them.

A setting protected by the current 2FA policy uses the shared `bilibi_ui::StepUp` form. Before setup its control stays disabled beside a setup link; other settings can still be saved. Restore is replaced by the setup link when it would clear a protected override. Every write keeps its existing authorization and 2FA checks.
