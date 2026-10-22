//! Governed pick-lists: value lists a module declares (`declare!`) and each
//! tenant curates. A module stores the stable code of a value, reads a form's
//! options through [`PickLists::choices`], shows a stored code with
//! [`PickLists::label`] and checks a new one with [`PickLists::check`] in its
//! own transaction; it builds no table, seed or admin page for the list.
//! Values a module's logic branches on stay in code (ADR 05). No Bilimbi
//! counterpart; the rules generalise `core/company` relationship roles.
//! Contract: `README.md`.
mod declaration;
mod seeds;
mod values;

pub use declaration::{PickList, Reach, Start, CODE_MAX, HINT_MAX, LABEL_MAX, LISTS, VALUES_MAX};
pub use seeds::provision;
pub use values::{
    Checked, FieldError, Label, NewValue, PickLists, PickValue, Refusal, ValueUpdate,
};
#[doc(hidden)]
pub use {
    bilibi_module_registry::{Contribution, Provenance},
    inventory,
};

use bilibi_authz::{CapabilityDefinition, VocabularyContribution, VOCABULARY};
use bilibi_module_registry::{
    contributions::{Seed, SeedAction, SeedMode, SEEDS},
    Catalog, Module, RegistryError, Service,
};
use std::{collections::BTreeSet, sync::Arc};

pub const SERVICE: &str = "base/picklists.lists";
/// The production seed service; its one operation is `reconcile`.
pub const SEEDS_SERVICE: &str = "base/picklists.seeds";
/// Read a list's values on the operator page and through agents.
pub const VIEW: &str = "base.picklists.view";
/// Change a tenant list's values.
pub const MANAGE: &str = "base.picklists.manage";
/// Change an installation-wide list; the platform operator's alone.
pub const INSTALLATION_MANAGE: &str = "base.picklists.installation.manage";

macro_rules! provenance {
    () => {
        Provenance {
            module: "base/picklists",
            source: file!(),
            line: line!(),
        }
    };
}

inventory::submit! { Contribution::new(VOCABULARY, "base/picklists", provenance!(), &VocabularyContribution {
    domains: &["base"],
    verbs: &[],
    capabilities: &[
        CapabilityDefinition::tenant(VIEW),
        CapabilityDefinition::tenant(MANAGE),
        CapabilityDefinition::platform(INSTALLATION_MANAGE),
    ],
    roles: &[],
    system_principals: &[],
}) }

/// What boot proves once the vocabulary exists (the service factory, not the
/// consumer validator, which runs while the vocabulary is being built):
/// a list's capability is declared, so the list can be managed; and every
/// module that declares lists contributes a production seed running this
/// module's `reconcile`, so existing tenants receive its starting values.
pub fn validate_installed(
    catalog: &Catalog,
    vocabulary: &bilibi_authz::Vocabulary,
) -> bilibi_module_registry::Result<()> {
    let seeded: BTreeSet<&str> = catalog
        .entries::<Seed>(SEEDS)?
        .iter()
        .filter(|entry| {
            entry.get::<Seed>().is_ok_and(|seed| {
                seed.mode == SeedMode::Production
                    && matches!(
                        seed.action,
                        SeedAction::Service {
                            service: SEEDS_SERVICE,
                            operation: "reconcile"
                        }
                    )
            })
        })
        .map(|entry| entry.provenance.module)
        .collect();
    for entry in catalog.entries::<PickList>(LISTS)? {
        let at = format!(
            "{} at {}:{}",
            entry.id, entry.provenance.source, entry.provenance.line
        );
        if let Some(capability) = entry
            .get::<PickList>()?
            .capability
            .filter(|key| !vocabulary.capabilities.contains_key(key))
        {
            return Err(RegistryError::Contribution(format!(
                "pick-list {at} names undeclared capability {capability}"
            )));
        }
        if !seeded.contains(entry.provenance.module) {
            return Err(RegistryError::Contribution(format!(
                "pick-list {at}: {} contributes no production seed running {SEEDS_SERVICE} reconcile",
                entry.provenance.module
            )));
        }
    }
    Ok(())
}

fn build(services: &bilibi_module_registry::Services) -> bilibi_module_registry::Result<PickLists> {
    // The linked modules' declarations; an unmounted module declares none.
    let catalog = &Catalog::discover(&bilibi_module_registry::discover()?)?;
    validate_installed(
        catalog,
        bilibi_authz::vocabulary()
            .map_err(|_| RegistryError::Service("authz vocabulary unavailable".into()))?,
    )?;
    let mut lists = std::collections::BTreeMap::new();
    for entry in catalog.entries::<PickList>(LISTS)? {
        let list = entry.get::<PickList>()?;
        lists.insert(list.key, list);
    }
    Ok(PickLists {
        lists,
        locales: services.get("base/locale.locales")?,
    })
}

inventory::submit! {
    Module {
        id: "base/picklists", layer: bilibi_module_registry::Layer::Base,
        dependencies: &["base/module_registry", "base/database", "base/tenancy", "base/authz", "base/locale", "base/ui", "base/audit"],
        migrations: &[("base-picklists-001-values", include_str!("../migrations/001-values.sql"))],
        services: &[
            Service { id: SERVICE, factory: |services| Ok(Arc::new(build(services)?)) },
            Service { id: SEEDS_SERVICE, factory: |_| Ok(Arc::new(bilibi_database::seeds::SeedService(Arc::new(seeds::Seeds)))) },
        ],
    }
}
