//! Grid table/field/link catalog, scoped paths, bounded page projections, the
//! lenses that turn them into FlexTable cells, and what each account arranged
//! on each list page. There is no Grid page: a list hosts the walked columns
//! through `PageColumns`; see `README.md`.
pub mod catalog;
pub mod column;
pub mod lens;
pub mod model;
pub mod page_columns;
pub mod page_views;
pub mod query;
pub mod reference;
pub mod view;
pub use lens::{Lens, Range};
pub use page_columns::{Outcome, PageColumns};
pub use page_views::{PageViews, PAGE_VIEWS};
pub use query::{project, project_lenses, stats, Lenses, Projection, QueryError, Row, Stats};
pub use view::View;

use bilibi_module_registry::{Consumer, Layer, Module, RegistryError, Result, Service};
use std::sync::OnceLock;

pub use catalog::{
    restrictable, restriction_catalog, Catalog, CatalogError, Restrictable, MAX_COLUMNS,
};
pub use column::{Agg, Column, ColumnError, ColumnKind};
pub use model::{
    Field, FieldType, GridContribution, Installed, Join, Link, LinkDecl, LinkKind, PersonalKind,
    RecordFields, Source, Table, TableDecl,
};

#[doc(hidden)]
pub use bilibi_module_registry::{Contribution, Provenance};
#[doc(hidden)]
pub use inventory;

/// The registry consumer every module's `GridContribution` is submitted to.
pub const TABLES: &str = "base/grid.tables";

/// Contribute tables and links from the owning module:
/// `bilibi_grid::contribute!("core/x", "x.grid", &GRID)` with
/// `static GRID: GridContribution`.
#[macro_export]
macro_rules! contribute {
    ($module:literal, $id:literal, $payload:expr) => {
        $crate::inventory::submit! {
            $crate::Contribution::new(
                $crate::TABLES,
                $id,
                $crate::Provenance { module: $module, source: file!(), line: line!() },
                $payload,
            )
        }
    };
}

/// The immutable deployment snapshot, validated once. Grants and restrictions are
/// never part of it.
pub fn installed() -> Result<&'static Installed> {
    static SNAPSHOT: OnceLock<Result<Installed>> = OnceLock::new();
    SNAPSHOT
        .get_or_init(|| {
            let installed = Installed::from_inventory()?;
            validate_capabilities(
                &installed,
                bilibi_authz::vocabulary().map_err(|_| {
                    RegistryError::Contribution("authz vocabulary unavailable".into())
                })?,
            )?;
            Ok(installed)
        })
        .as_ref()
        .map_err(|error| RegistryError::Contribution(error.to_string()))
}

/// A typo in a table's capability must fail at boot, not hide the table forever.
pub fn validate_capabilities(
    installed: &Installed,
    vocabulary: &bilibi_authz::Vocabulary,
) -> Result<()> {
    for table in installed.tables() {
        if !vocabulary.capabilities.contains_key(table.capability()) {
            return Err(RegistryError::Contribution(format!(
                "grid table {} from {} ({}:{}) names undeclared capability {}",
                table.id(),
                table.owner,
                table.provenance.source,
                table.provenance.line,
                table.capability()
            )));
        }
    }
    Ok(())
}

/// Grid supplies Authz's protected-field catalog (`bilibi_authz::field_catalog`)
/// as data: the restrictable fields of every installed table.
fn restriction_snapshot(
) -> std::result::Result<Vec<bilibi_authz::field_catalog::CatalogTable>, String> {
    use bilibi_authz::field_catalog::{CatalogField, CatalogTable};
    Ok(
        restriction_catalog(installed().map_err(|error| error.to_string())?)
            .into_iter()
            .map(|entry| CatalogTable {
                id: entry.table,
                label: entry.label,
                record_types: entry.record_types,
                fields: entry
                    .fields
                    .into_iter()
                    .map(|(id, label, personal)| CatalogField {
                        id,
                        label,
                        personal,
                    })
                    .collect(),
            })
            .collect(),
    )
}
inventory::submit! {
    Contribution::new(bilibi_authz::field_catalog::FIELD_CATALOG, "base/grid.restrictable",
        Provenance { module: "base/grid", source: file!(), line: line!() },
        &bilibi_authz::field_catalog::CatalogSource { snapshot: restriction_snapshot })
}

inventory::submit! { Consumer::new::<GridContribution>(TABLES, "base/grid", |entries| Installed::from_entries(entries).map(|_| ())) }
inventory::submit! {
    Module {
        id: "base/grid", layer: Layer::Base,
        dependencies: &["base/module_registry", "base/database", "base/tenancy", "base/authz", "base/ui", "base/datetime", "base/settings"],
        migrations: &[
            ("base-grid-001", include_str!("../migrations/001-reference.sql")),
            ("base-grid-002", include_str!("../migrations/002-reference-recorded-at.sql")),
        ],
        services: &[Service { id: PAGE_VIEWS, factory: page_views::service }],
    }
}
inventory::submit! {
    bilibi_module_registry::Ready { module: "base/grid", check: |_| installed().map(|_| ()) }
}
