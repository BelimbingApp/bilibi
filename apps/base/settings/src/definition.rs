//! Immutable owner contributions. Exact keys win over longest wildcard patterns.
use bilibi_module_registry::{Catalog, Consumer, Entry, RegistryError};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub const DEFINITIONS: &str = "base/settings.definitions";
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum ScopeKind {
    Global,
    Tenant,
    Company,
    User,
}
impl ScopeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Tenant => "tenant",
            Self::Company => "company",
            Self::User => "user",
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValueType {
    Array,
    Boolean,
    Float,
    Integer,
    Mixed,
    String,
}
pub struct Validator {
    pub accepts: fn(&Value) -> bool,
    pub message: &'static str,
}
pub struct Editor {
    pub group: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub capability: Option<&'static str>,
    pub reveal_duration_ms: u32,
    /// Changing, clearing or restoring this setting in the editor asks for
    /// the holder's authenticator code every time (`false` for almost every
    /// setting). For a value that opens something beyond the settings page.
    pub change_needs_step_up: bool,
}
pub struct Definition {
    pub value_type: ValueType,
    pub scopes: &'static [ScopeKind],
    pub default: fn() -> Value,
    pub nullable: bool,
    pub encrypted: bool,
    pub minimum: Option<f64>,
    pub maximum: Option<f64>,
    pub validator: Option<Validator>,
    pub editor: Option<Editor>,
}
impl Definition {
    pub fn validate(&self, value: &Value) -> std::result::Result<(), &'static str> {
        let typed = if value.is_null() {
            self.nullable
        } else {
            match self.value_type {
                ValueType::Array => value.is_array() || value.is_object(),
                ValueType::Boolean => value.is_boolean(),
                ValueType::Float => value.is_f64(),
                ValueType::Integer => value.is_i64() || value.is_u64(),
                ValueType::Mixed => true,
                ValueType::String => value.is_string(),
            }
        };
        if !typed {
            return Err("Value has the wrong type");
        }
        if let Some(number) = value.as_f64() {
            if self.minimum.is_some_and(|min| number < min)
                || self.maximum.is_some_and(|max| number > max)
            {
                return Err("Value is outside the allowed range");
            }
        }
        if let Some(validator) = &self.validator {
            if !(validator.accepts)(value) {
                return Err(validator.message);
            }
        }
        Ok(())
    }
}
/// Runtime claims have no default, reject null writes and store JSON without
/// encryption. Use a Definition for secrets or user-editable configuration.
pub enum Setting {
    Definition(Definition),
    RuntimeClaim,
}
#[derive(Clone)]
pub struct OwnedSetting {
    pub pattern: &'static str,
    pub owner: &'static str,
    pub setting: &'static Setting,
}
#[derive(Clone, Default)]
pub struct Definitions {
    entries: BTreeMap<&'static str, OwnedSetting>,
}
impl Definitions {
    pub fn from_catalog(catalog: &Catalog) -> bilibi_module_registry::Result<Self> {
        Self::from_entries(catalog.entries::<Setting>(DEFINITIONS)?)
    }
    fn from_entries(entries: &[Entry]) -> bilibi_module_registry::Result<Self> {
        let mut result = Self::default();
        for entry in entries {
            let fail = |reason: &str| {
                RegistryError::Contribution(format!(
                    "setting {} declared by {} at {}:{}: {reason}",
                    entry.id,
                    entry.provenance.module,
                    entry.provenance.source,
                    entry.provenance.line
                ))
            };
            if !valid_key(entry.id) || result.entries.contains_key(entry.id) {
                return Err(fail("invalid or duplicate key"));
            }
            let setting = entry.get::<Setting>()?;
            if let Setting::Definition(d) = setting {
                let scopes: BTreeSet<_> = d.scopes.iter().collect();
                if scopes.is_empty() || scopes.len() != d.scopes.len() {
                    return Err(fail("scopes must be nonempty and unique"));
                }
                if (d.minimum.is_some() || d.maximum.is_some())
                    && !matches!(d.value_type, ValueType::Integer | ValueType::Float)
                    || d.minimum.is_some_and(|v| !v.is_finite())
                    || d.maximum.is_some_and(|v| !v.is_finite())
                    || matches!((d.minimum,d.maximum),(Some(a),Some(b)) if a>b)
                {
                    return Err(fail("invalid numeric bounds"));
                }
                if let Some(editor) = &d.editor {
                    if editor.group.is_empty()
                        || editor.label.is_empty()
                        || editor.help.is_empty()
                        || !(1_000..=60_000).contains(&editor.reveal_duration_ms)
                        || editor.capability.is_some_and(str::is_empty)
                    {
                        return Err(fail("invalid editor metadata"));
                    }
                }
                if d.validator.as_ref().is_some_and(|v| v.message.is_empty()) {
                    return Err(fail("validator must have a message"));
                }
                d.validate(&(d.default)()).map_err(fail)?;
            }
            result.entries.insert(
                entry.id,
                OwnedSetting {
                    pattern: entry.id,
                    owner: entry.provenance.module,
                    setting,
                },
            );
        }
        Ok(result)
    }
    pub fn all(&self) -> impl Iterator<Item = &OwnedSetting> {
        self.entries.values()
    }
    pub fn lookup(&self, key: &str) -> bilibi_database::Result<&OwnedSetting> {
        if !valid_key(key) || key.contains('*') {
            return Err(bilibi_database::Error::Invalid);
        }
        self.entries
            .get(key)
            .or_else(|| {
                self.entries
                    .values()
                    .filter(|entry| entry.pattern.contains('*') && matches(entry.pattern, key))
                    .max_by_key(|entry| (entry.pattern.len(), entry.pattern))
            })
            .ok_or(bilibi_database::Error::Invalid)
    }
}
fn valid_key(key: &str) -> bool {
    !key.is_empty() && key.chars().count() <= 255 && !key.chars().any(char::is_control)
}
fn matches(pattern: &str, key: &str) -> bool {
    let parts: Vec<_> = pattern.split('*').collect();
    let Some(mut rest) = key.strip_prefix(parts[0]) else {
        return false;
    };
    for (index, part) in parts.iter().enumerate().skip(1) {
        if index == parts.len() - 1 {
            return rest.ends_with(part);
        }
        let Some(position) = rest.find(part) else {
            return false;
        };
        rest = &rest[position + part.len()..];
    }
    rest.is_empty()
}
inventory::submit! {
    Consumer::new::<Setting>(DEFINITIONS, "base/settings", |entries| Definitions::from_entries(entries).map(|_| ()))
}
