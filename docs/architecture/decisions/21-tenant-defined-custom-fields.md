# Tenant-defined custom fields stay "lite" and live in the owner's row

**Document Type:** Architecture decision record
**Status:** Accepted; implementation in progress
**Agents:** claude-code/sonnet-5.5-default
**Scope:** Fields a tenant administrator adds to a record type without a release (issue #131)
**Last Updated:** 2026-11-01

A hosted Bilibi with many customers cannot ship a release for every attribute a customer tracks. Static fields from an Extension (Company's field contribution contract) cover one customer per instance. The owner's direction on #131 (2026-10-26) is the scope here: simplicity now, open for later options. A tenant administrator defines **text, number, date and pick-list** fields, shown on the record page and as list columns. There are no reference fields and no per-field runtime permissions. Custom record types, calculated fields, dependent choices, unique fields and import are not part of it.

**Definitions are Base data, values are the owner's.** `base/custom_fields` owns one row per tenant, record type and key: label, help text, type, the pick-list a choice field uses, position, archived date and version. The key and the type never change and a key is never reused, so a value stored under it is always read as the type it was written as. A record's owner adds `custom_fields jsonb NOT NULL DEFAULT '{}'` to its own table (with an object check and a `gin (… jsonb_path_ops)` index) and checks every value against the tenant's definitions inside its own write. The tenant predicate, soft delete, version check and one audited `Change` then cover the values with the row. Tenants never run DDL.

**Opting in is a contribution.** An owner declares a `RecordType` to `base/custom_fields.record_types`. Nothing becomes extensible by accident, and a module that is not mounted offers nothing.

**Choice fields reuse governed pick-lists.** A choice field names a standalone tenant pick-list (`base/picklists`, ADR 07) instead of keeping options of its own, so renaming, retiring and translating values has one door. A retired value stays valid on records that hold it.

**Numbers are exact strings.** A number is stored as a decimal string, never a float, so no digit is lost; dates are `YYYY-MM-DD`.

**One door, many keys.** Page and agent share the `base.custom_fields.*` operations (ADR 01); the page calls the same contracts as an agent. Archiving is the only removal: values stay on records and the field can be restored.

**Limits are settings.** Fields per record type (default 50) is a tenant setting with bounds, edited in System Settings; a text length limit arrives with value editing. Lowering a limit never rewrites stored values.

## Why not the alternatives

- **A values table (one row per record and field).** Typed indexes, but a join per shown field, a second write per save and values outside the record's version.
- **Real columns per field.** Tenants would run DDL, which is refused.
- **Full runtime fields** (references, per-field permissions, restrictions in Authz, choice options of their own). The first attempt (#174) showed what it costs across Grid, Authz, audit views and the agent schema; it is not planned. The same `RecordType` contribution leaves room for it.
- **Fields as code only.** Right for one customer per instance (the static Company field contribution stays), wrong for a hosted many-customer instance.

The contract and the runtime proof are in [Custom Fields](../../../apps/base/custom_fields/README.md).
