# Menu contract

`base/menu` owns the navigation tree; it has no tables and does no IO. An owning
module contributes items with `bilibi_menu::contribute!("core/x", MenuItem::page(...))`
(see `apps/core/company/src/contributions.rs`) and adds `base/menu` to its
descriptor dependencies. Do not keep a host-side link list or hard-code
navigation in a template.

- `Menu::from_catalog(services.catalog()?)` is built once at boot (`App::menu`).
  Duplicate IDs, self-parents and parent cycles fail boot as contributor defects.
  Icon names use the bare names documented in `../ui/README.md`. Web checks every
  contribution at boot; an unknown name warns with its module and renders the
  shared missing-icon glyph, without blocking boot.
  A missing parent drops that item and its descendants, listed by `dropped()` and
  logged once, so one bad contributor cannot remove everyone's navigation.
- Render `visible_tree(&decide)`, never `tree()` (diagnostics). `decide` answers
  one capability; Menu does not depend on Authz. Decide every key in
  `capability_keys()` in one authorization pass for the actor, then filter. A
  container with no visible child is hidden, so unported sections never render.
- `Requirement::AnyOf` opens a combined page only. Hiding a link is presentation:
  the route and each operation must enforce their own capability.
- `base/menu` contributes the structural roots (`admin`, `admin.system.*`, and the
  Domain roots). Attach to them; do not redefine them.
- `portal` (`PORTAL_ROOT`) is the customer portal's menu ([ADR 16](../../../docs/architecture/decisions/16-customer-accounts-and-portal.md)). Items under it route only to `/portal` pages and ask for a portal capability; boot refuses one that routes elsewhere or asks for nothing, a staff item routing into `/portal`, and (Authz) a key of the wrong audience. `visible_tree` never includes it and `visible_portal_tree` returns only it, for the portal layout.

`apps/web/examples/p1-seams.rs` shows cycles, duplicates, dropped parents and
visibility against a disposable database.

## Workplace coverage

`MenuItem::page` defaults to tenant-wide coverage for administration and shared directories. Add `.company_scoped()` when its route declares `CompanyScope::Company`; Web checks that they agree at boot. A company page uses `Scope::current_company()` for its records and keeps its owner authorization. The shell retains the chosen desk on tenant-wide pages and marks them **All workplaces**.
