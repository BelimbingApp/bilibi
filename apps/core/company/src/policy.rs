//! Company policy settings, extending Bilimbi Core.Company through Base Settings.
use bilibi_database::{Error, Result};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_settings::{
    Definition, Editor, ScopeKind, Setting, Settings, Target, Validator, ValueType, DEFINITIONS,
};
use bilibi_tenancy::{ReadClient, Scope};
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock};

static SETTINGS: OnceLock<Arc<Settings>> = OnceLock::new();
pub fn bind(settings: Arc<Settings>) -> bilibi_module_registry::Result<()> {
    SETTINGS.set(settings).map_err(|_| {
        bilibi_module_registry::RegistryError::Service("duplicate company settings binding".into())
    })
}
pub async fn get(client: &dyn ReadClient, scope: &Scope, key: &str) -> Result<Value> {
    Ok(SETTINGS
        .get()
        .ok_or(Error::Internal)?
        .get(client, scope, Target::Tenant, key)
        .await?
        .value)
}
pub async fn depth(client: &dyn ReadClient, scope: &Scope) -> Result<i32> {
    Ok(get(client, scope, "company.hierarchy.max_depth")
        .await?
        .as_i64()
        .ok_or(Error::Internal)? as i32)
}
macro_rules! setting {
    ($key:literal, $kind:ident, $default:expr, $min:expr, $max:expr, $validator:expr, $label:literal, $help:literal) => {
        inventory::submit! { Contribution::new(DEFINITIONS, $key, Provenance { module: "core/company", source: file!(), line: line!() }, &Setting::Definition(Definition {
            value_type: ValueType::$kind, scopes: &[ScopeKind::Tenant], default: || json!($default), nullable: false, encrypted: false,
            minimum: $min, maximum: $max, validator: $validator,
            editor: Some(Editor { group: "Companies", label: $label, help: $help, capability: Some(crate::UPDATE), reveal_duration_ms: 5_000 }),
        })) }
    }
}
fn words(value: &Value) -> bool {
    value.as_array().is_some_and(|values| {
        values.len() <= 200
            && values.iter().all(|v| {
                v.as_str()
                    .is_some_and(|s| !s.is_empty() && s.len() <= 100 && !s.contains('\0'))
            })
    })
}
setting!(
    "company.duplicates.threshold",
    Float,
    0.80,
    Some(0.50),
    Some(1.0),
    None,
    "Duplicate match threshold",
    "Show companies with a match score at or above this number, from 0.50 to 1.00."
);
setting!(
    "company.duplicates.on_create",
    String,
    "suggest",
    None,
    None,
    Some(Validator {
        accepts: |v| matches!(v.as_str(), Some("suggest" | "require_reason")),
        message: "Choose suggest or require_reason."
    }),
    "Duplicate check on create",
    "Use suggest to allow creation, or require_reason to require a reason when matches exist."
);
setting!(
    "company.duplicates.ignored_words",
    Array,
    ["sdn", "bhd", "ltd", "inc", "llc", "gmbh"],
    None,
    None,
    Some(Validator {
        accepts: words,
        message: "Provide up to 200 words, each at most 100 bytes."
    }),
    "Words ignored in company names",
    "Ignore legal suffixes such as sdn, bhd and ltd when comparing names."
);
setting!(
    "company.duplicates.public_mail_domains",
    Array,
    [
        "gmail.com",
        "googlemail.com",
        "yahoo.com",
        "hotmail.com",
        "outlook.com",
        "live.com",
        "icloud.com",
        "aol.com",
        "proton.me",
        "protonmail.com"
    ],
    None,
    None,
    Some(Validator {
        accepts: words,
        message: "Provide up to 200 domains, each at most 100 bytes."
    }),
    "Public email domains",
    "Email addresses at these domains never count as a company match."
);
setting!(
    "company.hierarchy.max_depth",
    Integer,
    10,
    Some(2.0),
    Some(20.0),
    None,
    "Maximum company tree depth (levels)",
    "Limit new parent links to this many levels, from 2 to 20."
);
