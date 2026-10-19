//! Manual `/system/settings` specimen, deliberately separate from product
//! settings. It serves the normal app plus four fixture-only editable
//! settings: an encrypted token (reveal, keep-current mask, read-only without
//! a key), a fraction (float input), a switch (checkbox) and a limit gated on
//! its own capability (withheld when denied). Production has no encrypted
//! editable setting, so this is where the reveal flow can be exercised.
//!
//! Run with an isolated, migrated DATABASE_URL, APP_ORIGIN,
//! BIND_ADDR and (to store or show the token) BILIBI_SETTINGS_KEY. See
//! `apps/base/settings_editor/README.md` for the walkthrough.
use bilibi_base::authz::{CapabilityDefinition, VocabularyContribution, VOCABULARY};
use bilibi_base::settings::{Definition, Editor, ScopeKind, Setting, ValueType, DEFINITIONS};
use bilibi_database::{Error, Result};
use bilibi_module_registry::{Contribution, Layer, Module, Provenance};
use serde_json::{json, Value};
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();

const MANAGE: &str = "reference.settings.manage";
macro_rules! provenance {
    () => {
        Provenance {
            module: "extensions/settings_editor_reference",
            source: file!(),
            line: line!(),
        }
    };
}
macro_rules! editor {
    ($label:literal, $help:literal, $capability:expr) => {
        Some(Editor {
            group: "Reference",
            label: $label,
            help: $help,
            capability: $capability,
            reveal_duration_ms: 10_000,
            change_needs_step_up: false,
        })
    };
}

inventory::submit! {
    Module { id: "extensions/settings_editor_reference", layer: Layer::Extensions,
        dependencies: &["base/settings", "base/authz"], migrations: &[], services: &[] }
}
inventory::submit! { Contribution::new(VOCABULARY, "extensions/settings_editor_reference", provenance!(), &VocabularyContribution {
    domains: &["reference"], verbs: &["manage"], capabilities: &[CapabilityDefinition::tenant(MANAGE)],
    roles: &[], system_principals: &[],
}) }
inventory::submit! { Contribution::new(DEFINITIONS, "reference.settings.api_token", provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::String, scopes: &[ScopeKind::Global], default: || Value::Null, nullable: true, encrypted: true,
    minimum: None, maximum: None, validator: None,
    editor: editor!("Reference API token", "Credential a reference integration would send. Stored encrypted.", None),
})) }
inventory::submit! { Contribution::new(DEFINITIONS, "reference.settings.sample_ratio", provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::Float, scopes: &[ScopeKind::Global], default: || json!(0.25), nullable: false, encrypted: false,
    minimum: Some(0.0), maximum: Some(1.0), validator: None,
    editor: editor!("Reference sample ratio", "Fraction of reference events to keep, from 0 to 1.", None),
})) }
inventory::submit! { Contribution::new(DEFINITIONS, "reference.settings.enabled", provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::Boolean, scopes: &[ScopeKind::Global], default: || json!(true), nullable: false, encrypted: false,
    minimum: None, maximum: None, validator: None,
    editor: editor!("Reference integration", "Run the reference integration.", None),
})) }
inventory::submit! { Contribution::new(DEFINITIONS, "reference.settings.restricted_limit", provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::Integer, scopes: &[ScopeKind::Global], default: || json!(10), nullable: false, encrypted: false,
    minimum: Some(1.0), maximum: Some(100.0), validator: None,
    editor: editor!("Reference restricted limit", "Only people who manage reference settings see this.", Some(MANAGE)),
})) }

#[tokio::main]
async fn main() -> Result<()> {
    let db = bilibi_database::pool(&std::env::var("DATABASE_URL").map_err(|_| Error::Invalid)?)?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let origin = std::env::var("APP_ORIGIN").map_err(|_| Error::Invalid)?;
    let bind = std::env::var("BIND_ADDR").map_err(|_| Error::Invalid)?;
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|_| Error::Internal)?;
    println!("Settings editor reference listening at {origin}");
    axum::serve(
        listener,
        bilibi_web::router(bilibi_web::App::new(db, &origin)?),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await
    .map_err(|_| Error::Internal)
}
