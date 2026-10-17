//! Immutable contributed vocabulary. Grants never create capabilities implicitly.
use bilibi_module_registry::{Consumer, Contribution, Entry, Provenance, RegistryError, Result};
use std::collections::{BTreeMap, BTreeSet};

pub const VOCABULARY: &str = "base/authz.vocabulary";
#[derive(Clone, Copy, Debug)]
pub struct CapabilityDefinition {
    pub key: &'static str,
    pub platform: bool,
}
impl CapabilityDefinition {
    pub const fn tenant(key: &'static str) -> Self {
        Self {
            key,
            platform: false,
        }
    }
    pub const fn platform(key: &'static str) -> Self {
        Self {
            key,
            platform: true,
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct RoleDefinition {
    pub code: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub grant_all: bool,
    pub capabilities: &'static [&'static str],
}
#[derive(Clone, Copy, Debug)]
pub struct SystemPrincipalDefinition {
    pub name: &'static str,
    /// This declaration bounds possible grants; it grants nothing by itself.
    pub capabilities: &'static [&'static str],
}
#[derive(Clone, Copy, Debug, Default)]
pub struct VocabularyContribution {
    pub domains: &'static [&'static str],
    pub verbs: &'static [&'static str],
    pub capabilities: &'static [CapabilityDefinition],
    pub roles: &'static [RoleDefinition],
    pub system_principals: &'static [SystemPrincipalDefinition],
}
#[derive(Clone, Debug)]
pub struct OwnedCapability {
    pub definition: CapabilityDefinition,
    pub provenance: Provenance,
}
#[derive(Clone, Debug)]
pub struct SystemRole {
    pub definition: RoleDefinition,
    /// Multiple modules may extend a named seed role explicitly.
    pub capabilities: BTreeSet<&'static str>,
}
#[derive(Default, Debug)]
pub struct Vocabulary {
    pub domains: BTreeSet<&'static str>,
    pub verbs: BTreeSet<&'static str>,
    pub capabilities: BTreeMap<&'static str, OwnedCapability>,
    pub roles: BTreeMap<&'static str, SystemRole>,
    pub system_principals: BTreeMap<&'static str, SystemPrincipalDefinition>,
}
pub fn segment(key: &str) -> bool {
    let mut chars = key.bytes();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}
pub fn valid_key(key: &str) -> bool {
    key.len() <= 200 && key.split('.').count() >= 2 && key.split('.').all(segment)
}
fn invalid(reason: impl Into<String>) -> RegistryError {
    RegistryError::Contribution(reason.into())
}
impl Vocabulary {
    pub fn from_entries(entries: &[Entry]) -> Result<Self> {
        let mut result = Self::default();
        for entry in entries {
            let v = entry.get::<VocabularyContribution>()?;
            for (values, target) in [
                (v.domains, &mut result.domains),
                (v.verbs, &mut result.verbs),
            ] {
                for value in values {
                    if !segment(value) {
                        return Err(invalid(format!("invalid authz vocabulary {value}")));
                    }
                    target.insert(*value);
                }
            }
            for cap in v.capabilities {
                if !valid_key(cap.key)
                    || result
                        .capabilities
                        .insert(
                            cap.key,
                            OwnedCapability {
                                definition: *cap,
                                provenance: entry.provenance,
                            },
                        )
                        .is_some()
                {
                    return Err(invalid(format!("invalid/duplicate capability {}", cap.key)));
                }
            }
            for role in v.roles {
                if !segment(&role.code.replace('_', "-"))
                    || role.name.is_empty()
                    || role.name.len() > 200
                {
                    return Err(invalid(format!("invalid system role {}", role.code)));
                }
                let existing = result.roles.entry(role.code).or_insert_with(|| SystemRole {
                    definition: *role,
                    capabilities: BTreeSet::new(),
                });
                if existing.definition.name != role.name
                    || existing.definition.grant_all != role.grant_all
                {
                    return Err(invalid(format!("conflicting system role {}", role.code)));
                }
                // Other modules extend built-in roles with capabilities. Their
                // catalog order must not replace Authz's canonical metadata.
                if entry.provenance.module == "base/authz" && entry.id == "authz.builtin" {
                    existing.definition = *role;
                }
                existing
                    .capabilities
                    .extend(role.capabilities.iter().copied());
            }
            for principal in v.system_principals {
                if !segment(principal.name)
                    || principal.name.len() > 100
                    || result
                        .system_principals
                        .insert(principal.name, *principal)
                        .is_some()
                {
                    return Err(invalid(format!(
                        "invalid/duplicate system principal {}",
                        principal.name
                    )));
                }
            }
        }
        for cap in result.capabilities.values() {
            let mut parts = cap.definition.key.split('.');
            if !result.domains.contains(parts.next().unwrap())
                || !result.verbs.contains(parts.next_back().unwrap())
            {
                return Err(invalid(format!(
                    "undeclared domain/verb in {}",
                    cap.definition.key
                )));
            }
        }
        for key in result
            .roles
            .values()
            .flat_map(|r| r.capabilities.iter())
            .chain(
                result
                    .system_principals
                    .values()
                    .flat_map(|p| p.capabilities.iter()),
            )
        {
            if !result.capabilities.contains_key(key) {
                return Err(invalid(format!("undeclared grant {key}")));
            }
        }
        Ok(result)
    }
    pub fn from_catalog(catalog: &bilibi_module_registry::Catalog) -> Result<Self> {
        Self::from_entries(catalog.entries::<VocabularyContribution>(VOCABULARY)?)
    }
    pub fn from_inventory() -> Result<Self> {
        let catalog =
            bilibi_module_registry::Catalog::discover(&bilibi_module_registry::discover()?)?;
        let vocabulary = Self::from_catalog(&catalog)?;
        vocabulary.validate_references(&catalog)?;
        Ok(vocabulary)
    }
    /// Validate capability references without importing their owners. A typo in
    /// navigation/route/settings metadata must fail at boot, not hide a page.
    pub fn validate_references(&self, catalog: &bilibi_module_registry::Catalog) -> Result<()> {
        use bilibi_module_registry::contributions::{Access, Embed, Route, EMBEDS, ROUTES};
        let check = |key: &str| {
            if self.capabilities.contains_key(key) {
                Ok(())
            } else {
                Err(invalid(format!("undeclared capability reference {key}")))
            }
        };
        let access = |value: Access| match value {
            Access::Capability(key) | Access::OperatorCapability(key) => check(key),
            _ => Ok(()),
        };
        for entry in catalog.entries::<Route>(ROUTES)? {
            access(entry.get::<Route>()?.access)?;
        }
        for entry in catalog.entries::<Embed>(EMBEDS)? {
            access(entry.get::<Embed>()?.access)?;
        }
        for entry in catalog.entries::<bilibi_menu::MenuItem>(bilibi_menu::ITEMS)? {
            for key in entry.get::<bilibi_menu::MenuItem>()?.requirement.keys() {
                check(key)?;
            }
        }
        for entry in catalog.entries::<bilibi_settings::Setting>(bilibi_settings::DEFINITIONS)? {
            if let bilibi_settings::Setting::Definition(definition) =
                entry.get::<bilibi_settings::Setting>()?
            {
                if let Some(key) = definition
                    .editor
                    .as_ref()
                    .and_then(|editor| editor.capability)
                {
                    check(key)?;
                }
            }
        }
        Ok(())
    }
}
inventory::submit! { Consumer::new::<VocabularyContribution>(VOCABULARY,"base/authz",|entries| Vocabulary::from_entries(entries).map(|_|())) }

use CapabilityDefinition as C;
const fn role(
    code: &'static str,
    name: &'static str,
    grant_all: bool,
    capabilities: &'static [&'static str],
) -> RoleDefinition {
    RoleDefinition {
        code,
        name,
        description: "Contributed system role; reconciled explicitly",
        grant_all,
        capabilities,
    }
}
inventory::submit! { Contribution::new(VOCABULARY,"authz.builtin",Provenance {module:"base/authz",source:file!(),line:line!()}, &VocabularyContribution {
    domains:&["admin"],
    verbs:&["view","list","create","update","delete","grant","revoke","manage","impersonate"],
    capabilities:&[
        C::tenant("admin.authz.role.list"), C::tenant("admin.authz.role.view"), C::tenant("admin.authz.role.create"), C::tenant("admin.authz.role.update"), C::tenant("admin.authz.role.delete"),
        C::tenant("admin.authz.capability.list"), C::tenant("admin.authz.decision-log.list"),
        C::tenant("admin.authz.system-principal.list"), C::tenant("admin.authz.system-principal.grant"), C::tenant("admin.authz.system-principal.revoke"),
        C::tenant("admin.authz.field.manage"), C::tenant("admin.authz.step-up.manage"), C::tenant("admin.user.impersonate"),
    ],
    roles:&[
        RoleDefinition {
            description: "System role with all capabilities. New capabilities are automatically granted.",
            ..role("core_admin", "Core Administrator", true, &[])
        },
        RoleDefinition {
            description: "Full control within a single tenant: commerce, AI, messaging, company, employees, and addresses. No platform administration.",
            ..role("tenant_owner", "Tenant Owner", false, &["admin.authz.field.manage", "admin.authz.step-up.manage"])
        },
        RoleDefinition {
            description: "Read-only access to decision logs, system logs, and sessions for compliance.",
            ..role("auditor", "Auditor", false, &["admin.authz.decision-log.list"])
        },
        RoleDefinition {
            description: "Read-only access to system infrastructure: tables, jobs, cache, schedule, and sessions.",
            ..role("system_viewer", "System Viewer", false, &[])
        }],
    system_principals:&[],
}) }
