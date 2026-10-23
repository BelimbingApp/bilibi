//! The protected-field catalog: which fields of which tables an operator may
//! restrict, as one immutable deployment snapshot.
//!
//! Authz owns this contract but not its content. Grid owns tables and fields and
//! depends on Authz, so Grid contributes one [`CatalogSource`] to
//! [`FIELD_CATALOG`] and Authz reads the result as plain data, keeping no Authz →
//! Grid dependency. A source returns only restrictable fields: the owner's
//! `protected` fields, hidden fields and the key, label, time and join fields are
//! left out before Authz sees them. A table with no restrictable field is absent.
//!
//! With no source installed the catalog is empty, so nothing can be restricted.
//! Two sources fail discovery. The snapshot is built once, at the first call
//! (the `base/authz` Ready check makes that boot); never call it from a
//! contribution validator, since a source may read another consumer's snapshot.
use bilibi_database::{Error, Result};
use bilibi_module_registry::{Consumer, Entry, RegistryError};
use std::{collections::BTreeSet, sync::OnceLock};

/// The registry consumer the field owner submits its [`CatalogSource`] to.
pub const FIELD_CATALOG: &str = "base/authz.field_catalog";

/// One restrictable field, by the id restrictions store and its display label.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogField {
    pub id: &'static str,
    pub label: &'static str,
    /// The field's personal kind (`name`, `email`, `phone`, `identifier`,
    /// `date`, `free_text`) when its owner marks it personal. Only such a
    /// field may be masked, since the kind shapes the partial value.
    pub personal: Option<&'static str>,
}
/// One table with the fields an operator may restrict. `record_types` are the
/// audit record types its rows use, so a restriction reaches their audit views.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogTable {
    pub id: &'static str,
    pub label: &'static str,
    pub record_types: &'static [&'static str],
    pub fields: Vec<CatalogField>,
}
/// The contribution payload. `snapshot` runs once per process, after discovery.
pub struct CatalogSource {
    pub snapshot: fn() -> std::result::Result<Vec<CatalogTable>, String>,
}

/// The validated snapshot: tables ordered by label, then id; fields in the
/// order their owner declared them.
#[derive(Debug, Default)]
pub struct FieldCatalog {
    tables: Vec<CatalogTable>,
    source: Option<&'static str>,
}
impl FieldCatalog {
    pub fn tables(&self) -> &[CatalogTable] {
        &self.tables
    }
    pub fn table(&self, id: &str) -> Option<&CatalogTable> {
        self.tables.iter().find(|table| table.id == id)
    }
    pub fn field(&self, table: &str, field: &str) -> Option<(&CatalogTable, &CatalogField)> {
        let table = self.table(table)?;
        table
            .fields
            .iter()
            .find(|f| f.id == field)
            .map(|f| (table, f))
    }
    /// Whether a restriction of `field` of `table` may be written now.
    pub fn restrictable(&self, table: &str, field: &str) -> bool {
        self.field(table, field).is_some()
    }
    /// The module that supplied the snapshot, for diagnostics.
    pub fn source(&self) -> Option<&'static str> {
        self.source
    }

    fn build(entries: &[Entry]) -> std::result::Result<Self, String> {
        let Some(entry) = entries.first() else {
            return Ok(Self::default());
        };
        let tables = (entry
            .get::<CatalogSource>()
            .map_err(|e| e.to_string())?
            .snapshot)()
        .map_err(|error| {
            format!(
                "field catalog from {} ({}:{}): {error}",
                entry.provenance.module, entry.provenance.source, entry.provenance.line
            )
        })?;
        let mut ids = BTreeSet::new();
        let mut kept = Vec::new();
        for table in tables {
            if !valid_id(table.id) || !ids.insert(table.id) || table.label.is_empty() {
                return Err(format!(
                    "field catalog table {:?} is invalid or repeated",
                    table.id
                ));
            }
            let mut fields = BTreeSet::new();
            for field in &table.fields {
                if !valid_id(field.id) || !fields.insert(field.id) || field.label.is_empty() {
                    return Err(format!(
                        "field catalog field {}.{} is invalid or repeated",
                        table.id, field.id
                    ));
                }
            }
            if !table.fields.is_empty() {
                kept.push(table);
            }
        }
        kept.sort_by(|a, b| a.label.cmp(b.label).then(a.id.cmp(b.id)));
        Ok(Self {
            tables: kept,
            source: Some(entry.provenance.module),
        })
    }
}

/// Restrictions store ids of at most 200 bytes (`field_restrictions` checks).
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 200
}

/// The installed snapshot. An invalid source is a deployment defect: it fails
/// the Ready check at boot, and every later call returns `Error::Internal`.
pub fn field_catalog() -> Result<&'static FieldCatalog> {
    static SNAPSHOT: OnceLock<std::result::Result<FieldCatalog, String>> = OnceLock::new();
    SNAPSHOT
        .get_or_init(|| {
            let modules = bilibi_module_registry::discover().map_err(|e| e.to_string())?;
            let catalog =
                bilibi_module_registry::Catalog::discover(&modules).map_err(|e| e.to_string())?;
            FieldCatalog::build(
                catalog
                    .entries::<CatalogSource>(FIELD_CATALOG)
                    .map_err(|e| e.to_string())?,
            )
        })
        .as_ref()
        .map_err(|error| {
            eprintln!("authz field catalog unavailable: {error}");
            Error::Internal
        })
}

inventory::submit! {
    Consumer::new::<CatalogSource>(FIELD_CATALOG, "base/authz", |entries| match entries {
        [_, second, ..] => Err(RegistryError::Contribution(format!(
            "a second field catalog source {} from {} ({}:{}); exactly one module owns restrictable fields",
            second.id, second.provenance.module, second.provenance.source, second.provenance.line
        ))),
        _ => Ok(()),
    })
}
inventory::submit! {
    bilibi_module_registry::Ready { module: "base/authz", check: |_| field_catalog().map(|_| ()).map_err(|_| {
        RegistryError::Contribution("the authz field catalog snapshot failed; see the log".into())
    }) }
}
