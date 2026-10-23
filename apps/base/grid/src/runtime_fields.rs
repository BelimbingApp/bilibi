//! Runtime field metadata owned by opted-in record owners (Bilimbi
//! `CustomFields` Grid catalog seam). Providers return data only; Grid remains
//! responsible for scope visibility and query construction.
use bilibi_database::{Error, Result};
use bilibi_module_registry::{Catalog, Consumer, Entry, RegistryError};
use bilibi_ports::PortClient;
use bilibi_tenancy::Scope;
use std::{future::Future, pin::Pin, sync::OnceLock};

pub const RUNTIME_FIELDS: &str = "base/grid.runtime_fields";
pub type RuntimeFuture<'a> = Pin<Box<dyn Future<Output = Result<Vec<RuntimeField>>> + Send + 'a>>;

#[derive(Clone, Debug)]
pub struct RuntimeField {
    pub table: String,
    pub column: &'static str,
    pub key: String,
    pub label: String,
    pub ty: crate::FieldType,
    pub values: Vec<String>,
    /// For reference values, only these target keys may leave the projection.
    pub readable_values: Option<Vec<String>>,
}

pub trait RuntimeFieldProvider: Send + Sync {
    fn fields<'a>(&'a self, client: PortClient<'a>, scope: &'a Scope) -> RuntimeFuture<'a>;
}

pub struct RuntimeFieldSource {
    pub provider: &'static dyn RuntimeFieldProvider,
}

fn validate(entries: &[Entry]) -> bilibi_module_registry::Result<()> {
    for entry in entries {
        entry.get::<RuntimeFieldSource>()?;
    }
    if entries.len() > 1 {
        return Err(RegistryError::Contribution(
            "Grid has more than one runtime field provider".into(),
        ));
    }
    Ok(())
}
inventory::submit! { Consumer::new::<RuntimeFieldSource>(RUNTIME_FIELDS, "base/grid", validate) }

pub(crate) fn provider() -> Result<Option<&'static dyn RuntimeFieldProvider>> {
    static PROVIDER: OnceLock<
        std::result::Result<Option<&'static dyn RuntimeFieldProvider>, String>,
    > = OnceLock::new();
    match PROVIDER.get_or_init(|| {
        let modules = bilibi_module_registry::discover().map_err(|error| error.to_string())?;
        let catalog = Catalog::discover(&modules).map_err(|error| error.to_string())?;
        let entries = catalog
            .entries::<RuntimeFieldSource>(RUNTIME_FIELDS)
            .map_err(|error| error.to_string())?;
        entries
            .first()
            .map(|entry| {
                entry
                    .get::<RuntimeFieldSource>()
                    .map(|source| source.provider)
                    .map_err(|error| error.to_string())
            })
            .transpose()
    }) {
        Ok(provider) => Ok(*provider),
        Err(_) => Err(Error::Internal),
    }
}
