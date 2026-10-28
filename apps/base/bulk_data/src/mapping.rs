//! A run's mapping: for each field the column (or fixed value) that fills it,
//! the column holding each row's source key for child imports, and what to do
//! with a row that matches an existing record. Checked against the record
//! type's offered fields and the file's header whenever it is saved or used.
use crate::record::{Field, Kind};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Most field entries one mapping holds.
pub const MAX_FIELDS: usize = 100;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mapping {
    pub fields: Vec<FieldMap>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_key: Option<String>,
    #[serde(default)]
    pub duplicates: Policy,
}

/// One field: a `column` of the file or a fixed `value`. A reference field's
/// column is looked up, or with `parent_run` names a row of that earlier
/// import by its source key.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldMap {
    pub field: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_run: Option<i64>,
}

/// What a row matching one existing record does. Several matches are always
/// a row error, never a guess.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Policy {
    /// Leave the existing record alone.
    #[default]
    Skip,
    /// Change the existing record to the row's values.
    Update,
    /// Create a new record anyway.
    Create,
}
impl Policy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Skip => "skip",
            Self::Update => "update",
            Self::Create => "create",
        }
    }
}

pub type FieldErrors = BTreeMap<String, Vec<String>>;

impl Mapping {
    /// The columns this mapping reads; it is offered for a file whose
    /// header has them all.
    pub fn columns(&self) -> Vec<String> {
        let mut columns: Vec<String> = self
            .fields
            .iter()
            .filter_map(|field| field.column.clone())
            .chain(self.source_key.clone())
            .collect();
        columns.sort();
        columns.dedup();
        columns
    }
    pub fn fits(&self, header: &[String]) -> bool {
        self.columns()
            .iter()
            .all(|column| header.iter().any(|name| name == column))
    }

    /// Refusals keyed by `fields.<field>`, `source_key` or `duplicates`.
    /// `offered` is what the person may fill; `header` is the file's, when
    /// the mapping is for a run. Parent runs are checked by the caller.
    pub fn check(
        &self,
        offered: &[Field],
        restricted: &[&Field],
        header: Option<&[String]>,
        matches: bool,
    ) -> Result<(), FieldErrors> {
        let mut errors = FieldErrors::new();
        let mut say = |key: String, message: &str| {
            errors.entry(key).or_default().push(message.to_owned());
        };
        if self.fields.len() > MAX_FIELDS {
            say("fields".into(), "has too many entries");
        }
        let known =
            |column: &str| header.is_none_or(|header| header.iter().any(|name| name == column));
        for (index, entry) in self.fields.iter().enumerate() {
            let key = format!("fields.{}", entry.field);
            if self.fields[..index]
                .iter()
                .any(|other| other.field == entry.field)
            {
                say(key, "is mapped twice");
                continue;
            }
            let Some(field) = offered.iter().find(|field| field.name == entry.field) else {
                let message = if restricted.iter().any(|field| field.name == entry.field) {
                    "is restricted for you, so you cannot import it"
                } else {
                    "is not a field of this record type"
                };
                say(key, message);
                continue;
            };
            match (&entry.column, &entry.value) {
                (Some(_), Some(_)) | (None, None) => {
                    say(key, "needs a column or a fixed value, not both");
                    continue;
                }
                (Some(column), None) if !known(column) => {
                    say(key.clone(), "names a column the file does not have")
                }
                (None, Some(value)) if value.trim().is_empty() => {
                    say(key.clone(), "needs a fixed value")
                }
                (None, Some(value)) => {
                    if let Err(message) = convert(field, value) {
                        say(key.clone(), &message);
                    }
                }
                _ => {}
            }
            match (entry.parent_run, field.reference) {
                (Some(_), None) => say(key, "is not a reference, so it has no parent import"),
                (Some(_), Some(reference)) if reference.parent.is_none() => {
                    say(key, "cannot name a parent import")
                }
                (Some(_), Some(_)) if entry.column.is_none() => {
                    say(key, "needs the column holding the parent's source key")
                }
                (None, Some(reference)) if entry.column.is_some() && reference.lookup.is_none() => {
                    say(
                        key,
                        "needs a parent import to find the record by its source key",
                    )
                }
                _ => {}
            }
        }
        for field in offered.iter().filter(|field| field.required) {
            if !self.fields.iter().any(|entry| entry.field == field.name) {
                say(
                    format!("fields.{}", field.name),
                    "is required: map a column or give a fixed value",
                );
            }
        }
        if let Some(column) = &self.source_key {
            if !known(column) {
                say("source_key".into(), "names a column the file does not have");
            }
        }
        if !matches && self.duplicates == Policy::Update {
            say(
                "duplicates".into(),
                "this record type cannot match existing records",
            );
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

/// A cell's text as a field's value: `None` when blank. Choices match
/// without regard to case; whole numbers, numbers and yes/no are parsed.
pub fn convert(field: &Field, text: &str) -> Result<Option<Value>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let value = match field.kind {
        Kind::Text | Kind::Date if !field.choices.is_empty() => field
            .choices
            .iter()
            .find(|choice| choice.eq_ignore_ascii_case(text))
            .map(|choice| Value::String(choice.clone()))
            .ok_or_else(|| format!("must be one of {}", field.choices.join(", ")))?,
        Kind::Text | Kind::Date => Value::String(text.to_owned()),
        Kind::Integer => text
            .parse::<i64>()
            .map(Value::from)
            .map_err(|_| "must be a whole number".to_owned())?,
        Kind::Number => text
            .parse::<f64>()
            .ok()
            .filter(|number| number.is_finite())
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number)
            .ok_or_else(|| "must be a number".to_owned())?,
        Kind::Boolean => match text.to_ascii_lowercase().as_str() {
            "true" | "yes" | "y" | "1" => Value::Bool(true),
            "false" | "no" | "n" | "0" => Value::Bool(false),
            _ => return Err("must be yes or no".into()),
        },
    };
    Ok(Some(value))
}
