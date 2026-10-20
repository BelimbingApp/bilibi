//! The thin per-harness seam. An adapter knows one harness's manifest format
//! and file layout, and nothing about settings, scopes or the agent registry:
//! the builder (`package.rs`) hands it a finished `Package`. The settings page
//! is one MCP tab (`web.rs`), not a page per harness.
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

pub trait Harness: Sync {
    /// Stable key: the download operation and the exporter's `--harness` value.
    fn key(&self) -> &'static str;
    /// The harness's product name, as people know it. The settings page does
    /// not show this; the setup guide and the exporter do.
    fn name(&self) -> &'static str;
    /// The download's file name, from the package's instance name
    /// (`bilibi-staging-chatgpt-plugin.zip`).
    fn archive_name(&self, package: &Package) -> String;
    /// The manifest, MCP configuration and skills in this harness's layout.
    fn files(&self, package: &Package) -> Vec<PackageFile>;
}

/// Every adapter. Adding a harness adds one entry here, one adapter file and
/// its download contribution. It does not add a settings tab.
pub static HARNESSES: &[&dyn Harness] = &[&ChatGpt];

pub fn find(key: &str) -> Option<&'static dyn Harness> {
    HARNESSES
        .iter()
        .copied()
        .find(|harness| harness.key() == key)
}
