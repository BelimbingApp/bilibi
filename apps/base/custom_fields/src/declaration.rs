//! The record types that offer custom fields, declared by their owners the way
//! pick-lists and capabilities are: one typed contribution each, validated at
//! boot, discovered with no central list. A module that is not mounted declares
//! none, so its record type leaves the page; its definitions stay stored.
use bilibi_module_registry::{Consumer, Entry, RegistryError, Result};
use std::collections::BTreeSet;

pub const RECORD_TYPES: &str = "base/custom_fields.record_types";
pub const RECORD_TYPE_MAX: usize = 32;
pub const LABEL_MAX: usize = 80;

/// One kind of record whose owner keeps tenant-defined values in its own table
/// (`custom_fields jsonb NOT NULL DEFAULT '{}'`) and checks them against the
/// tenant's definitions inside its own write.
#[derive(Debug)]
pub struct RecordType {
    /// Lower-case letters, digits and `_`, such as `company`; stored with each
    /// definition, so it never changes.
    pub id: &'static str,
    /// What the page calls the records: "Companies".
    pub label: &'static str,
}

pub(crate) fn shaped(value: &str, max: usize) -> bool {
    let mut bytes = value.bytes();
    value.len() <= max
        && bytes.next().is_some_and(|byte| byte.is_ascii_lowercase())
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn validate(entries: &[Entry]) -> Result<()> {
    let mut seen = BTreeSet::new();
    for entry in entries {
        let record = entry.get::<RecordType>()?;
        let at = format!(
            "custom-field record type {} at {}:{}",
            entry.id, entry.provenance.source, entry.provenance.line
        );
        if entry.id != record.id
            || !shaped(record.id, RECORD_TYPE_MAX)
            || record.label.trim().is_empty()
            || record.label.len() > LABEL_MAX
        {
            return Err(RegistryError::Contribution(format!(
                "{at}: the id must match the contribution id and be lower-case letters, digits and '_' (at most {RECORD_TYPE_MAX}), with a label"
            )));
        }
        if !seen.insert(record.id) {
            return Err(RegistryError::Contribution(format!(
                "{at}: record type declared twice"
            )));
        }
    }
    Ok(())
}
inventory::submit! { Consumer::new::<RecordType>(RECORD_TYPES, "base/custom_fields", validate) }
