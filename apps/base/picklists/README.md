# Base Picklists

Value lists a module declares and each tenant curates: case categories, industries, loss reasons, lead sources. A record stores a value's stable code, so renaming or retiring the value never breaks it. No Bilimbi counterpart; the rules generalise `core/company` relationship roles.

- **The module builds no table, seed logic or admin page for a list.** It declares the list and calls the facade.
- **Values a module's logic branches on stay in code** (a Rust enum or a `base/workflow` status). See [ADR 07](../../../docs/architecture/decisions/07-hard-coded-values-and-operator-pick-lists.md).
- **This module reads no owner table, and owners never read its table.**

## Offer a pick-list

Three steps in the owning module, nothing anywhere else.

1. Declare it (discovered like capabilities and menu items, validated at boot):

   ```rust
   use bilibi_picklists::{PickList, Start};
   bilibi_picklists::declare!(
       "domain/cases",
       PickList::new("domain.cases.category", "Case category", &[
           Start::new("billing", "Billing"),
           Start::new("technical", "Technical"),
           Start::new("other", "Other").hint("Asks for a note").locked(),
       ])
       .default("technical")
   );
   ```

   - Add `base/picklists` to the module's descriptor `dependencies` and `bilibi-picklists.workspace = true` to its `Cargo.toml`.
   - Starting values appear in the declared order. Each tenant can change them from there.

2. Contribute its starting-values seed (boot refuses a module that declares lists without one):

   ```rust
   inventory::submit! { Contribution::new(SEEDS, "domain/cases.picklists", provenance!(), &Seed {
       version: "domain-cases-picklists-v1", mode: SeedMode::Production,
       action: SeedAction::Service { service: bilibi_picklists::SEEDS_SERVICE, operation: "reconcile" },
   }) }
   ```

   - A later release that adds lists or values contributes the next version (`…-v2`). The ledger runs each version once.

3. Use it through the facade (`Arc<PickLists>` from `bilibi_picklists::SERVICE`):

   | Need | Call |
   | --- | --- |
   | A form's options, in the reader's language | `choices(client, scope, list, parent)` → `Vec<bilibi_ui::Choice>` |
   | Whether a preview may offer a code on its existing `ReadClient` | `contains_choice(client, scope, list, code, parent)`; no labels or locks, with the same active and parent filters as `choices` |
   | The value that pre-fills a new record | `default(client, scope, list)` |
   | How a record shows its stored code | `label(client, scope, list, code)` → `Label { text, retired }` |
   | Accept a newly chosen code | `check(&tx, scope, list, code, parent)` on the owner's `MutationTransaction`; `check_in(sql, …)` is available inside a scoped SQL mutation closure when the old value is read there |

   - `check` locks the value `FOR SHARE`, so a concurrent retirement waits for the owner's commit. A refusal (`Unknown`, `Retired`, `Parent`) is the owner's field error: `refusal.message()`.
   - Call `check` only when the code changes. A record keeps a retired code it already holds and saves unchanged.
   - Store the code. Never store a label or a row id.

## Declaration rules

Boot refuses each of these, naming the list and its source line:

| Refused | Rule |
| --- | --- |
| Key | Lower case, prefixed by the owning module id with `/` as `.`, at most 128 characters, unique. |
| Code | 1–64 characters of lower-case letters, digits, `_`, `-` and `.`; unique in the list. |
| Size | At most 500 values. |
| Default | Must be a starting value. |
| Parent list | Must be declared, not form a cycle, have the same reach, and belong to the module itself or a declared dependency. |
| Parent codes | A dependent list's values each name at least one starting value of the parent list; other lists' values name none. |
| Capability | `.requires(key)` must be in the Authz vocabulary (checked by the service factory). |
| Seed | The declaring module contributes a production seed running `base/picklists.seeds` `reconcile` (checked by the service factory). |

Other options: `.installation()` (one list for the installation, see below) and `.under(parent_list)` (a dependent list).

## Behaviour

- **Stable codes.** A code never changes. Rename, translate, reorder and re-parent keep it.
- **Retire, not delete.** A retired value leaves `choices` and `check` refuses it. `label` still shows it, marked retired. Restore brings it back. Values are never deleted, because this module cannot see which records hold them.
- **Locked values.** A value declared `.locked()` is one module code names (such as `other`, which asks for a note). The operator may relabel, translate and reorder it, but not retire it. Module code may name only locked codes.
- **Labels per language.** A default label plus optional labels for languages in `base/locale`'s catalogue. Display follows the reader's resolved language, then the default label.
- **Dependent lists.** A child value names the parent codes it belongs to. `choices` with no parent returns nothing, and with a parent only its values. `check` refuses a mismatched pair. The form clears the child when the parent changes.
- **No cache.** Lists are capped at 500 values and read per request.

## Starting values

Starting values go in by code: a missing code is inserted, an existing one is left exactly as the operator keeps it. The declared default becomes the default only when it is inserted and the list has none. Nothing seeds on boot.

| When | How |
| --- | --- |
| Existing tenants | The declaring module's production seed (service `base/picklists.seeds`, operation `reconcile`) runs over every live tenant when `seed-production` first sees its version. |
| A new tenant | `core/company` provisioning calls `bilibi_picklists::provision`, which returns one `Change` per inserted value: tenant administration records each on its `MutationTransaction`, attributed to the person or agent; the deployment helpers (`tenant-provision`, the platform operator) use `provision_deployment`, which lists every value in one retained system action, as their company rows are recorded. |
| A module adds values later | It contributes the next seed version. |

- Each seed inserts the missing codes of every linked list, so running one module's seed never harms another's lists.
- Each list is reconciled under its list lock and within the 500-value cap: a run that would pass the cap is refused whole and changes nothing, naming the tenant, the list and the codes it could not add. Values are never deleted and retired ones count, so a full list cannot take new starting values: the release that declares them drops those codes or reuses existing ones, then `seed-production` runs again.
- `base/picklists` contributes no seed of its own: it declares no list.

## Curating values

Each write runs on the caller's `MutationTransaction` and records one `Change` of kind `picklist_value` per changed row.

| Write | Rule |
| --- | --- |
| `create` | New code, label, translations, hint, parent codes. Takes the next position. |
| `update` | Label, translations, hint, parent codes. Version-checked. |
| `reorder` | The full list of codes, retired ones included, in the new order. Only moved values change. |
| `set_default` | Any active value, or `None` to clear. |
| `retire` / `restore` | Version-checked. A locked value cannot be retired; a retired default stops being the default. |

- A refusal is `Ok(Err(Vec<FieldError>))`. A stale version is a refusal on `version`.
- Reading every value (`values`) needs `base.picklists.view`. **Changing a list** follows one rule (`values::required`), which the facade enforces in the write's transaction and the page reads to show its editors: a tenant list needs `base.picklists.manage`; an installation-wide list needs `base.picklists.installation.manage` in the platform-operator tenant; a list's `.requires(key)` capability applies to both.
- **Installation-wide lists** keep their rows on the platform-operator tenant. Every tenant reads them; only the platform operator writes them, with `base.picklists.installation.manage`.

## Operator page

**Administration › Pick-lists** (`/admin/picklists`, `base.picklists.view`) lists the mounted lists the person may see; `/admin/picklists/{key}` shows one list's values (`src/web.rs`).

- Label, translation (pick the language in the toolbar), hint and parent codes are edited in place. Move up and down, make or clear the default, retire and restore sit on each row; retire and restore confirm first.
- Every change goes through the `base.picklists.value.*` contracts below (`PAGE_WRITES` names each event), so the page and agents share one check and one audit trail.
- Editors and the "+ Value" button show only for a person who may change the list. Retired values show with "Include retired".

## Agent operations

Agents curate through `base.picklists.*` (`src/operations.rs`). Each write is a reviewed safe write: the preview runs the facade's checks without locking or writing, and the commit writes on the plan's audited transaction.

| Key | Capability | Does |
| --- | --- | --- |
| `base.picklists.lists` | `base.picklists.view` | The mounted lists the caller may see: key, label, reach, parent list, extra capability, locked codes. |
| `base.picklists.values` | `base.picklists.view` | One list's values in order; retired ones with `include_retired`. |
| `base.picklists.value.create` | `base.picklists.view`, then by reach | Add a value. |
| `base.picklists.value.update` | `base.picklists.view`, then by reach | Rename, translate, re-hint or re-parent; needs `version`. |
| `base.picklists.value.reorder` | `base.picklists.view`, then by reach | Name every code once, in the new order. |
| `base.picklists.value.default` | `base.picklists.view`, then by reach | Set the default, or omit `code` to clear it. |
| `base.picklists.value.retire` / `.restore` | `base.picklists.view`, then by reach | Retire or restore; needs `version`. |

- The registry checks one fixed capability per operation, so the writes open at `base.picklists.view` and the facade decides by reach (the rule above). An operator who holds only `base.picklists.installation.manage` curates the installation-wide lists, through the page and the contracts alike.
- Translations (`labels`) accept one property per language `base/locale` supports; any other field is refused.
- The guide `base.picklists` explains codes, retirement, locked values and dependent lists to agents.

## Storage

`base_picklist_values` (migration `base-picklists-001-values`): one row per tenant, list and code, with the default label, translations (`labels`, keyed by language), hint, position, default flag, parent codes, `retired_on` and `version`. Whether a value is locked comes from its declaration. Every statement binds the tenant.

## Runtime check

`apps/web/examples/picklists-reference/` declares a category list with a locked `other` value, a dependent sub-category list and a second module's region list. Each declaring module contributes its seed. Run it on a fresh disposable database:

```sh
DATABASE_URL=… cargo run -p bilibi-web --example picklists-reference
```

| Case | Expected observation |
| --- | --- |
| Boot refusals | Each invalid declaration is refused with its source line: key, duplicate code, code shape, default, parent code, unknown parent, cycle, parent outside the dependencies, capability, missing seed. |
| Seed | Both modules' seed versions apply; tenant 1 and 2 get the starting values in order, with the default. |
| Curate | Rename and translate, create, reorder, set the default and retire save; a duplicate code, a child with no parent, a locked retirement and a stale version are refused. |
| Capability | A person without `base.picklists.manage` gets `Access denied`. |
| Repeat | A second seed run is already applied, and a reconcile inserts nothing; the edits stay. |
| Isolation | Tenant 2 keeps the starting values. |
| Check | Retired, unknown and mismatched pairs are refused; a retired value still shows its label, marked retired; restore accepts it again. |
| Provisioning | A new tenant has every reference value; a tenant created through `Tenants::create_in` and `provision_new_tenant_in` has one `created` mutation per value, by the admin. |
| Review cases | Two default changes at once: the second waits and saves, one default left. A tenant holding 500 values with four starting codes missing is refused with the list's name and keeps 500. Staff (only `installation.manage`) add to Region and are refused on a tenant list, by the facade and the contract. Codes `a.b`, `a_b`, `a-b` get distinct element ids; after editing `a_b` focus stays on it. |
| Audit | One `picklist_value` row per change, attributed to the actor: `created` for a new value, `updated` otherwise. |
| Agent reads | `base.picklists.lists` shows each list's locked codes; `base.picklists.values` lists the codes in order. |
| Agent writes | A create's preview is valid and writes no row; its commit saves one; replaying the plan code is refused. |
| Operator page | Serve the fixture with `APP_ORIGIN` and `BIND_ADDR`, sign in as `admin@example.test`: add, rename, translate, reorder, set the default, retire and restore on `/admin/picklists`; `viewer@example.test` gets 403 and no menu entry. |
| Case form | `/reference/cases` (`cases.rs`) as `manager@example.test`, who reads in Malay: the category options show the Malay labels; choosing a category narrows the sub-categories, changing it clears the chosen one; after retiring `feedback`, case #1 still shows it marked retired and saves unchanged, a new case cannot choose it; a forged retired value and a forged mismatched pair are refused on their fields. |
| Installation-wide list | Region is kept on the platform operator: `admin@example.test` edits it; `manager@other.example.test` (tenant 2) sees the operator's labels read-only, picks from them on the case form, and a forged write is refused. |
| Agent refusals | Retiring the locked value, a stale version and an unknown field are refused in the preview; the viewer's plan is `missing_capability`. |
