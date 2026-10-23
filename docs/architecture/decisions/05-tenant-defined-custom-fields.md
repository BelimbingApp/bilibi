# ADR 05: Tenant-defined fields stay with their owning records

**Document Type:** Architecture Decision Record
**Status:** Accepted; runtime Grid/Authz providers are implemented; full #131 remains in progress.
**Agents:** codex/gpt-6.1-sol-default (author)
**Scope:** Tenant-defined typed fields on opted-in module record types
**Last Updated:** 2026-10-22

## Context

Modules currently declare record and Grid fields at compile time. Tenants need additional attributes on records without a release or tenant-specific DDL, while retaining the owner's tenant boundary, version checks, audit history, and field restrictions.

## Decision

1. **The owner row stores values.** An opting-in owner adds one `custom jsonb NOT NULL DEFAULT '{}'` object column with an object check and a GIN `jsonb_path_ops` index. The owner migration adds it once. The owner remains responsible for tenant scoping, locking, versioning, and writes.
2. **Definitions are tenant-owned Base data.** `base/custom_fields` stores definitions keyed by tenant, record type, and immutable field key. It exposes a facade for listing, defining, updating, archiving, restoring, and validating definitions. Every write uses `MutationBoundary` and returns `Change` records.
3. **Opt-in is explicit.** An owner contributes a record type, its audit kind, Grid table ID, custom JSON column, and owner capability. Discovery validates the declaration against installed Grid and audit metadata. No other record type is extensible by default.
4. **Grid, Authz and Custom Fields use runtime-provider contracts.** Grid and Authz own their runtime metadata contracts; Custom Fields owns a typed reference-target resolver contract implemented by opted-in target owners. Runtime metadata and readable target keys are loaded for a sealed scope on each request and are never cached across requests. Grid masks unreadable reference keys and excludes reference fields from text search and owner sort/filter expressions. Dynamic relationship paths remain out of scope.
5. **Field keys never become SQL identifiers.** Grid resolves custom values through the owner's declared JSON column and a bound JSON key. Typed casts are selected from validated field metadata. List filters, sorting, and projections use the same query cost and timeout bounds as other Grid reads.
6. **Field restrictions reuse Authz storage.** Restrictions keep the existing `(tenant, table, field)` identity. Dynamic custom field IDs use the `cf_<key>` namespace. Authz validates new restrictions against the current tenant catalog, applies existing deny-wins masking/refusal behavior, and maps field restrictions to the opted-in audit record kind.
7. **Choice options belong to the field in this phase.** Codes are stable; labels and order may change; retired options remain valid on stored values but are unavailable for new assignments. Integration with governed pick-lists is a later change.
8. **No reporting or shared export support is added.** Those are separate issues.
9. **Custom-field resource limits are tenant settings.** The defaults are 100 active fields per record type, 100,000 text characters and 1 MiB of serialized custom JSON; tenant custom-field administrators may adjust them within 1–1,000 fields, 1–1,000,000 characters and 1 KiB–16 MiB. Owners resolve the current tenant values during definition and record-write transactions. Lowering a limit never rewrites stored values; changed values and the resulting custom object must satisfy the current limits.

## Consequences

- The dependency graph stays acyclic: Custom Fields depends on the Grid and Authz interfaces it implements; neither consumer imports Custom Fields.
- Grid and Authz gain request-scoped runtime catalog APIs, and Authz's restriction page becomes tenant-aware for contributed dynamic metadata while retaining static fields from the existing installed catalog. Custom Fields accepts reference target providers only for opted-in Grid tables; the target owner supplies readable choices and resolution on the caller's connection. The Company provider returns only live companies the actor may read. Grid and audit history mask unreadable target keys using that request's readable-key set.
- Grid's typed runtime provider feeds `Catalog::for_scope_with_runtime`; projection and text search bind each JSON key, while `query::field_expression` returns key bindings for owner-managed sort/filter statements. Authz's runtime provider feeds its restriction and audit-mask catalog. Providers run on the caller's connection or transaction and fail closed on errors.
- Types are enforced by the owner facade, not by PostgreSQL's JSONB storage. Decimal values are kept as exact strings; dates as `YYYY-MM-DD`; date-times as UTC ISO strings; choice values as option codes; references as target keys.
- A reference field's target table is immutable after creation. An existing target that becomes missing, archived or unreadable remains stored but is withheld; preserving it during an unrelated update does not reauthorize or reveal it. Assigning a new target requires a current readable-target proof inside the owner's mutation transaction.
- JSON expression sorting and filtering may be more expensive than indexed typed columns. Existing Grid admission and statement timeout bounds apply; per-field indexes remain a later, measured optimization.
- Custom field definition changes and each changed custom value are auditable. Restricted values must be removed before rendering or serialization, including agent output and audit history.

## First adopter

Core Company opts in through a new additive migration and uses the shared Base UI controls. Its existing free-form `metadata` JSON remains unchanged. Other modules opt in through their own contributions and migrations.
