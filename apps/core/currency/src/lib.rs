//! Currencies and exchange rates as Core master data: the ISO 4217 list, the
//! currencies each tenant enables, a base currency, dated rates and the one
//! conversion every module shares. A Domain may depend on Core but never on
//! another Domain, so a list kept in one Domain could not be shared. No
//! Bilimbi counterpart. Contract: `README.md`; decision: ADR 08.
mod facade;
mod operations;
mod web;

pub use facade::{
    normalize, Checked, Conversion, Currencies, Currency, FieldError, Path, Quote, Rate, RateInput,
    RateQuery, Refusal, Request, Unusable, NOTE_MAX, RATE_PLACES,
};
/// Civil dates in this API are `jiff::civil::Date`; this saves a caller the
/// extra dependency edge.
pub use jiff::civil::Date;
pub use rust_decimal::Decimal;

use bilibi_authz::{CapabilityDefinition, RoleDefinition, VocabularyContribution, VOCABULARY};
use bilibi_module_registry::{
    contributions::{
        Access, Method, PageWrites, Pending, Route, Seed, SeedAction, SeedMode, PAGE_WRITES,
        ROUTES, SEEDS,
    },
    Contribution, Layer, Module, Provenance, Service,
};
use bilibi_settings::{
    Definition, Editor, ScopeKind, Setting, Settings, Validator, ValueType, DEFINITIONS,
};
use serde_json::{json, Value};
use std::sync::Arc;

/// The registry service of the facade.
pub const SERVICE: &str = "core/currency.currencies";
/// The two operator pages.
pub(crate) const PAGES: &str = "core/currency.pages";

/// See the currency list, the rates and the conversion.
pub const VIEW: &str = "admin.currency.view";
/// Enable and disable currencies and choose the base currency.
pub const MANAGE: &str = "admin.currency.manage";
/// Add, change and delete exchange rates.
pub const RATES_MANAGE: &str = "admin.currency.rates.manage";

/// The base currency's setting: a code, for the tenant or one company; null
/// until set.
pub const BASE_KEY: &str = "currency.base";
/// How old a rate may be and still be used, in days; 0 means no limit.
pub const MAX_AGE_KEY: &str = "currency.rates.max_age_days";

pub const CURRENCIES_PATH: &str = "/currencies";
pub const RATES_PATH: &str = "/currencies/rates";

pub const MIGRATION: (&str, &str) = (
    "core-currency-001-currency",
    include_str!("../migrations/001-currency.sql"),
);

macro_rules! provenance {
    () => {
        Provenance {
            module: "core/currency",
            source: file!(),
            line: line!(),
        }
    };
}

inventory::submit! {
    Module {
        id: "core/currency", layer: Layer::Core,
        dependencies: &["base/module_registry", "base/database", "base/tenancy", "base/authz", "base/settings", "base/ui", "base/agent_api", "base/menu", "base/party_ids", "core/address", "core/company"],
        migrations: &[MIGRATION],
        services: &[
            Service { id: SERVICE, factory: |services| {
                let currencies = Arc::new(Currencies {
                    settings: services.get::<Settings>("base/settings.store")?,
                });
                operations::bind(services.get::<bilibi_database::Db>("base/database.pool")?.as_ref().clone(), currencies.clone())?;
                Ok(currencies)
            } },
            Service { id: PAGES, factory: |services| Ok(Arc::new(bilibi_ui::page::PageService::<bilibi_tenancy::Scope, bilibi_database::Error>(Arc::new(web::Pages {
                db: services.get::<bilibi_database::Db>("base/database.pool")?.as_ref().clone(),
                currencies: services.get(SERVICE)?,
                settings: services.get("base/settings.store")?,
                addresses: services.get("core/address.addresses")?,
                contracts: services.get(bilibi_agent_api::page::SERVICE)?,
            })))) },
        ],
    }
}

inventory::submit! { Contribution::new(VOCABULARY, "core/currency", provenance!(), &VocabularyContribution {
    domains: &[],
    verbs: &[],
    capabilities: &[
        CapabilityDefinition::tenant(VIEW),
        CapabilityDefinition::tenant(MANAGE),
        CapabilityDefinition::tenant(RATES_MANAGE),
    ],
    roles: &[
        RoleDefinition { code: "tenant_owner", name: "Tenant Owner", description: "Contributed system role; reconciled explicitly", grant_all: false, capabilities: &[VIEW, MANAGE, RATES_MANAGE] },
    ],
    system_principals: &[],
}) }

// The ISO 4217 list, installed once by `bilibi-web seed-production`. A later
// version adds new codes and marks withdrawn ones under its own version.
inventory::submit! { Contribution::new(SEEDS, "core/currency.iso4217-v1", provenance!(), &Seed {
    version: "currencies-iso4217-v1",
    mode: SeedMode::Production,
    action: SeedAction::Sql(include_str!("../seeds/iso4217-v1.sql")),
}) }

fn code_or_null(value: &Value) -> bool {
    value.is_null()
        || value
            .as_str()
            .is_some_and(|code| code.len() == 3 && code.bytes().all(|b| b.is_ascii_uppercase()))
}
// The page and `core.currency.base.set` are the operator UI for the base
// currency: they only accept an enabled currency, which a settings form would not know.
inventory::submit! { Contribution::new(DEFINITIONS, BASE_KEY, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::String, scopes: &[ScopeKind::Tenant, ScopeKind::Company], default: || Value::Null, nullable: true, encrypted: false,
    minimum: None, maximum: None,
    validator: Some(Validator { accepts: code_or_null, message: "Use a three-letter currency code such as MYR." }),
    editor: None,
})) }
inventory::submit! { Contribution::new(DEFINITIONS, MAX_AGE_KEY, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::Integer, scopes: &[ScopeKind::Tenant], default: || json!(7), nullable: false, encrypted: false,
    minimum: Some(0.0), maximum: Some(3650.0), validator: None,
    editor: Some(Editor { group: "Currencies", label: "Oldest usable exchange rate (days)", help: "A conversion ignores a rate older than this many days before its date. Use 0 for no limit.", capability: Some(MANAGE), reveal_duration_ms: 5_000 }),
})) }

inventory::submit! { Contribution::new(ROUTES, "core/currency.currencies", provenance!(), &Route {
    method: Method::Get, path: CURRENCIES_PATH, service: PAGES, operation: "currencies",
    access: Access::Capability(VIEW), framed: true,
}) }
inventory::submit! { Contribution::new(ROUTES, "core/currency.rates", provenance!(), &Route {
    method: Method::Get, path: RATES_PATH, service: PAGES, operation: "rates",
    access: Access::Capability(VIEW), framed: true,
}) }

// Every write goes through the contracts agents call.
inventory::submit! { Contribution::new(PAGE_WRITES, PAGES, provenance!(), &PageWrites {
    service: PAGES,
    contracts: &[
        ("enable", "core.currency.enabled.set"),
        ("disable", "core.currency.enabled.set"),
        ("save_base", "core.currency.base.set"),
        ("use_suggestion", "core.currency.base.set"),
        ("save_company_base", "core.currency.base.set"),
        ("create_rate", "core.currency.rates.set"),
        ("save_rate", "core.currency.rates.set"),
        ("delete_rate", "core.currency.rates.delete"),
    ],
    pending: Pending::None,
}) }

bilibi_menu::contribute!(
    "core/currency",
    bilibi_menu::MenuItem::container("admin.currency", "Currencies")
        .under("admin")
        .icon("currency-dollar")
        .requires(bilibi_menu::Requirement::Capability(VIEW))
        .order(55)
);
bilibi_menu::contribute!(
    "core/currency",
    bilibi_menu::MenuItem::page("admin.currency.list", "Currency list", CURRENCIES_PATH)
        .under("admin.currency")
        .icon("banknotes")
        .order(10)
);
bilibi_menu::contribute!(
    "core/currency",
    bilibi_menu::MenuItem::page("admin.currency.rates", "Exchange rates", RATES_PATH)
        .under("admin.currency")
        .icon("arrows-right-left")
        .order(20)
);
