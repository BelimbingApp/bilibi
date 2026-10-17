//! Boot checks. An event name is its publisher's module ID, a dot and a
//! snake_case name (`supply_chain/receiving.goods_received`); its container is
//! the module ID's first segment. A subscriber reads only events of other
//! containers: within one container a module calls its neighbour directly.
use crate::{EventType, Subscriber, EVENTS, SUBSCRIBERS};
use bilibi_module_registry::{Catalog, Consumer, Entry, Module, RegistryError, Result};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) type Types = BTreeMap<&'static str, (&'static str, u16)>;
pub(crate) type Subscribers = BTreeMap<&'static str, &'static Subscriber>;

fn snake(segment: &str) -> bool {
    !segment.is_empty()
        && segment.len() <= 64
        && segment.starts_with(|c: char| c.is_ascii_lowercase())
        && segment
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}
/// `(module ID, container)` of a well-formed `<container>/<module>.<name>`.
fn named(name: &str) -> Option<(&str, &str)> {
    let (module, event) = name.split_once('.')?;
    let (container, rest) = module.split_once('/')?;
    (snake(container) && snake(rest) && snake(event)).then_some((module, container))
}
fn container(module: &str) -> &str {
    module
        .split_once('/')
        .map_or(module, |(container, _)| container)
}
fn refuse(entry: &Entry, reason: String) -> RegistryError {
    RegistryError::Contribution(format!(
        "{} at {}:{} {reason}",
        entry.id, entry.provenance.source, entry.provenance.line
    ))
}

fn validate_events(entries: &[Entry]) -> Result<()> {
    for entry in entries {
        let event = entry.get::<EventType>()?;
        if named(entry.id).map(|(module, _)| module) != Some(entry.provenance.module) {
            return Err(refuse(
                entry,
                format!(
                    "must be named {}.<snake_case name>",
                    entry.provenance.module
                ),
            ));
        }
        if event.version == 0 {
            return Err(refuse(entry, "must declare a version from 1".into()));
        }
    }
    Ok(())
}
fn validate_subscribers(entries: &[Entry]) -> Result<()> {
    for entry in entries {
        let subscriber = entry.get::<Subscriber>()?;
        let own = entry.provenance.module;
        if entry
            .id
            .strip_prefix(own)
            .and_then(|rest| rest.strip_prefix('.'))
            .is_none_or(|name| !snake(name))
        {
            return Err(refuse(
                entry,
                format!("must be named {own}.<snake_case name>"),
            ));
        }
        if subscriber.events.is_empty() {
            return Err(refuse(entry, "subscribes to no event".into()));
        }
        let mut seen = BTreeSet::new();
        for on in subscriber.events {
            let Some((_, publisher)) = named(on.event) else {
                return Err(refuse(entry, format!("subscribes to {}, which is not an event name (<container>/<module>.<snake_case name>)", on.event)));
            };
            if publisher == container(own) {
                return Err(refuse(entry, format!("subscribes to {} in its own container {publisher}; call that module directly instead", on.event)));
            }
            if on.versions.is_empty() || on.versions.contains(&0) || !seen.insert(on.event) {
                return Err(refuse(
                    entry,
                    format!("lists {} twice or without a version from 1", on.event),
                ));
            }
        }
    }
    Ok(())
}
inventory::submit! { Consumer::new::<EventType>(EVENTS, "base/events", validate_events) }
inventory::submit! { Consumer::new::<Subscriber>(SUBSCRIBERS, "base/events", validate_subscribers) }

/// The mounted routing table. Publishers and subscribers must depend on
/// `base/events`; a subscriber to a mounted publisher must name an event type
/// it declares and read its current version. An unmounted publisher is not an
/// error: its subscribers simply receive nothing.
pub(crate) fn routes(catalog: &Catalog, modules: &[&Module]) -> Result<(Types, Subscribers)> {
    let depends = |entry: &Entry| {
        let ok = modules.iter().any(|module| {
            module.id == entry.provenance.module && module.dependencies.contains(&"base/events")
        });
        if ok {
            Ok(())
        } else {
            Err(refuse(
                entry,
                "is contributed by a module that does not depend on base/events".into(),
            ))
        }
    };
    let mut types = Types::new();
    for entry in catalog.entries::<EventType>(EVENTS)? {
        depends(entry)?;
        types.insert(
            entry.id,
            (entry.provenance.module, entry.get::<EventType>()?.version),
        );
    }
    let mut subscribers = Subscribers::new();
    for entry in catalog.entries::<Subscriber>(SUBSCRIBERS)? {
        depends(entry)?;
        let subscriber = entry.get::<Subscriber>()?;
        for on in subscriber.events {
            let publisher = on.event.split_once('.').map_or("", |(module, _)| module);
            if !modules.iter().any(|module| module.id == publisher) {
                continue;
            }
            match types.get(on.event) {
                None => {
                    return Err(refuse(
                        entry,
                        format!(
                            "subscribes to {}, which mounted {publisher} does not declare",
                            on.event
                        ),
                    ))
                }
                Some((_, version)) if !on.versions.contains(version) => {
                    return Err(refuse(
                        entry,
                        format!(
                            "does not read version {version} of {}, which {publisher} records",
                            on.event
                        ),
                    ))
                }
                Some(_) => {}
            }
        }
        subscribers.insert(entry.id, subscriber);
    }
    Ok((types, subscribers))
}
