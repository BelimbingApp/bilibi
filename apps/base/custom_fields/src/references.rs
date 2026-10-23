//! Scoped reference targets supplied by opted-in record owners.
use bilibi_database::Error;
use bilibi_module_registry::{Catalog, Consumer, Entry, RegistryError};
use bilibi_tenancy::{ReadClient, Scope};
use std::{future::Future, pin::Pin};

pub const REFERENCE_TARGETS: &str = "base/custom_fields.reference_targets";
pub type ReferenceFuture<'a, T> =
    Pin<Box<dyn Future<Output = std::result::Result<T, Error>> + Send + 'a>>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReferenceOption {
    pub key: String,
    pub label: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReferenceTargetType {
    pub table: String,
    pub label: String,
}

/// The target owner provides only records the current actor may read. Both
/// methods receive the caller's connection, so validation participates in the
/// owner's mutation transaction. This mirrors Bilimbi's scoped target picker.
pub trait ReferenceTargetProvider: Send + Sync {
    fn choices<'a>(
        &'a self,
        client: &'a dyn ReadClient,
        scope: &'a Scope,
    ) -> ReferenceFuture<'a, Vec<ReferenceOption>>;
    fn resolve<'a>(
        &'a self,
        client: &'a dyn ReadClient,
        scope: &'a Scope,
        key: &'a str,
    ) -> ReferenceFuture<'a, Option<String>>;
}

pub struct ReferenceTargetSource {
    pub table: &'static str,
    pub provider: &'static dyn ReferenceTargetProvider,
}

fn validate(entries: &[Entry]) -> bilibi_module_registry::Result<()> {
    let mut tables = std::collections::BTreeSet::new();
    for entry in entries {
        let source = entry.get::<ReferenceTargetSource>()?;
        if source.table.is_empty()
            || source.table.len() > 64
            || !source
                .table
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
            || !tables.insert(source.table)
        {
            return Err(RegistryError::Contribution(format!(
                "invalid or duplicate reference target table {}",
                source.table
            )));
        }
    }
    Ok(())
}
inventory::submit! { Consumer::new::<ReferenceTargetSource>(REFERENCE_TARGETS, "base/custom_fields", validate) }

pub(crate) fn providers() -> std::result::Result<Vec<ReferenceTargetSourceRef>, RegistryError> {
    let modules = bilibi_module_registry::discover()?;
    let catalog = Catalog::discover(&modules)?;
    let sources = catalog.entries::<ReferenceTargetSource>(REFERENCE_TARGETS)?;
    sources
        .iter()
        .map(|entry| {
            let source = entry.get::<ReferenceTargetSource>()?;
            Ok(ReferenceTargetSourceRef {
                table: source.table,
                provider: source.provider,
            })
        })
        .collect()
}

#[derive(Clone, Copy)]
pub(crate) struct ReferenceTargetSourceRef {
    pub table: &'static str,
    pub provider: &'static dyn ReferenceTargetProvider,
}
