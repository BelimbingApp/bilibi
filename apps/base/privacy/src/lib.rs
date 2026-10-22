//! Personal data (ADR 05): one registry in which every module that keeps a
//! person's data answers for it. Base never reads another module's tables; it
//! asks each store's handler. Legal holds, data-subject requests, retention
//! and masking build on this registry. No Bilimbi counterpart: Bilimbi keeps
//! per-store retention workers only.
mod registry;
pub use registry::{
    Action, Deferral, Erased, Exported, FileRef, Handler, HandlerFuture, Identifier, Kept,
    KeptReason, KindAction, KindCount, Located, PersonalData, Subject, ERASED, PERSONAL_DATA,
};

use bilibi_module_registry::{
    Catalog, Consumer, Layer, Module, Ready, Result as RegistryResult, Service,
};
use std::sync::Arc;

/// The `Privacy` service, after declaring `base/privacy`.
pub const SERVICE: &str = "base/privacy.facade";

/// The mounted personal-data stores.
pub struct Privacy {
    stores: registry::Stores,
}
impl Privacy {
    /// Every mounted store key with its label, sorted by key.
    pub fn stores(&self) -> impl Iterator<Item = (&'static str, &'static str)> + '_ {
        self.stores
            .by_key
            .iter()
            .map(|(key, (_, data))| (*key, data.label))
    }
}

inventory::submit! { Consumer::new::<PersonalData>(PERSONAL_DATA, "base/privacy", registry::validate) }

fn build() -> RegistryResult<Privacy> {
    let modules = bilibi_module_registry::discover()?;
    let catalog = Catalog::discover(&modules)?;
    Ok(Privacy {
        stores: registry::Stores::from_catalog(&catalog, &modules)?,
    })
}

inventory::submit! {
    Module {
        id: "base/privacy", layer: Layer::Base,
        dependencies: &["base/module_registry", "base/database", "base/tenancy", "base/grid", "base/party_ids"],
        migrations: &[],
        services: &[Service { id: SERVICE, factory: |_| Ok(Arc::new(build()?)) }],
    }
}
inventory::submit! {
    Ready { module: "base/privacy", check: |services| {
        let privacy = services.get::<Privacy>(SERVICE)?;
        registry::check_marks(&privacy.stores)
    } }
}
