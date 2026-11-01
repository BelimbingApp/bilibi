# Base Custom Fields

Typed fields (text, number, date, pick-list) a tenant administrator adds to the records of a module that opts in, without a release. No Bilimbi counterpart. The decision and the rejected alternatives are in [ADR 21](../../../docs/architecture/decisions/21-tenant-defined-custom-fields.md).

- **This module owns the definitions, not the values.** A record's owner keeps values in its own row and checks them in its own write.
- **A tenant that defines no fields, or a module that does not opt in, sees no change.**

## Offer custom fields on a record type

Three steps in the owning module.

1. Declare the record type (discovered like capabilities, validated at boot):

   ```rust
   inventory::submit! {
       bilibi_module_registry::Contribution::new(
           bilibi_custom_fields::RECORD_TYPES, "company",
           bilibi_module_registry::Provenance { module: "core/company", source: file!(), line: line!() },
           &bilibi_custom_fields::RecordType { id: "company", label: "Companies" },
       )
   }
   ```

   - Add `base/custom_fields` to the module's descriptor `dependencies` and `bilibi-custom-fields.workspace = true` to its `Cargo.toml`.
   - The id is lower-case letters, digits and `_` (at most 32), unique, and never changes.
2. Add the storage in your own migration, once:

   ```sql
   ALTER TABLE companies
    ADD COLUMN custom_fields jsonb NOT NULL DEFAULT '{}'::jsonb,
    ADD CONSTRAINT companies_custom_fields_object CHECK (jsonb_typeof(custom_fields) = 'object');
   CREATE INDEX companies_custom_fields_idx ON companies USING gin (custom_fields jsonb_path_ops);
   ```
3. Read the fields a record offers with `CustomFields::active(client, scope, "company")` (service `bilibi_custom_fields::SERVICE`). It needs no custom-field capability: authorize your own page or write first.

## Behaviour

| Rule | Detail |
| --- | --- |
| Key | A lower-case letter, then letters, digits and `_`, at most 40. Fixed at creation, unique per tenant and record type, **never reused**, not even by an archived field. |
| Type | `text`, `number`, `date` or `choice`. Fixed at creation. |
| Stored value | Text as a JSON string; number as an exact decimal string, never a float; date as `YYYY-MM-DD`; choice as the pick-list value's code. |
| Choice | Names a standalone tenant pick-list that carries no capability of its own (`choice_lists()`); renaming, retiring and translating values happen in Pick-lists. |
| Archive, not delete | An archived field leaves forms, columns and agents; its values stay on records; restoring brings it back. |
| Limit | Active fields per record type: tenant setting `base.custom_fields.max_fields` (default 50, 1–200). Archived fields do not count, restoring one is refused at the limit. |
| Audit | Each definition write is one `custom_field` change with before and after. |
| Capabilities | `base.custom-fields.view` reads definitions; `base.custom-fields.manage` defines, renames, archives and restores. |

## Administration page

**Administration › Custom fields** (`/admin/custom-fields`, `base.custom-fields.view`) is the one list of the tenant's fields. The record type and the status are a filter and a column. Label and help text edit in place; archive and restore confirm first; **+ Field** opens the create dialog. Every change goes through the `base.custom_fields.*` contracts below, so the page and agents share one check and one audit trail.

## Agent operations

| Key | Capability | Does |
| --- | --- | --- |
| `base.custom_fields.record_types` | `view` | The record types that take fields and the pick-lists a choice field may use. |
| `base.custom_fields.list` | `view` | The tenant's fields, optionally for one record type; archived ones with `include_archived`. |
| `base.custom_fields.define` | `manage` | Add a field. |
| `base.custom_fields.update` | `manage` | Change label or help text; needs `version`. |
| `base.custom_fields.archive` / `.restore` | `manage` | Needs `version`. |

Each write is a reviewed safe write: the preview runs the facade's checks without locking or writing, and the commit writes on the plan's audited transaction. The guide `base.custom_fields` explains keys, types and archiving to agents.

## Storage

`base_custom_fields` (migration `base-custom_fields-001-definitions`): one row per tenant, record type and key. Every statement binds the tenant.
