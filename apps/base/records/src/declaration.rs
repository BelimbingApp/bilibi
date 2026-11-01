//! Record kinds and reference holders, declared by their owners the way
//! capabilities and pick-lists are: one typed contribution each, validated at
//! boot, discovered with no central list. A module that is not mounted
//! declares nothing, so its kind's references show as unavailable.
use bilibi_module_registry::{Consumer, Entry, Module, RegistryError, Result};
use bilibi_tenancy::{ProofClient, ProofFuture, Scope};
use std::collections::BTreeSet;

pub const KINDS: &str = "base/records.kinds";
pub const HOLDERS: &str = "base/records.holders";

/// What an owner answers for one record and one reader.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Access {
    /// A live record of the reader's tenant that the reader may view.
    Readable,
    /// No such record for this reader: missing, removed or another tenant's.
    NotFound,
    /// The record exists but the reader may not view it.
    Forbidden,
}

/// Implement on the owner's own type. Load the live record with the caller's
/// sealed scope and check the owner's own view capability; an id is a
/// request, never proof. Like Media's `SubjectOwner::authorize`.
pub trait RecordOwner: Send + Sync {
    fn authorize<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        scope: &'a Scope,
        id: &'a str,
    ) -> ProofFuture<'a, Access>;
}

/// One kind of record an owner lets other modules point at.
pub struct RecordKind {
    /// The audit kind the owner already writes in `Change.kind` (`person`,
    /// `company`, `finance.receivables.document`).
    pub key: &'static str,
    /// The owner's Grid table; it must claim `key` in `record_types` and
    /// name a `label_field`.
    pub table: &'static str,
    /// The record's page, a contributed GET page route with one `{id}`.
    pub path: Option<&'static str>,
    pub owner: &'static dyn RecordOwner,
}
impl RecordKind {
    pub const fn new(
        key: &'static str,
        table: &'static str,
        owner: &'static dyn RecordOwner,
    ) -> Self {
        Self {
            key,
            table,
            path: None,
            owner,
        }
    }
    /// The record's page, such as `/contacts/{id}`.
    pub const fn page(self, path: &'static str) -> Self {
        Self {
            path: Some(path),
            ..self
        }
    }
    /// The page of one record, when the kind has one.
    pub fn path_of(&self, id: &str) -> Option<String> {
        self.path.map(|path| path.replace("{id}", id))
    }
}

/// A holder's table of references: its Grid table and the two text fields
/// holding `(record_kind, record_id)`. `referrers` counts through it.
pub struct Holder {
    pub table: &'static str,
    pub kind_field: &'static str,
    pub id_field: &'static str,
}

/// Declares one record kind from module `$module`:
/// `bilibi_records::declare!("core/person", RecordKind::new("person", "persons", &OWNER).page("/contacts/{id}"));`
#[macro_export]
macro_rules! declare {
    ($module:literal, $kind:expr) => {
        $crate::inventory::submit! {{
            const KIND: $crate::RecordKind = $kind;
            $crate::Contribution::new(
                $crate::KINDS,
                KIND.key,
                $crate::Provenance { module: $module, source: file!(), line: line!() },
                &KIND,
            )
        }}
    };
}

/// Declares one table of references from module `$module`:
/// `bilibi_records::holder!("domain/notes", Holder { table: "notes", kind_field: "record_kind", id_field: "record_id" });`
#[macro_export]
macro_rules! holder {
    ($module:literal, $holder:expr) => {
        $crate::inventory::submit! {{
            const HOLDER: $crate::Holder = $holder;
            $crate::Contribution::new(
                $crate::HOLDERS,
                HOLDER.table,
                $crate::Provenance { module: $module, source: file!(), line: line!() },
                &HOLDER,
            )
        }}
    };
}

/// A kind key: lower-case dotted words of letters, digits, `_` and `-`.
pub(crate) fn key_shaped(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 100
        && key.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-".contains(&b))
        })
}

fn at(entry: &Entry) -> String {
    format!(
        "{} at {}:{}",
        entry.id, entry.provenance.source, entry.provenance.line
    )
}

/// The contributor must depend on `base/records`, so a stray registration
/// cannot borrow its trust.
pub(crate) fn depends(entry: &Entry) -> bool {
    entry.provenance.module == "base/records"
        || inventory::iter::<Module>
            .into_iter()
            .any(|m| m.id == entry.provenance.module && m.dependencies.contains(&"base/records"))
}

/// The checks that need nothing but the declarations. The Grid table, its
/// `record_types` and `label_field`, and the page route are checked by the
/// service factory, where those catalogs are ready.
fn validate_kinds(entries: &[Entry]) -> Result<()> {
    let mut keys = BTreeSet::new();
    for entry in entries {
        let kind = entry.get::<RecordKind>()?;
        let refuse = |reason: String| {
            Err(RegistryError::Contribution(format!(
                "record kind {} {reason}",
                at(entry)
            )))
        };
        if !key_shaped(kind.key) || entry.id != kind.key {
            return refuse("must be keyed by its audit kind, in lower case".into());
        }
        if !keys.insert(kind.key) {
            return refuse("is registered twice".into());
        }
        if !depends(entry) {
            return refuse(format!(
                "is contributed by {}, which does not depend on base/records",
                entry.provenance.module
            ));
        }
        if let Some(path) = kind.path {
            if !path.starts_with('/') || path.matches("{id}").count() != 1 || path.contains("?") {
                return refuse(format!("has page path {path}: it needs exactly one {{id}}"));
            }
        }
    }
    Ok(())
}

fn validate_holders(entries: &[Entry]) -> Result<()> {
    for entry in entries {
        let holder = entry.get::<Holder>()?;
        if entry.id != holder.table || holder.kind_field == holder.id_field {
            return Err(RegistryError::Contribution(format!(
                "record holder {} must be keyed by its Grid table and name two fields",
                at(entry)
            )));
        }
        if !depends(entry) {
            return Err(RegistryError::Contribution(format!(
                "record holder {} is contributed by {}, which does not depend on base/records",
                at(entry),
                entry.provenance.module
            )));
        }
    }
    Ok(())
}

inventory::submit! { Consumer::new::<RecordKind>(KINDS, "base/records", validate_kinds) }
inventory::submit! { Consumer::new::<Holder>(HOLDERS, "base/records", validate_holders) }
