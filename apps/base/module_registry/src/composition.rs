//! Stable software identity types, supplied by the executable host at startup.
//! Bilibi-specific version contract (docs/versions.md).
use crate::Layer;

/// Whether the mounted Domains and Extensions were pinned when this program was built.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PinState {
    /// Built from `.local/composition`: every version and commit is exact.
    Pinned,
    /// Built without `--pin`: a development build; only the Platform commit is known.
    Unpinned,
    /// A pin was present but describes another composition or other checkouts; its facts are not trusted.
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
