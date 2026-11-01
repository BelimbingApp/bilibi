//! The executable owns the changing Git/pin facts; registry types stay stable.
//! Bilibi-specific version contract; no Bilimbi module counterpart (docs/versions.md).
use bilibi_module_registry::{Composition, Container, Layer, PinState};

/// One mounted Domain or Extension as the pin recorded it.
#[derive(Clone, Copy, Debug)]
struct PinnedContainer {
    /// `domains/finance`: the layer folder and the container ID.
    name: &'static str,
    commit: &'static str,
    /// The `vX.Y.Z` tag at that commit, without the `v`; `None` when untagged.
    version: Option<&'static str>,
}
/// The pin as `build.rs` read it.
#[derive(Clone, Copy, Debug)]
struct Pin {
    platform_commit: &'static str,
    platform_version: &'static str,
    /// `v0.1.0` when the Platform commit is a release; `None` between releases.
    platform_tag: Option<&'static str>,
    containers: &'static [PinnedContainer],
}
/// `PIN`, `PIN_STALE` (why the pin was left out because the checkouts moved
/// since it was made) and `BUILT_FROM` (the commit Git reported at build time,
/// set only for an unpinned build) and `BUILT_CONTAINERS` (the mounted checkouts' own commits and tags Git reported then), as `build.rs` generated them.
mod generated {
    #![allow(dead_code, unused_imports)]
    use super::{Pin, PinnedContainer};
    include!(concat!(env!("OUT_DIR"), "/composition.rs"));
}

/// The program's composition from the compiled-in pin and the mounted module
/// lists the host passes to `verify_composition` (`bilibi_domains::MODULES`,
/// `bilibi_extensions::MODULES`). A pin that names other containers than the
/// ones mounted, another Platform version than `Cargo.toml`, or other commits than
/// the checkouts `build.rs` compiled, is stale, and its commits are left out rather than shown as if they were true.
pub(super) fn composition() -> Composition {
    let domains = bilibi_domains::MODULES;
    let extensions = bilibi_extensions::MODULES;
    let version = env!("CARGO_PKG_VERSION");
    let mut containers = Vec::new();
    for (layer, modules) in [(Layer::Domains, domains), (Layer::Extensions, extensions)] {
        for (module, _) in modules {
            let id = module.split('/').next().unwrap_or(module);
            let index = containers
                .iter()
                .position(|container: &Container| container.layer == layer && container.id == id)
                .unwrap_or_else(|| {
                    containers.push(Container {
                        layer,
                        id,
                        modules: Vec::new(),
                        commit: None,
                        version: None,
                    });
                    containers.len() - 1
                });
            containers[index].modules.push(module);
        }
    }
    let Some(pin) = generated::PIN else {
        if let Some(reason) = generated::PIN_STALE {
            return Composition {
                version,
                commit: None,
                tag: None,
                pin: PinState::Stale(format!(
                    "{reason}; run scripts/discover-modules.py --pin and build again"
                )),
                containers,
            };
        }
        for container in &mut containers {
            let name = format!("{}/{}", folder_of(container.layer), container.id);
            if let Some(built) = generated::BUILT_CONTAINERS.iter().find(|c| c.name == name) {
                container.commit = Some(built.commit);
                container.version = built.version;
            }
        }
        return Composition {
            version,
            commit: generated::BUILT_FROM,
            tag: None,
            pin: PinState::Unpinned,
            containers,
        };
    };
    let mut stale = None;
    if pin.platform_version != version {
        stale = Some(format!(
            "the pin was made for Platform {} but this is {version}",
            pin.platform_version
        ));
    }
    let pinned_names: Vec<&str> = pin.containers.iter().map(|c| c.name).collect();
    let mounted_names: Vec<String> = containers
        .iter()
        .map(|c| format!("{}/{}", folder_of(c.layer), c.id))
        .collect();
    for name in &pinned_names {
        if stale.is_none() && !mounted_names.iter().any(|mounted| mounted == name) {
            stale = Some(format!("the pin names {name}, which is not mounted"));
        }
    }
    for name in &mounted_names {
        if stale.is_none() && !pinned_names.contains(&name.as_str()) {
            stale = Some(format!("{name} is mounted but not in the pin"));
        }
    }
    if let Some(reason) = stale {
        return Composition {
            version,
            commit: None,
            tag: None,
            pin: PinState::Stale(format!(
                "{reason}; run scripts/discover-modules.py --pin and build again"
            )),
            containers,
        };
    }
    for container in &mut containers {
        let name = format!("{}/{}", folder_of(container.layer), container.id);
        if let Some(pinned) = pin.containers.iter().find(|c| c.name == name) {
            container.commit = Some(pinned.commit);
            container.version = pinned.version;
        }
    }
    Composition {
        version,
        commit: Some(pin.platform_commit),
        tag: pin.platform_tag,
        pin: PinState::Pinned,
        containers,
    }
}

fn folder_of(layer: Layer) -> &'static str {
    match layer {
        Layer::Domains => "domains",
        Layer::Extensions => "extensions",
        Layer::Base => "base",
        Layer::Core => "core",
    }
}
