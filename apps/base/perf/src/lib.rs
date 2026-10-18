//! Bounded, redacted operational performance history: request, live-event, job
//! and process samples recorded without ever delaying the work they describe.
//! See `README.md` for the contract and the running proof.
mod jobs;
mod observe;
mod read;
mod recorder;
mod retention;
mod runtime;
mod sample;
mod web;

pub use bilibi_database::timing;
pub use observe::Finish;
pub use read::{
    Diagnostics, Filter, Health, Page, Recorded, Regression, Store, Windows, PAGE_SIZES,
};
pub use recorder::{
    Config, Recorder, Stats, Submit, ENABLED, KEEP_DAYS, MAX_ROWS, MINIMUM_DURATION_MS,
    SAMPLE_RATE, SLOW_THRESHOLD_MS,
};
pub use sample::{valid_identity, Kind, Outcome, Process, Sample, SizeClass};

use bilibi_authz::{CapabilityDefinition, RoleDefinition, VocabularyContribution, VOCABULARY};
use bilibi_database::Db;
use bilibi_module_registry::{Contribution, Module, Provenance, RegistryError, Service};
use bilibi_settings::{Definition, Editor, ScopeKind, Setting, Settings, ValueType, DEFINITIONS};
use serde_json::json;
use std::sync::Arc;

pub const VIEW: &str = "admin.system.perf.view";
pub const MANAGE: &str = "admin.system.perf.manage";

macro_rules! provenance {
    () => {
        Provenance {
            module: "base/perf",
            source: file!(),
            line: line!(),
        }
    };
}
// Installation-global, so a grant alone is not enough: the persistent platform
// operator identity is required too.
inventory::submit! { Contribution::new(VOCABULARY, "base/perf", provenance!(), &VocabularyContribution {
    domains: &[],
    verbs: &[],
    capabilities: &[CapabilityDefinition::platform(VIEW), CapabilityDefinition::platform(MANAGE)],
    roles: &[RoleDefinition { code: "system_viewer", name: "System Viewer", description: "Contributed system role; reconciled explicitly", grant_all: false, capabilities: &[VIEW] }],
    system_principals: &[],
}) }

macro_rules! setting {
    ($key:expr, $type:expr, $default:expr, $min:expr, $max:expr, $label:expr, $help:expr) => {
        inventory::submit! { Contribution::new(DEFINITIONS, $key, provenance!(), &Setting::Definition(Definition {
            value_type: $type, scopes: &[ScopeKind::Global], default: || $default, nullable: false, encrypted: false,
            minimum: Some($min), maximum: Some($max), validator: None,
            editor: Some(Editor { group: "Performance", label: $label, help: $help, capability: Some(MANAGE), reveal_duration_ms: 5_000 }),
        })) }
    };
}
inventory::submit! { Contribution::new(DEFINITIONS, ENABLED, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::Boolean, scopes: &[ScopeKind::Global], default: || json!(true), nullable: false, encrypted: false,
    minimum: None, maximum: None, validator: None,
    editor: Some(Editor { group: "Performance", label: "Performance history", help: "Keep a history of how fast pages and jobs run.", capability: Some(MANAGE), reveal_duration_ms: 5_000 }),
})) }
setting!(
    MINIMUM_DURATION_MS,
    ValueType::Integer,
    json!(100),
    0.0,
    60_000.0,
    "Minimum duration",
    "Do not record anything faster than this, in milliseconds."
);
setting!(
    SAMPLE_RATE,
    ValueType::Float,
    json!(1.0),
    0.0,
    1.0,
    "Sample rate",
    "Share of requests to record, from 0 to 1."
);
setting!(
    SLOW_THRESHOLD_MS,
    ValueType::Integer,
    json!(1000),
    1.0,
    3_600_000.0,
    "Slow threshold",
    "Anything slower than this, in milliseconds, counts as slow."
);
setting!(
    KEEP_DAYS,
    ValueType::Integer,
    json!(30),
    1.0,
    3650.0,
    "History retention",
    "Days to keep history."
);
setting!(
    MAX_ROWS,
    ValueType::Integer,
    json!(200_000),
    1_000.0,
    1_000_000.0,
    "History row limit",
    "Most records to keep."
);

inventory::submit! {
    Module { id: "base/perf", layer: bilibi_module_registry::Layer::Base,
        dependencies: &["base/module_registry", "base/database", "base/authz", "base/queue", "base/schedule", "base/settings", "base/dashboard", "base/tenancy", "base/datetime", "base/locale", "base/menu", "base/ui"],
        migrations: &[("base-perf-001", include_str!("../migrations/001-perf.sql"))],
        services: &[
            Service { id: "base/perf.recorder", factory: |services| {
                let recorder = Recorder::new(
                    services.get::<Db>("base/database.pool")?.as_ref().clone(),
                    services.get::<Settings>("base/settings.store")?,
                    Config::default(),
                ).map_err(|error| RegistryError::Service(error.to_string()))?;
                Ok(Arc::new(recorder))
            } },
            Service { id: "base/perf.pages", factory: |services| {
                Ok(Arc::new(bilibi_ui::page::PageService::<bilibi_tenancy::Scope, bilibi_database::Error>(Arc::new(web::Pages {
                    recorder: services.get("base/perf.recorder")?,
                    db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
                    display: services.get("base/datetime.display")?,
                    locales: services.get("base/locale.locales")?,
                }))))
            } },
            Service { id: "base/perf.jobs", factory: |services| {
                Ok(Arc::new(bilibi_queue::execution::ObserverService(Arc::new(jobs::Jobs(services.get("base/perf.recorder")?)))))
            } },
            Service { id: "base/perf.authz_retention", factory: |services| {
                Ok(Arc::new(bilibi_queue::execution::HandlerService(Arc::new(retention::AuthzRetention {
                    schedule: services.get("base/schedule.store")?,
                    settings: services.get::<Settings>("base/settings.store")?,
                    db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
                }))))
            } },
            Service { id: "base/perf.retention", factory: |services| {
                Ok(Arc::new(bilibi_queue::execution::HandlerService(Arc::new(retention::Retention {
                    schedule: services.get("base/schedule.store")?,
                    recorder: services.get("base/perf.recorder")?,
                }))))
            } },
        ],
    }
}
