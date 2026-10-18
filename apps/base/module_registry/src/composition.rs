//! What this program is made of: the Platform version and commit, and each
//! mounted Domain or Extension with its version and commit, as
//! `scripts/discover-modules.py --pin` recorded them and `build.rs` compiled
//! them in (docs/versions.md). The program is the truth about its own make-up,
//! so nothing here is read from a file or a database at run time.
use crate::Layer;

/// One mounted Domain or Extension as the pin recorded it.
#[derive(Clone, Copy, Debug)]
pub struct PinnedContainer {
    /// `domains/finance`: the layer folder and the container ID.
    pub name: &'static str,
    pub commit: &'static str,
    /// The `vX.Y.Z` tag at that commit, without the `v`; `None` when untagged.
    pub version: Option<&'static str>,
}
/// The pin as `build.rs` read it.
#[derive(Clone, Copy, Debug)]
pub struct Pin {
    pub platform_commit: &'static str,
    pub platform_version: &'static str,
    /// `v0.1.0` when the Platform commit is a release; `None` between releases.
    pub platform_tag: Option<&'static str>,
    pub containers: &'static [PinnedContainer],
}
/// `PIN`, and `BUILT_FROM` (the commit Git reported at build time, set only
/// for an unpinned build), as `build.rs` generated them.
mod generated {
    #![allow(dead_code, unused_imports)]
    use super::{Pin, PinnedContainer};
    include!(concat!(env!("OUT_DIR"), "/composition.rs"));
}

/// Whether the mounted Domains and Extensions were pinned when this program was built.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PinState {
    /// Built from `.local/composition`: every version and commit is exact.
    Pinned,
    /// Built without `--pin`: a development build; only the Platform commit is known.
    Unpinned,
    /// A pin was present but describes another composition; its facts are not trusted.
    Stale(String),
}

/// A mounted Domain or Extension in this program.
#[derive(Clone, Debug)]
pub struct Container {
    pub layer: Layer,
    /// The container ID, `finance`.
    pub id: &'static str,
    /// Its module IDs, `finance/ledger`.
    pub modules: Vec<&'static str>,
    pub commit: Option<&'static str>,
    /// The release the container is at, `0.1.0`; `None` when unpinned or built from an untagged commit.
    pub version: Option<&'static str>,
}

/// The whole program's make-up.
#[derive(Clone, Debug)]
pub struct Composition {
    /// The Platform version from `Cargo.toml`.
    pub version: &'static str,
    pub commit: Option<&'static str>,
    /// `Some("v0.1.0")` when the Platform commit is that release.
    pub tag: Option<&'static str>,
    pub pin: PinState,
    pub containers: Vec<Container>,
}
impl Composition {
    /// `true` when this build is exactly the release `v<version>`.
    pub fn released(&self) -> bool {
        self.tag.is_some()
    }
    /// The commit shortened to the usual twelve characters, if known.
    pub fn short_commit(&self) -> Option<&'static str> {
        self.commit.map(short)
    }
}
impl Container {
    pub fn short_commit(&self) -> Option<&'static str> {
        self.commit.map(short)
    }
}
fn short(commit: &'static str) -> &'static str {
    commit.get(..12).unwrap_or(commit)
}

/// The program's composition from the compiled-in pin and the mounted module
/// lists the host passes to `verify_composition` (`bilibi_domains::MODULES`,
/// `bilibi_extensions::MODULES`). A pin that names other containers than the
/// ones mounted, or another Platform version than `Cargo.toml`, is stale, and
/// its commits are left out rather than shown as if they were true.
pub fn composition(
    domains: &[(&'static str, &'static str)],
    extensions: &[(&'static str, &'static str)],
) -> Composition {
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
