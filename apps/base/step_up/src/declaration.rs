//! Step-up-capable operations, declared by their owners the way capabilities
//! and menu items are: one typed contribution each, validated at boot,
//! discovered with no central list. A module that is not mounted declares
//! nothing, so its rows leave the policy page.
use bilibi_module_registry::{Consumer, Entry, RegistryError, Result};
use std::collections::BTreeSet;

pub const OPERATIONS: &str = "base/step_up.operations";

/// One operation an owner lets step-up guard. The owner chooses the stable
/// key (prefixed by its module id, `/` as `.`), the label administrators
/// read, and the default; the tenant's policy decides whether it applies.
#[derive(Debug)]
pub struct Operation {
    pub key: &'static str,
    pub label: &'static str,
    /// Required unless the tenant turns it off.
    pub default_on: bool,
    /// Part of the installation floor by default: tenants cannot turn it off
    /// (the installation owner can change the floor).
    pub floor: bool,
}
impl Operation {
    /// On by default, not part of the floor.
    pub const fn new(key: &'static str, label: &'static str) -> Self {
        Self {
            key,
            label,
            default_on: true,
            floor: false,
        }
    }
    pub const fn off_by_default(self) -> Self {
        Self {
            default_on: false,
            ..self
        }
    }
    pub const fn floor(self) -> Self {
        Self {
            floor: true,
            default_on: true,
            ..self
        }
    }
}

/// Declares one step-up-capable operation from module `$module`:
/// `bilibi_step_up::declare!("domain/module", Operation::new("domain.module.thing.approve", "Approve a thing").off_by_default());`
#[macro_export]
macro_rules! declare {
    ($module:literal, $operation:expr) => {
        $crate::inventory::submit! {{
            const OPERATION: $crate::Operation = $operation;
            $crate::Contribution::new(
                $crate::OPERATIONS,
                OPERATION.key,
                $crate::Provenance { module: $module, source: file!(), line: line!() },
                &OPERATION,
            )
        }}
    };
}

pub(crate) fn validate(entries: &[Entry]) -> Result<()> {
    let mut keys = BTreeSet::new();
    for entry in entries {
        let operation = entry.get::<Operation>()?;
        let at = format!(
            "{} at {}:{}",
            entry.id, entry.provenance.source, entry.provenance.line
        );
        let prefix = format!("{}.", entry.provenance.module.replace('/', "."));
        let shaped = operation.key.len() <= 128
            && operation.key.starts_with(&prefix)
            && operation.key.len() > prefix.len()
            && operation
                .key
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b));
        if !shaped || entry.id != operation.key {
            return Err(RegistryError::Contribution(format!(
                "step-up operation {at} must be keyed `{prefix}…` in lower case"
            )));
        }
        let label = operation.label.trim();
        if label.is_empty() || label.chars().count() > 120 {
            return Err(RegistryError::Contribution(format!(
                "step-up operation {at} needs a label of 1 to 120 characters"
            )));
        }
        if !keys.insert(operation.key) {
            return Err(RegistryError::Contribution(format!(
                "step-up operation {at} is declared twice"
            )));
        }
    }
    Ok(())
}

inventory::submit! { Consumer::new::<Operation>(OPERATIONS, "base/step_up", validate) }
