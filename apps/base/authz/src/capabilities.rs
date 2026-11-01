//! Capability catalog read model: every contributed capability key with its
//! parts and contributing module, searched, filtered and sorted in memory.
//! The vocabulary is the deployment's immutable snapshot, so nothing is queried.
use crate::vocabulary::Vocabulary;

pub const LIST: &str = "admin.authz.capability.list";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Sort {
    Key,
    Domain,
    Resource,
    Action,
    Module,
}
impl Sort {
    pub fn parse(key: &str) -> Option<Self> {
        Some(match key {
            "key" => Self::Key,
            "domain" => Self::Domain,
            "resource" => Self::Resource,
            "action" => Self::Action,
            "module" => Self::Module,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug)]
pub struct CapabilitySummary {
    pub key: &'static str,
    pub domain: &'static str,
    pub resource: &'static str,
    pub action: &'static str,
    pub module: String,
}

/// Key parts: the first segment is the domain, the last the action, and
/// anything between the resource. Two-part keys (`domain.action`) have
/// no resource.
pub fn key_parts(key: &str) -> (&str, &str, &str) {
    let (domain, rest) = key.split_once('.').unwrap_or((key, ""));
    match rest.rsplit_once('.') {
        Some((resource, action)) => (domain, resource, action),
        None => (domain, "", rest),
    }
}

/// `base/principal_directory` reads `Base / PrincipalDirectory`.
fn humanize_module(owner: &str) -> String {
    owner
        .split('/')
        .map(|segment| {
            segment
                .split('_')
                .map(|word| {
                    let mut chars = word.chars();
                    chars.next().map_or_else(String::new, |first| {
                        first.to_uppercase().chain(chars).collect()
                    })
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join(" / ")
}

/// Registered domains, sorted and unique: the toolbar's domain choices.
pub fn domains(vocabulary: &Vocabulary) -> Vec<&'static str> {
    let mut domains: Vec<_> = vocabulary
        .capabilities
        .keys()
        .map(|key| key_parts(key).0)
        .collect();
    domains.sort_unstable();
    domains.dedup();
    domains
}

/// Every matching capability in display order. Search matches the key or the
/// module case-insensitively; equal sort values fall back to the key ascending
/// whichever the direction, as the source does.
pub fn list(
    vocabulary: &Vocabulary,
    search: &str,
    domain: &str,
    sort: Sort,
    descending: bool,
) -> Vec<CapabilitySummary> {
    let search = search.trim().to_lowercase();
    let mut items: Vec<_> = vocabulary
        .capabilities
        .iter()
        .map(|(key, owned)| {
            let (domain, resource, action) = key_parts(key);
            CapabilitySummary {
                key,
                domain,
                resource,
                action,
                module: humanize_module(owned.provenance.module),
            }
        })
        .filter(|item| {
            search.is_empty()
                || item.key.to_lowercase().contains(&search)
                || item.module.to_lowercase().contains(&search)
        })
        .filter(|item| domain.is_empty() || item.domain == domain)
        .collect();
    let value = |item: &CapabilitySummary| -> String {
        match sort {
            Sort::Key => item.key.to_lowercase(),
            Sort::Domain => item.domain.to_lowercase(),
            Sort::Resource => item.resource.to_lowercase(),
            Sort::Action => item.action.to_lowercase(),
            Sort::Module => item.module.to_lowercase(),
        }
    };
    items.sort_by(|a, b| {
        let order = value(a).cmp(&value(b));
        let order = if descending { order.reverse() } else { order };
        order.then_with(|| a.key.to_lowercase().cmp(&b.key.to_lowercase()))
    });
    items
}
