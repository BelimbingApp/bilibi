//! Embedded public assets, mirroring Bilimbi's apps/web/priv/static and
//! Phoenix digest URLs. Hash the final served bytes, including local references
//! and the build-specific live reload switch; never hash request input.
use sha2::{Digest, Sha256};
use std::{borrow::Cow, sync::OnceLock};

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
    });
}

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
        let live = JS.replace(
            "__BILIBI_DEV_RELOAD__",
            if cfg!(debug_assertions) {
                "true"
            } else {
                "false"
            },
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
        let specimen = references(include_str!("../assets/static/ui-specimen.html"), &assets);
        add(
            &mut assets,
            "/assets/ui-specimen.html",
            "text/html; charset=utf-8",
            specimen,
        );
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
/// `BrowserTimeZone`) and the live file input (`FileUpload`).
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
    include_str!("../assets/flex_table.js"),
    "\n;\n",
    include_str!("../assets/datetime.js")
);
/// Bilimbi's vendored page-loading bar (MIT), shown by live.js for page-level round trips.
const TOPBAR_JS: &str = include_str!("../assets/vendor/topbar.js");
