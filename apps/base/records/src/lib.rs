//! Record references: one way for any module to point at a record another
//! module owns, including a record in another Domain, without depending on
//! that module. An owner registers each kind of record once (`declare!`): its
//! audit kind, Grid table, page path and an access check. A holder stores
//! `(record_kind, record_id)` and names, links and checks references through
//! [`Records`]. Contract: `README.md`.
mod declaration;
mod drops;
mod operations;
mod records;

pub use declaration::{Access, Holder, RecordKind, RecordOwner, HOLDERS, KINDS};
pub use drops::{AddTo, DropRefusal, DropTarget, DROP_EVENT, DROP_PARAM, DROP_TARGETS, MAX_DROP};
pub use records::{
    Hit, Records, Reference, Referrers, Refusal, Resolved, MAX_PER_KIND, MAX_SEARCH_KINDS,
    PICKER_HITS,
};
#[doc(hidden)]
pub use {
    bilibi_module_registry::{Contribution, Provenance},
    inventory,
};

use bilibi_authz::{CapabilityDefinition, RoleDefinition, VocabularyContribution, VOCABULARY};
use bilibi_module_registry::{
    contributions::{Method, Route, ROUTES},
    Catalog, Module, RegistryError, Service,
};
use bilibi_settings::{Definition, Editor, ScopeKind, Setting, ValueType, DEFINITIONS};
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc};

pub const SERVICE: &str = "base/records.records";
/// Reads references and picker hits through agents; names still follow the
/// reader's permission on each kind.
pub const VIEW: &str = "base.records.view";

macro_rules! provenance {
    () => {
        Provenance {
            module: "base/records",
            source: file!(),
            line: line!(),
        }
    };
}

/// Authz's built-in roles read references too, as they search; Core
/// Administrator holds every capability already.
const fn role(code: &'static str, name: &'static str) -> RoleDefinition {
    RoleDefinition {
        code,
        name,
        description: "Contributed system role; reconciled explicitly",
        grant_all: false,
        capabilities: &[VIEW],
    }
}
inventory::submit! { Contribution::new(VOCABULARY, "base/records", provenance!(), &VocabularyContribution {
    domains: &["base"], verbs: &["view"],
    capabilities: &[CapabilityDefinition::tenant(VIEW)],
    roles: &[role("tenant_owner", "Tenant Owner"), role("auditor", "Auditor"), role("system_viewer", "System Viewer")],
    system_principals: &[],
}) }

inventory::submit! { Contribution::new(DEFINITIONS, PICKER_HITS, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::Integer, scopes: &[ScopeKind::Global, ScopeKind::Tenant], default: || json!(8),
    nullable: false, encrypted: false, minimum: Some(1.0), maximum: Some(50.0), validator: None,
    editor: Some(Editor {
        group: "Records",
        label: "Picker results per kind",
        help: "How many matching records a record picker offers for each kind of record.",
        capability: None,
        reveal_duration_ms: 5_000,
    }),
})) }

/// What boot proves once the Grid catalog and the routes are known (the
/// service factory, not the consumer validator):
/// - a kind's Grid table exists, claims the kind's key in `record_types` and
///   names a `label_field`;
/// - a kind's page path is a contributed GET page route;
/// - a holder's Grid table exists with both of its fields.
pub fn validate_installed(catalog: &Catalog) -> bilibi_module_registry::Result<()> {
    let installed = bilibi_grid::installed()?;
    let routes: Vec<&Route> = catalog
        .entries::<Route>(ROUTES)?
        .iter()
        .map(|entry| entry.get::<Route>())
        .collect::<bilibi_module_registry::Result<_>>()?;
    for entry in catalog.entries::<RecordKind>(KINDS)? {
        let kind = entry.get::<RecordKind>()?;
        let refuse = |reason: String| {
            Err(RegistryError::Contribution(format!(
                "record kind {} at {}:{} {reason}",
                entry.id, entry.provenance.source, entry.provenance.line
            )))
        };
        let Some(table) = installed.table(kind.table) else {
            return refuse(format!(
                "names Grid table {}, which does not exist",
                kind.table
            ));
        };
        if !table.decl.record_types.contains(&kind.key) {
            return refuse(format!(
                "names Grid table {}, which does not claim {} in record_types",
                kind.table, kind.key
            ));
        }
        if table.decl.label_field.is_none() {
            return refuse(format!(
                "names Grid table {}, which has no label_field",
                kind.table
            ));
        }
        if let Some(path) = kind.path {
            if !routes
                .iter()
                .any(|route| route.method == Method::Get && route.path == path)
            {
                return refuse(format!(
                    "names page {path}, which is no contributed GET page route"
                ));
            }
        }
    }
    for entry in catalog.entries::<Holder>(HOLDERS)? {
        let holder = entry.get::<Holder>()?;
        let ok = installed.table(holder.table).is_some_and(|table| {
            table.field(holder.kind_field).is_some() && table.field(holder.id_field).is_some()
        });
        if !ok {
            return Err(RegistryError::Contribution(format!(
                "record holder {} at {}:{} names no Grid table with fields {} and {}",
                entry.id,
                entry.provenance.source,
                entry.provenance.line,
                holder.kind_field,
                holder.id_field
            )));
        }
    }
    Ok(())
}

fn build(services: &bilibi_module_registry::Services) -> bilibi_module_registry::Result<Records> {
    // The linked modules' declarations; an unmounted module declares none.
    let catalog = &Catalog::discover(&bilibi_module_registry::discover()?)?;
    validate_installed(catalog)?;
    drops::validate_installed(catalog)?;
    let mut kinds = BTreeMap::new();
    for entry in catalog.entries::<RecordKind>(KINDS)? {
        let kind = entry.get::<RecordKind>()?;
        kinds.insert(kind.key, (kind, entry.provenance.module));
    }
    let holders = catalog
        .entries::<Holder>(HOLDERS)?
        .iter()
        .map(|entry| entry.get::<Holder>())
        .collect::<bilibi_module_registry::Result<_>>()?;
    let drop_targets = catalog
        .entries::<DropTarget>(DROP_TARGETS)?
        .iter()
        .map(|entry| entry.get::<DropTarget>())
        .collect::<bilibi_module_registry::Result<_>>()?;
    Ok(Records {
        db: services
            .get::<bilibi_database::Db>("base/database.pool")?
            .as_ref()
            .clone(),
        settings: services.get("base/settings.store")?,
        kinds,
        holders,
        drop_targets,
    })
}

inventory::submit! {
    Module {
        id: "base/records", layer: bilibi_module_registry::Layer::Base,
        dependencies: &["base/module_registry", "base/database", "base/tenancy", "base/authz", "base/grid", "base/ui", "base/settings", "base/agent_api"],
        migrations: &[("base-records-001-redirects", include_str!("../migrations/001-redirects.sql"))],
        services: &[Service { id: SERVICE, factory: |services| {
            let records = Arc::new(build(services)?);
            operations::bind(records.clone())?;
            Ok(records)
        } }],
    }
}
