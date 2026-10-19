//! The thin per-harness seam. An adapter knows one harness's manifest format,
//! file layout and setup copy, and nothing about settings, scopes or the agent
//! registry: the builder (`package.rs`) hands it a finished `Package`.
use crate::{chatgpt::ChatGpt, package::Package};

/// One file of the package, at its path relative to the plugin folder (the
/// export directory, or the archive's top folder).
pub struct PackageFile {
    pub path: String,
    pub bytes: Vec<u8>,
}
impl PackageFile {
    pub fn new(path: impl Into<String>, bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            path: path.into(),
            bytes: bytes.into(),
        }
    }
}

/// An external guide the setup page links to.
pub struct GuideLink {
    pub label: &'static str,
    pub href: &'static str,
}

/// The setup page's harness-specific copy; the page owns everything else.
pub struct PageCopy {
    /// Install and sign in, in order.
    pub install: &'static [&'static str],
    pub download_note: &'static str,
    pub public: &'static [&'static str],
    pub public_guide: GuideLink,
    pub private: &'static [&'static str],
    pub private_note: &'static str,
    pub private_guide: GuideLink,
}

pub trait Harness: Sync {
    /// Stable key: the route operation and the exporter's `--harness` value.
    fn key(&self) -> &'static str;
    /// The harness's product name, as people know it.
    fn name(&self) -> &'static str;
    /// The page and document title, such as "Plugin for ChatGPT".
    fn title(&self) -> &'static str;
    /// The setup page; its download is the same path plus `/download`. Both
    /// are submitted in `contributions.rs` under the operation `key()`.
    fn path(&self) -> &'static str;
    /// The download's file name, from the package's instance name
    /// (`bilibi-staging-chatgpt-plugin.zip`).
    fn archive_name(&self, package: &Package) -> String;
    fn copy(&self) -> &'static PageCopy;
    /// The manifest, MCP configuration and skills in this harness's layout.
    fn files(&self, package: &Package) -> Vec<PackageFile>;
}

/// Every adapter. Adding a harness adds one entry here, one adapter file and
/// its two route contributions.
pub static HARNESSES: &[&dyn Harness] = &[&ChatGpt];

pub fn find(key: &str) -> Option<&'static dyn Harness> {
    HARNESSES
        .iter()
        .copied()
        .find(|harness| harness.key() == key)
}
