//! Global geographic reference data: countries, first-level divisions,
//! postcodes and cities. A fresh install creates the tables and no rows; every
//! lookup answers empty until an operator imports. See `README.md`.
mod contributions;
mod corrections;
mod download;
mod importer;
mod lookup;
mod model;
mod names;
mod query;
mod reference_data;
mod web;

pub use contributions::{LIST, UPDATE, VIEW};
pub use lookup::{country_known, normalize_iso, Geonames};
pub use model::*;
pub use query::*;

pub use contributions::{ADMIN1_PATH, COUNTRIES_PATH, POSTCODES_PATH};
pub use corrections::{FieldError, PostcodeDraft, PostcodeWriteError, Provenance};
pub use download::{DownloadFailure, DownloadStatus, FallbackCause};
pub use importer::ImportFailure;
pub use names::NameWriteError;
pub use reference_data::{
    default_cache_dir, Dataset, DatasetReport, ImportError, ImportOptions, ReferenceImport, Target,
    DEFAULT_BASE_URL, DEFAULT_POSTCODES_URL, IMPORT_SERVICE, USAGE,
};

use bilibi_database::{Db, MutationBoundary};
use bilibi_integration::Integration;
use bilibi_module_registry::{Layer, Module, Service};
use std::sync::Arc;

/// The registry service id of the lookup API.
pub const SERVICE: &str = "core/geonames.lookup";
/// The registry service id of the three index screens.
pub const PAGES_SERVICE: &str = "core/geonames.pages";
pub const MIGRATION: (&str, &str) = (
    "core-geonames-001",
    include_str!("../migrations/001-geonames.sql"),
);
pub const MIGRATION_OVERRIDES: (&str, &str) = (
    "core-geonames-002",
    include_str!("../migrations/002-postcode-overrides.sql"),
);

inventory::submit! {
    Module {
        id: "core/geonames", layer: Layer::Core,
        dependencies: &["base/module_registry", "base/database", "base/authz", "base/grid", "base/integration", "base/tenancy", "base/ui", "base/menu", "base/datetime", "base/locale"],
        migrations: &[MIGRATION, MIGRATION_OVERRIDES],
        services: &[
            Service { id: SERVICE, factory: |services| {
                Ok(Arc::new(Geonames::new(
                    services.get::<Db>("base/database.pool")?.as_ref().clone(),
                    services.get::<MutationBoundary>("base/database.mutations")?,
                )))
            } },
            Service { id: IMPORT_SERVICE, factory: |services| {
                Ok(Arc::new(ReferenceImport::new(
                    services.get::<Db>("base/database.pool")?.as_ref().clone(),
                    services.get::<Integration>(bilibi_integration::SERVICE)?,
                )))
            } },
            Service { id: PAGES_SERVICE, factory: |services| {
                Ok(Arc::new(bilibi_ui::page::PageService::<bilibi_tenancy::Scope, bilibi_database::Error>(Arc::new(web::Pages {
                    db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
                    geonames: services.get(SERVICE)?,
                    importer: services.get(IMPORT_SERVICE)?,
                    display: services.get("base/datetime.display")?,
                    locales: services.get("base/locale.locales")?,
                }))))
            } },
        ],
    }
}

// ADR 01 gap, listed so it stays visible: these pages still write through
// the facade rather than an operation contract (`base/agent_api` page).
inventory::submit! { bilibi_module_registry::Contribution::new(bilibi_module_registry::contributions::PAGE_WRITES, "core/geonames.pages", bilibi_module_registry::Provenance { module: "core/geonames", source: file!(), line: line!() }, &bilibi_module_registry::contributions::PageWrites {
    service: "core/geonames.pages",
    contracts: &[],
    pending: bilibi_module_registry::contributions::Pending::Every { reason: "Postcode corrections write through the Geonames facade; no contract yet." },
}) }
