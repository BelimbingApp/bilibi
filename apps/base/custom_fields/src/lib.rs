//! Tenant-defined custom fields: typed fields (text, number, date, pick-list)
//! an administrator adds to the records of a module that opts in, without a
//! release. This module owns the tenant's definitions and the page and agent
//! operations that manage them; the record's owner keeps the values in its own
//! row and checks them against these definitions in its own write.
//! Contract: `README.md`; decision: ADR 21.
mod declaration;
mod definitions;
mod operations;
mod values;
mod web;

pub use declaration::{RecordType, LABEL_MAX, RECORD_TYPES, RECORD_TYPE_MAX};
pub use definitions::{
    Checked, CustomFields, Definition, FieldError, FieldUpdate, Kind, NewField, HELP_MAX, KEY_MAX,
};
pub use values::{valid_date, valid_number};
#[doc(hidden)]
pub use {
    bilibi_module_registry::{Contribution, Provenance},
    inventory,
};

use bilibi_authz::{CapabilityDefinition, VocabularyContribution, VOCABULARY};
use bilibi_module_registry::{
    contributions::{Access, Method, PageWrites, Pending, Route, PAGE_WRITES, ROUTES},
    Catalog, Module, Service,
};
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc};

pub const SERVICE: &str = "base/custom_fields.fields";
/// The administration page.
pub(crate) const PAGES: &str = "base/custom_fields.pages";
/// Read the definitions on the page and through agents.
pub const VIEW: &str = "base.custom-fields.view";
/// Define, rename, archive and restore fields.
pub const MANAGE: &str = "base.custom-fields.manage";
/// The most active fields one record type may have, per organisation.
pub const MAX_FIELDS: &str = "base.custom_fields.max_fields";
pub const MAX_FIELDS_DEFAULT: i64 = 50;
pub const MAX_FIELDS_MIN: i64 = 1;
pub const MAX_FIELDS_MAX: i64 = 200;
/// The longest text value a text field may hold, per organisation.
pub const MAX_TEXT_LENGTH: &str = "base.custom_fields.max_text_length";
pub const MAX_TEXT_LENGTH_DEFAULT: i64 = 1_000;
pub const MAX_TEXT_LENGTH_MIN: i64 = 1;
/// Keeps every valid value inside a live page event.
pub const MAX_TEXT_LENGTH_MAX: i64 = 10_000;

macro_rules! provenance {
    () => {
        Provenance {
            module: "base/custom_fields",
            source: file!(),
            line: line!(),
        }
    };
}

inventory::submit! { Contribution::new(VOCABULARY, "base/custom_fields", provenance!(), &VocabularyContribution {
    domains: &["base"],
    verbs: &[],
    capabilities: &[CapabilityDefinition::tenant(VIEW), CapabilityDefinition::tenant(MANAGE)],
    roles: &[],
    system_principals: &[],
}) }

inventory::submit! { Contribution::new(bilibi_settings::DEFINITIONS, MAX_FIELDS, provenance!(),
&bilibi_settings::Setting::Definition(bilibi_settings::Definition {
    value_type: bilibi_settings::ValueType::Integer,
    scopes: &[bilibi_settings::ScopeKind::Tenant],
    default: || json!(MAX_FIELDS_DEFAULT),
    nullable: false,
    encrypted: false,
    minimum: Some(MAX_FIELDS_MIN as f64),
    maximum: Some(MAX_FIELDS_MAX as f64),
    validator: None,
    editor: Some(bilibi_settings::Editor {
        group: "Custom fields",
        label: "Custom fields per record type",
        help: "The most active custom fields one kind of record, such as a company, may have. Archived fields do not count.",
        capability: Some(MANAGE),
        reveal_duration_ms: 5_000,
    }),
})) }

inventory::submit! { Contribution::new(bilibi_settings::DEFINITIONS, MAX_TEXT_LENGTH, provenance!(),
&bilibi_settings::Setting::Definition(bilibi_settings::Definition {
    value_type: bilibi_settings::ValueType::Integer,
    scopes: &[bilibi_settings::ScopeKind::Tenant],
    default: || json!(MAX_TEXT_LENGTH_DEFAULT),
    nullable: false,
    encrypted: false,
    minimum: Some(MAX_TEXT_LENGTH_MIN as f64),
    maximum: Some(MAX_TEXT_LENGTH_MAX as f64),
    validator: None,
    editor: Some(bilibi_settings::Editor {
        group: "Custom fields",
        label: "Custom text field length",
        help: "The most characters a custom text field may hold. A lower limit never changes values already saved.",
        capability: Some(MANAGE),
        reveal_duration_ms: 5_000,
    }),
})) }

fn build(
    services: &bilibi_module_registry::Services,
) -> bilibi_module_registry::Result<CustomFields> {
    // The linked modules' declarations; an unmounted module declares none.
    let catalog = &Catalog::discover(&bilibi_module_registry::discover()?)?;
    let mut record_types = BTreeMap::new();
    for entry in catalog.entries::<RecordType>(RECORD_TYPES)? {
        let record = entry.get::<RecordType>()?;
        record_types.insert(record.id, record);
    }
    Ok(CustomFields {
        record_types,
        picklists: services.get(bilibi_picklists::SERVICE)?,
        settings: services.get("base/settings.store")?,
    })
}

inventory::submit! {
    Module {
        id: "base/custom_fields", layer: bilibi_module_registry::Layer::Base,
        dependencies: &["base/module_registry", "base/database", "base/tenancy", "base/authz", "base/settings", "base/picklists", "base/ui", "base/audit", "base/agent_api", "base/menu"],
        migrations: &[("base-custom_fields-001-definitions", include_str!("../migrations/001-definitions.sql"))],
        services: &[
            Service { id: SERVICE, factory: |services| {
                let fields = Arc::new(build(services)?);
                operations::bind(services.get::<bilibi_database::Db>("base/database.pool")?.as_ref().clone(), fields.clone())?;
                Ok(fields)
            } },
            Service { id: PAGES, factory: |services| Ok(Arc::new(bilibi_ui::page::PageService::<bilibi_tenancy::Scope, bilibi_database::Error>(Arc::new(web::Pages {
                db: services.get::<bilibi_database::Db>("base/database.pool")?.as_ref().clone(),
                fields: services.get(SERVICE)?,
                contracts: services.get(bilibi_agent_api::page::SERVICE)?,
            })))) },
        ],
    }
}

inventory::submit! { Contribution::new(ROUTES, "base/custom_fields.index", provenance!(), &Route { company_scope: bilibi_module_registry::contributions::CompanyScope::Tenant,
    method: Method::Get, path: web::INDEX, service: PAGES, operation: "index", access: Access::Capability(VIEW), framed: true,
}) }
// Every write goes through the contracts agents call.
inventory::submit! { Contribution::new(PAGE_WRITES, PAGES, provenance!(), &PageWrites {
    service: PAGES,
    contracts: &[
        ("create", "base.custom_fields.define"),
        ("save_field", "base.custom_fields.update"),
        ("archive", "base.custom_fields.archive"),
        ("restore", "base.custom_fields.restore"),
    ],
    pending: Pending::None,
}) }
bilibi_menu::contribute!(
    "base/custom_fields",
    bilibi_menu::MenuItem::page("admin.custom_fields", "Custom fields", web::INDEX)
        .icon("puzzle-piece")
        .under("admin")
        .requires(bilibi_menu::Requirement::Capability(VIEW))
        .order(62)
);
