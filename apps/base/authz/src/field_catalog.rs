//! The protected-field catalog: which fields of which tables an operator may
//! restrict, as one immutable deployment snapshot.
//!
//! Authz owns this contract but not its content. Grid and record owners submit
//! [`CatalogSource`] contributions to [`FIELD_CATALOG`]; Authz merges fields
//! by table identity and reads the result as plain data, keeping no upward
//! dependency. A source returns only restrictable fields: the owner's
//! `protected` fields, hidden fields and the key, label, time and join fields are
//! left out before Authz sees them. A table with no restrictable field is absent.
//!
//! With no source installed the catalog is empty, so nothing can be restricted.
//! Sources may contribute disjoint fields to the same table; duplicate field
//! identities fail closed. The snapshot is built once, at the first call
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
    /// Other restriction identities that protect this record field. A Company
    /// field can be stored in an Extension table while its audit value is
    /// recorded under the Company field id.
    pub restriction_aliases: Vec<RestrictionAlias>,
}
/// A second table and field identity that holds the same record value. Its
/// target table's audit record types let Authz withhold that value under the
/// owner's audit identity too, even when the target field is protected from
/// restrictions.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct RestrictionAlias {
    pub table: &'static str,
    pub field: &'static str,
    pub record_types: &'static [&'static str],
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
        if entries.is_empty() {
            return Ok(Self::default());
        }
        let mut grouped: std::collections::BTreeMap<&'static str, CatalogTable> =
            std::collections::BTreeMap::new();
        for entry in entries {
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
            for mut table in tables {
                if !valid_id(table.id) || table.label.is_empty() {
                    return Err(format!("field catalog table {:?} is invalid", table.id));
                }
                let mut fields = BTreeSet::new();
                for field in &table.fields {
                    if !valid_id(field.id) || !fields.insert(field.id) || field.label.is_empty() {
                        return Err(format!(
                            "field catalog field {}.{} is invalid or repeated",
                            table.id, field.id
                        ));
                    }
                    let mut aliases = BTreeSet::new();
                    for alias in &field.restriction_aliases {
                        let mut alias_record_types = BTreeSet::new();
                        if !valid_id(alias.table)
                            || !valid_id(alias.field)
                            || !aliases.insert((alias.table, alias.field))
                            || alias.record_types.iter().any(|kind| {
                                kind.is_empty()
                                    || kind.len() > 200
                                    || !alias_record_types.insert(*kind)
                            })
                        {
                            return Err(format!(
                                "field catalog field {}.{} has an invalid or repeated restriction alias",
                                table.id, field.id
                            ));
                        }
                    }
                }
                let target = grouped.entry(table.id).or_insert_with(|| CatalogTable {
                    id: table.id,
                    label: table.label,
                    record_types: table.record_types,
                    fields: Vec::new(),
                });
                if target.label != table.label || target.record_types != table.record_types {
                    return Err(format!(
                        "field catalog sources disagree about table {}",
                        table.id
                    ));
                }
                target.fields.append(&mut table.fields);
            }
        }
        let mut kept = Vec::new();
        for (id, table) in grouped {
            let mut fields = BTreeSet::new();
            for field in &table.fields {
                if !fields.insert(field.id) {
                    return Err(format!(
                        "field catalog field {id}.{} is contributed more than once",
                        field.id
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
            source: Some(entries[0].provenance.module),
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
    Consumer::new::<CatalogSource>(FIELD_CATALOG, "base/authz", |_| Ok(()))
}
inventory::submit! {
    bilibi_module_registry::Ready { module: "base/authz", check: |_| field_catalog().map(|_| ()).map_err(|_| {
        RegistryError::Contribution("the authz field catalog snapshot failed; see the log".into())
    }) }
}
