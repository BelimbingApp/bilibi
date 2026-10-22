//! Pick-lists, declared by their owners the way step-up operations and
//! capabilities are: one typed contribution each, validated at boot,
//! discovered with no central list. A module that is not mounted declares
//! nothing, so its lists leave the operator page; their stored values stay.
use bilibi_module_registry::{Consumer, Entry, Module, RegistryError, Result};
use std::collections::{BTreeMap, BTreeSet};

pub const LISTS: &str = "base/picklists.lists";
/// More values than this is a sign the list is data to import, not a list
/// to curate; every read loads the whole list.
pub const VALUES_MAX: usize = 500;
pub const CODE_MAX: usize = 64;
pub const LABEL_MAX: usize = 160;
pub const HINT_MAX: usize = 255;

/// Who curates a list's values.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Reach {
    /// Each tenant keeps and edits its own values.
    Tenant,
    /// One set for the installation, kept by the platform operator and
    /// read-only to every other tenant.
    Installation,
}

/// One starting value. Its position is its place in the declaration.
#[derive(Debug)]
pub struct Start {
    pub code: &'static str,
    pub label: &'static str,
    pub hint: Option<&'static str>,
    /// Module code names this value, so the operator may relabel, translate
    /// and reorder it but not retire it.
    pub locked: bool,
    /// In a dependent list, the parent codes this value belongs to.
    pub parents: &'static [&'static str],
}
impl Start {
    pub const fn new(code: &'static str, label: &'static str) -> Self {
        Self {
            code,
            label,
            hint: None,
            locked: false,
            parents: &[],
        }
    }
    pub const fn hint(self, hint: &'static str) -> Self {
        Self {
            hint: Some(hint),
            ..self
        }
    }
    pub const fn locked(self) -> Self {
        Self {
            locked: true,
            ..self
        }
    }
    /// The parent codes (of the list's parent list) this value belongs to.
    pub const fn under(self, parents: &'static [&'static str]) -> Self {
        Self { parents, ..self }
    }
}

/// One list a module offers. The owner chooses the stable key (prefixed by
/// its module id, `/` as `.`) and the starting values; each tenant curates
/// them from there.
#[derive(Debug)]
pub struct PickList {
    pub key: &'static str,
    pub label: &'static str,
    pub reach: Reach,
    pub values: &'static [Start],
    /// The code that pre-fills a new record, until the operator changes it.
    pub default: Option<&'static str>,
    /// The list this one depends on (a sub-category under a category).
    pub parent: Option<&'static str>,
    /// A capability from the owner's vocabulary that managing this list also
    /// needs, beyond `base.picklists.manage`.
    pub capability: Option<&'static str>,
}
impl PickList {
    /// A tenant list with no default, no parent and no extra capability.
    pub const fn new(key: &'static str, label: &'static str, values: &'static [Start]) -> Self {
        Self {
            key,
            label,
            reach: Reach::Tenant,
            values,
            default: None,
            parent: None,
            capability: None,
        }
    }
    pub const fn installation(self) -> Self {
        Self {
            reach: Reach::Installation,
            ..self
        }
    }
    pub const fn default(self, code: &'static str) -> Self {
        Self {
            default: Some(code),
            ..self
        }
    }
    pub const fn under(self, parent: &'static str) -> Self {
        Self {
            parent: Some(parent),
            ..self
        }
    }
    pub const fn requires(self, capability: &'static str) -> Self {
        Self {
            capability: Some(capability),
            ..self
        }
    }
    pub fn start(&self, code: &str) -> Option<&Start> {
        self.values.iter().find(|value| value.code == code)
    }
    /// Whether module code may name `code` (the operator cannot retire it).
    pub fn locked(&self, code: &str) -> bool {
        self.start(code).is_some_and(|value| value.locked)
    }
}

/// Declares one pick-list from module `$module`:
/// `bilibi_picklists::declare!("domain/module", PickList::new("domain.module.category", "Category", &[Start::new("billing", "Billing")]));`
#[macro_export]
macro_rules! declare {
    ($module:literal, $list:expr) => {
        $crate::inventory::submit! {{
            const LIST: $crate::PickList = $list;
            $crate::Contribution::new(
                $crate::LISTS,
                LIST.key,
                $crate::Provenance { module: $module, source: file!(), line: line!() },
                &LIST,
            )
        }}
    };
}

/// Lower-case letters, digits, `_`, `-` and `.`, 1 to `max` characters.
pub(crate) fn code_shaped(code: &str, max: usize) -> bool {
    !code.is_empty()
        && code.len() <= max
        && code
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
}
fn text(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.chars().count() <= max
}

/// Every refusal names the list and the source line that declared it. The
/// capability check is in the service factory: the Authz vocabulary is not
/// ready while consumers validate.
pub(crate) fn validate(entries: &[Entry]) -> Result<()> {
    let mut lists = BTreeMap::new();
    for entry in entries {
        let list = entry.get::<PickList>()?;
        let at = format!(
            "{} at {}:{}",
            entry.id, entry.provenance.source, entry.provenance.line
        );
        let refuse = |reason: String| {
            Err(RegistryError::Contribution(format!(
                "pick-list {at} {reason}"
            )))
        };
        let prefix = format!("{}.", entry.provenance.module.replace('/', "."));
        if !code_shaped(list.key, 128)
            || !list.key.starts_with(&prefix)
            || list.key.len() == prefix.len()
            || entry.id != list.key
        {
            return refuse(format!("must be keyed `{prefix}…` in lower case"));
        }
        if !text(list.label, 120) {
            return refuse("needs a label of 1 to 120 characters".into());
        }
        if list.values.len() > VALUES_MAX {
            return refuse(format!("has more than {VALUES_MAX} values"));
        }
        let mut codes = BTreeSet::new();
        for value in list.values {
            if !code_shaped(value.code, CODE_MAX) {
                return refuse(format!(
                    "has code `{}`: use 1 to {CODE_MAX} lower-case letters, digits, '_', '-' and '.'",
                    value.code
                ));
            }
            if !codes.insert(value.code) {
                return refuse(format!("declares code `{}` twice", value.code));
            }
            if !text(value.label, LABEL_MAX) || value.hint.is_some_and(|hint| !text(hint, HINT_MAX))
            {
                return refuse(format!(
                    "needs a label of 1 to {LABEL_MAX} characters and a hint of at most {HINT_MAX} for `{}`",
                    value.code
                ));
            }
            if list.parent.is_some() == value.parents.is_empty() {
                return refuse(format!(
                    "value `{}` must name parent codes exactly when the list has a parent list",
                    value.code
                ));
            }
        }
        if let Some(default) = list.default.filter(|code| !codes.contains(code)) {
            return refuse(format!(
                "names default `{default}`, which is not a starting value"
            ));
        }
        if list.parent == Some(list.key) {
            return refuse("is its own parent".into());
        }
        if lists.insert(list.key, (entry, list)).is_some() {
            return refuse("is declared twice".into());
        }
    }
    for (entry, list) in lists.values() {
        let Some(parent_key) = list.parent else {
            continue;
        };
        let at = format!(
            "{} at {}:{}",
            entry.id, entry.provenance.source, entry.provenance.line
        );
        let refuse = |reason: String| {
            Err(RegistryError::Contribution(format!(
                "pick-list {at} {reason}"
            )))
        };
        let Some((parent_entry, parent)) = lists.get(parent_key) else {
            return refuse(format!("names unknown parent list {parent_key}"));
        };
        let owner = entry.provenance.module;
        let parent_owner = parent_entry.provenance.module;
        if owner != parent_owner
            && !inventory::iter::<Module>
                .into_iter()
                .any(|m| m.id == owner && m.dependencies.contains(&parent_owner))
        {
            return refuse(format!(
                "names parent list {parent_key} of {parent_owner}, which {owner} does not depend on"
            ));
        }
        if parent.reach != list.reach {
            return refuse(format!("and its parent list {parent_key} differ in reach"));
        }
        for value in list.values {
            if let Some(code) = value
                .parents
                .iter()
                .find(|code| parent.start(code).is_none())
            {
                return refuse(format!(
                    "value `{}` names parent code `{code}`, which is not a starting value of {parent_key}",
                    value.code
                ));
            }
        }
        // Following parents from here must end; a cycle comes back to a list
        // already seen.
        let mut seen = BTreeSet::from([list.key]);
        let mut next = Some(parent_key);
        while let Some(key) = next {
            if !seen.insert(key) {
                return refuse(format!("is part of a parent cycle through {key}"));
            }
            next = lists.get(key).and_then(|(_, list)| list.parent);
        }
    }
    Ok(())
}

inventory::submit! { Consumer::new::<PickList>(LISTS, "base/picklists", validate) }
