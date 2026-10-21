# Menu contract

`base/menu` owns the navigation tree; it has no tables and does no IO. An owning
module contributes items with `bilibi_menu::contribute!("core/x", MenuItem::page(...))`
(see `apps/core/company/src/contributions.rs`) and adds `base/menu` to its
descriptor dependencies. Do not keep a host-side link list or hard-code
navigation in a template.

- `Menu::from_catalog(services.catalog()?)` is built once at boot (`App::menu`).
  Duplicate IDs, self-parents and parent cycles fail boot as contributor defects.
  A missing parent drops that item and its descendants, listed by `dropped()` and
  logged once, so one bad contributor cannot remove everyone's navigation.
- Render `visible_tree(&decide)`, never `tree()` (diagnostics). `decide` answers
  one capability; Menu does not depend on Authz. Decide every key in
  `capability_keys()` in one authorization pass for the actor, then filter. A
  container with no visible child is hidden, so unported sections never render.
- `.leading()` draws a root page first, in bold, with a rule before the
  sections (Dashboard). A leading item that is not a root page fails boot.
- `Requirement::AnyOf` opens a combined page only. Hiding a link is presentation:
  the route and each operation must enforce their own capability.
- `base/menu` contributes the structural roots (`admin`, `admin.system.*`, and the
  Domain roots). Attach to them; do not redefine them.

`apps/web/examples/p1-seams.rs` shows cycles, duplicates, dropped parents and
visibility against a disposable database.
