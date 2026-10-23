//! Static opt-in and the runtime definition model (Bilimbi `CustomFields`).
use bilibi_module_registry::{Consumer, Entry, RegistryError};
use serde::{Deserialize, Serialize};

pub const SERVICE: &str = "base/custom_fields.fields";
pub const RECORD_TYPES: &str = "base/custom_fields.record_types";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    Text,
    LongText,
    Integer,
    Decimal,
    Date,
    DateTime,
    Boolean,
    Choice,
    MultiChoice,
    Reference,
}
impl FieldType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::LongText => "long_text",
            Self::Integer => "integer",
            Self::Decimal => "decimal",
            Self::Date => "date",
            Self::DateTime => "date_time",
            Self::Boolean => "boolean",
            Self::Choice => "choice",
            Self::MultiChoice => "multi_choice",
            Self::Reference => "reference",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "text" => Self::Text,
            "long_text" => Self::LongText,
            "integer" => Self::Integer,
            "decimal" => Self::Decimal,
            "date" => Self::Date,
            "date_time" => Self::DateTime,
            "boolean" => Self::Boolean,
            "choice" => Self::Choice,
            "multi_choice" => Self::MultiChoice,
            "reference" => Self::Reference,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RecordType {
    pub key: &'static str,
    pub label: &'static str,
    pub audit_kind: &'static str,
    pub grid_table: &'static str,
    pub custom_column: &'static str,
    pub capability: &'static str,
}
impl RecordType {
    pub const fn new(
        key: &'static str,
        label: &'static str,
        audit_kind: &'static str,
        grid_table: &'static str,
        custom_column: &'static str,
        capability: &'static str,
    ) -> Self {
        Self {
            key,
            label,
            audit_kind,
            grid_table,
            custom_column,
            capability,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Definition {
    pub id: i64,
    pub tenant_id: i64,
    pub record_type: String,
    pub key: String,
    pub label: String,
    pub field_type: FieldType,
    pub required: bool,
    pub help_text: Option<String>,
    pub position: i32,
    pub options: serde_json::Value,
    pub reference_table: Option<String>,
    pub text_limit: Option<i32>,
    pub format: Option<String>,
    pub archived: bool,
    pub version: i64,
}

pub(crate) fn registered(entries: &[Entry]) -> Result<Vec<RecordType>, RegistryError> {
    let mut types = Vec::new();
    for entry in entries {
        let ty = entry.get::<RecordType>()?;
        let valid_key = !ty.key.is_empty()
            && ty.key.len() <= 120
            && ty.key.split('.').all(|part| {
                !part.is_empty()
                    && part
                        .bytes()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
            });
        if !valid_key
            || ty.label.trim().is_empty()
            || ty.audit_kind.is_empty()
            || ty.grid_table.is_empty()
            || ty.custom_column.is_empty()
            || ty.capability.is_empty()
            || types.iter().any(|old: &RecordType| old.key == ty.key)
        {
            return Err(RegistryError::Contribution(format!(
                "invalid or duplicate custom-field record type {} from {} ({}:{})",
                ty.key, entry.provenance.module, entry.provenance.source, entry.provenance.line
            )));
        }
        types.push(*ty);
    }
    Ok(types)
}

inventory::submit! { Consumer::new::<RecordType>(RECORD_TYPES, "base/custom_fields", |entries| { registered(entries).map(|_| ()) }) }
