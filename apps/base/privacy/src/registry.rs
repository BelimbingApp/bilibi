//! The personal-data registry's boot check (ADR 05). Stores contribute to
//! `base/personal_data`; this module sits above Grid and refuses a Grid table
//! that marks a field personal when its module answers for no store, so
//! nothing a person asks about is left out quietly.
use bilibi_module_registry::{RegistryError, Result as RegistryResult};
use bilibi_personal_data::Stores;

pub(crate) fn check_marks(stores: &Stores) -> RegistryResult<()> {
    let answering: Vec<&str> = stores.modules().collect();
    for table in bilibi_grid::installed()?.tables() {
        if let Some(field) = table.personal_fields().next() {
            if !answering.contains(&table.owner) {
                return Err(RegistryError::Contribution(format!(
                    "grid table {} from {} ({}:{}) marks field {} personal, but {} contributes no base/personal_data.stores store",
                    table.id(),
                    table.owner,
                    table.provenance.source,
                    table.provenance.line,
                    field.id,
                    table.owner
                )));
            }
        }
    }
    Ok(())
}
