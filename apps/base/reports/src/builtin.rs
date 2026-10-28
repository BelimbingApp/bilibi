//! Built-in reports: a module contributes a report in code, the way it
//! contributes a dashboard panel. It is read-only and copyable, runs as each
//! reader like any report, and is offered only to a reader who can read its
//! record type. Nothing is stored or seeded. No Bilimbi counterpart.
use crate::{Definition, Kind};
use bilibi_module_registry::{Consumer, Entry, RegistryError};
use serde_json::Value;
use std::collections::BTreeSet;

pub const BUILTINS: &str = "base/reports.builtins";

/// One built-in report. `key` is its durable address
/// (`/built-in-reports/<key>`); keep it stable when the name changes.
/// `definition` is the agent dialect `base.reports.create` takes.
#[derive(Clone, Copy)]
pub struct Builtin {
    pub key: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    /// A Grid table id.
    pub record_type: &'static str,
    pub kind: Kind,
    pub definition: fn() -> Value,
}

/// Contribute a built-in report from the owning module:
/// `bilibi_reports::builtin!("core/x", Builtin { key: "x-by-status", .. })`.
#[macro_export]
macro_rules! builtin {
    ($module:literal, $builtin:expr) => {
        $crate::inventory::submit! {{
            const BUILTIN: $crate::Builtin = $builtin;
            $crate::Contribution::new($crate::BUILTINS, BUILTIN.key,
                $crate::Provenance { module: $module, source: file!(), line: line!() }, &BUILTIN)
        }}
    };
}

fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 64
        && key
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// The contributed built-ins, proven: a valid unique key, a name, and a
/// definition that parses and keeps the bounds of its kind. Whether its
/// columns exist is the reader's catalog's answer at every run.
pub(crate) fn validate(entries: &[Entry]) -> bilibi_module_registry::Result<Vec<&'static Builtin>> {
    let mut keys = BTreeSet::new();
    let mut builtins = Vec::new();
    for entry in entries {
        let builtin = entry.get::<Builtin>()?;
        let at = format!("{}:{}", entry.provenance.source, entry.provenance.line);
        let parsed = Definition::from_input(&(builtin.definition)())
            .ok()
            .filter(|d| d.check(builtin.kind).is_ok());
        let name = builtin.name.trim();
        if builtin.key != entry.id
            || !valid_key(builtin.key)
            || name.is_empty()
            || name.chars().count() > 120
            || builtin.description.chars().count() > 1000
            || builtin.record_type.is_empty()
            || parsed.is_none()
            || !keys.insert(builtin.key)
        {
            return Err(RegistryError::Contribution(format!(
                "invalid/duplicate built-in report {} at {at}",
                entry.id
            )));
        }
        builtins.push(builtin);
    }
    builtins.sort_by_key(|b| b.name);
    Ok(builtins)
}
inventory::submit! { Consumer::new::<Builtin>(BUILTINS, "base/reports", |entries| validate(entries).map(|_| ())) }
