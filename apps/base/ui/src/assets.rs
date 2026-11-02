//! Embedded public assets, with Phoenix-style digest URLs. Hash the final served bytes, including local references
//! and the build-specific live reload switch; never hash request input.
use askama::Template;
use bilibi_module_registry::{Catalog, Consumer, Entry, RegistryError};
use sha2::{Digest, Sha256};
use std::{borrow::Cow, collections::BTreeSet, sync::OnceLock};

/// Module-owned public assets served by the generic Web asset route.
pub const PUBLIC_ASSETS: &str = "base/ui.public-assets";

/// A self-contained asset contributed by a module. The optional CSP is for
/// HTML documents with stricter or different needs than Bilibi pages, such as
/// sandboxed MCP App documents. It never changes the host page policy.
pub struct PublicAsset {
    pub path: &'static str,
    pub content_type: &'static str,
    pub bytes: &'static [u8],
    pub content_security_policy: Option<&'static str>,
}

/// The bounded embedded asset catalogue. Templates use `url`; the HTTP host
/// uses `get` and grants immutable caching only for the matching version.
pub struct Assets;
impl Assets {
    pub fn get(path: &str) -> Option<&'static StaticAsset> {
        catalogue().iter().find(|asset| asset.path == path)
    }

    pub fn url(path: &str) -> &'static str {
        &Self::get(path).expect("registered UI asset").url
    }
}

pub struct StaticAsset {
    path: &'static str,
    url: String,
    version: String,
    pub content_type: &'static str,
    pub bytes: Cow<'static, [u8]>,
    pub content_security_policy: Option<&'static str>,
}
impl StaticAsset {
    pub fn version(&self) -> &str {
        &self.version
    }
}

fn add(
    assets: &mut Vec<StaticAsset>,
    path: &'static str,
    content_type: &'static str,
    bytes: impl Into<Cow<'static, [u8]>>,
) {
    add_with_policy(assets, path, content_type, bytes, None);
}

fn add_with_policy(
    assets: &mut Vec<StaticAsset>,
    path: &'static str,
    content_type: &'static str,
    bytes: impl Into<Cow<'static, [u8]>>,
    content_security_policy: Option<&'static str>,
) {
    let bytes = bytes.into();
    let version = Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    assets.push(StaticAsset {
        path,
        url: format!("{path}?v={version}"),
        version,
        content_type,
        bytes,
        content_security_policy,
    });
}

fn validate(entries: &[Entry]) -> bilibi_module_registry::Result<()> {
    let mut paths = BTreeSet::new();
    for entry in entries {
        let asset = entry.get::<PublicAsset>()?;
        if !asset.path.starts_with("/assets/")
            || asset.path[8..].contains('/')
            || asset.path.contains(['?', '#'])
            || asset.bytes.len() > 2 * 1024 * 1024
            || asset.content_type.is_empty()
            || !paths.insert(asset.path)
        {
            return Err(RegistryError::Contribution(format!(
                "invalid or duplicate public asset {} at {}:{}",
                asset.path, entry.provenance.source, entry.provenance.line
            )));
        }
    }
    Ok(())
}
inventory::submit! { Consumer::new::<PublicAsset>(PUBLIC_ASSETS, "base/ui", validate) }

fn references(source: &str, assets: &[StaticAsset]) -> Vec<u8> {
    let mut text = source.to_owned();
    for asset in assets {
        text = text.replace(asset.path, &asset.url);
    }
    text.into_bytes()
}

fn catalogue() -> &'static [StaticAsset] {
    static ASSETS: OnceLock<Vec<StaticAsset>> = OnceLock::new();
    ASSETS.get_or_init(|| {
        let mut assets = Vec::new();
        for (path, content_type, bytes) in [
            (
                "/fonts/instrument-sans-variable.woff2",
                "font/woff2",
                &include_bytes!("../assets/static/fonts/instrument-sans-variable.woff2")[..],
            ),
            (
                "/fonts/instrument-sans-variable-italic.woff2",
                "font/woff2",
                &include_bytes!("../assets/static/fonts/instrument-sans-variable-italic.woff2")[..],
            ),
            (
                "/fonts/OFL.txt",
                "text/plain; charset=utf-8",
                &include_bytes!("../assets/static/fonts/OFL.txt")[..],
            ),
            (
                "/assets/logo.svg",
                "image/svg+xml",
                &include_bytes!("../assets/static/logo.svg")[..],
            ),
            (
                "/favicon.svg",
                "image/svg+xml",
                &include_bytes!("../assets/static/favicon.svg")[..],
            ),
            (
                "/favicon.ico",
                "image/x-icon",
                &include_bytes!("../assets/static/favicon.ico")[..],
            ),
        ] {
            add(&mut assets, path, content_type, bytes);
        }
        for (path, script) in [
            ("/assets/ui.js", UI_JS),
            ("/assets/shell.js", SHELL_JS),
            ("/assets/hooks.js", HOOKS_JS),
            ("/assets/topbar.js", TOPBAR_JS),
            (
                "/assets/ui-feedback-driver.js",
                include_str!("../assets/static/ui-feedback-driver.js"),
            ),
        ] {
            add(
                &mut assets,
                path,
                "text/javascript; charset=utf-8",
                script.as_bytes(),
            );
        }
        let live = JS
            .replace(
                "__BILIBI_DEV_RELOAD__",
                if cfg!(debug_assertions) {
                    "true"
                } else {
                    "false"
                },
            )
            .replace(
                "__BILIBI_EVENT_LIMIT__",
                &crate::LIVE_EVENT_LIMIT.to_string(),
            )
            .replace(
                "__BILIBI_EVENT_TOO_LARGE__",
                &serde_json::to_string(crate::LIVE_EVENT_TOO_LARGE).expect("static refusal"),
            );
        add(
            &mut assets,
            "/assets/live.js",
            "text/javascript; charset=utf-8",
            live.into_bytes(),
        );
        // Version fonts before the CSS, and CSS before the static HTML: a changed
        // dependency changes the referring asset's bytes and hence its URL too.
        let css = references(CSS, &assets);
        add(
            &mut assets,
            "/assets/app.css",
            "text/css; charset=utf-8",
            css,
        );
        let specimen = references(
            &AssetSpecimen.render().expect("embedded UI asset specimen"),
            &assets,
        );
        add(
            &mut assets,
            "/assets/ui-specimen.html",
            "text/html; charset=utf-8",
            specimen,
        );
        let modules = bilibi_module_registry::discover().expect("module descriptors");
        let catalog = Catalog::discover(&modules).expect("validated contribution catalog");
        for entry in catalog
            .entries::<PublicAsset>(PUBLIC_ASSETS)
            .expect("public asset contributions")
        {
            let asset = entry.get::<PublicAsset>().expect("validated public asset");
            if assets
                .iter()
                .any(|registered| registered.path == asset.path)
            {
                panic!("public asset {} shadows a built-in UI asset", asset.path);
            }
            add_with_policy(
                &mut assets,
                asset.path,
                asset.content_type,
                asset.bytes,
                asset.content_security_policy,
            );
        }
        assets
    })
}
const CSS: &str = include_str!(concat!(env!("OUT_DIR"), "/app.css"));
const JS: &str = include_str!("../assets/live.js");
/// The hook controllers of the feedback, dialog and tab components.
const SHELL_JS: &str = include_str!("../assets/shell.js");
const UI_JS: &str = include_str!("../assets/ui.js");
/// The `data-hook` browser behaviour of the shared form controls, the inline
/// editors, tiling, the flexible table, timestamp display (`DateTime`,
/// `BrowserTimeZone`), the calendar and board (`Calendar`, `Board`) and the live file input (`FileUpload`).
const HOOKS_JS: &str = concat!(
    include_str!("../assets/hooks.js"),
    // Both files are semicolon-free IIFEs; without this the second calls the first.
    "\n;\n",
    include_str!("../assets/inline.js"),
    "\n;\n",
    include_str!("../assets/upload.js"),
    "\n;\n",
    include_str!("../assets/tiling.js"),
    "\n;\n",
    include_str!("../assets/drops.js"),
    "\n;\n",
    include_str!("../assets/flex_table.js"),
    "\n;\n",
    include_str!("../assets/datetime.js"),
    "\n;\n",
    include_str!("../assets/calendar.js"),
    "\n;\n",
    include_str!("../assets/board.js"),
    "\n;\n",
    include_str!("../assets/app_frame.js")
);
/// The vendored page-loading bar (MIT), shown by live.js for page-level round trips.
const TOPBAR_JS: &str = include_str!("../assets/vendor/topbar.js");

#[derive(Template)]
#[template(path = "ui_asset_specimen.html")]
struct AssetSpecimen;
