//! Embedded icons and the semantic action vocabulary. The shared icon macro renders every name as SVG.
use std::{
    collections::BTreeSet,
    sync::{Mutex, OnceLock},
};

/// A trusted SVG embedded by the UI build. Its body never contains caller data.
pub struct SvgIcon {
    pub name: &'static str,
    pub view_box: &'static str,
    pub fill: &'static str,
    pub stroke_width: &'static str,
    pub size: u8,
    body: &'static str,
}
impl SvgIcon {
    /// Original SVG children, including path fill/clip rules.
    /// Only the shared icon macro may render this trusted body with `safe`.
    pub fn body(&self) -> &'static str {
        self.body
    }
}

include!(concat!(env!("OUT_DIR"), "/icons.rs"));

/// The icon catalogue: bare SVG names and semantic actions use the same format.
/// Unknown names keep the page usable with a neutral missing-icon glyph.
pub struct Icons;
impl Icons {
    /// Resolve a name without CSS scanning, filesystem reads, or prefix guessing.
    /// An unknown name warns once per process and uses the missing-icon glyph.
    pub fn resolve(name: &str) -> &'static SvgIcon {
        find(name).unwrap_or_else(|| {
            warn_unknown(name, None);
            &MISSING
        })
    }

    /// Whether the catalogue knows the name, without warning about it.
    pub fn is_known(name: &str) -> bool {
        find(name).is_some()
    }

    /// Diagnose menu contributions at boot, before the first render. Even a
    /// hidden or dropped menu item gets a warning naming its contributing module.
    pub fn check_menu(name: &str, module: &str) {
        if find(name).is_none() {
            warn_unknown(name, Some(module));
        }
    }

    /// The SVG name chosen for a familiar user-facing action.
    pub fn action(name: &str) -> Option<&'static str> {
        ACTIONS
            .iter()
            .find(|(action, _)| *action == name)
            .map(|(_, icon)| *icon)
    }

    /// Shared shell action names keep the top bar and Design Library in step.
    pub fn shell(action: &str) -> &'static str {
        SHELL
            .iter()
            .find(|(name, _)| *name == action)
            .map(|(_, icon)| *icon)
            .unwrap_or_else(|| {
                warn_unknown(action, None);
                MISSING.name
            })
    }

    /// Every accepted name once, including semantic actions and all SVG variants.
    pub fn names() -> impl Iterator<Item = &'static str> {
        GLYPHS
            .iter()
            .map(|icon| icon.name)
            .chain(ACTIONS.iter().map(|(name, _)| *name))
            .chain(
                HEROICONS
                    .iter()
                    .map(|icon| icon.name)
                    .filter(|name| Self::action(name).is_none()),
            )
    }

    pub fn glyphs() -> &'static [SvgIcon] {
        GLYPHS
    }
    pub fn heroicons() -> &'static [SvgIcon] {
        HEROICONS
    }
    pub fn actions() -> &'static [(&'static str, &'static str)] {
        ACTIONS
    }
}

fn find(name: &str) -> Option<&'static SvgIcon> {
    let name = Icons::action(name).unwrap_or(name);
    GLYPHS.iter().find(|icon| icon.name == name).or_else(|| {
        HEROICONS
            .binary_search_by_key(&name, |icon| icon.name)
            .ok()
            .map(|index| &HEROICONS[index])
    })
}

type UnknownNames = BTreeSet<(String, Option<String>)>;

fn warn_unknown(name: &str, module: Option<&str>) {
    // Menu validation runs before rendering. Suppress a later context-free
    // warning for a name already diagnosed with its module, but retain each
    // contributing module's diagnostic when several use the same unknown name.
    static WARNED: OnceLock<Mutex<UnknownNames>> = OnceLock::new();
    let mut warned = WARNED
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if module.is_none() && warned.iter().any(|(previous, _)| previous == name) {
        return;
    }
    if warned.insert((name.to_owned(), module.map(str::to_owned))) {
        if let Some(module) = module {
            eprintln!(
                "warning: unknown icon {name:?} contributed by module {module:?}; using missing-icon"
            );
        } else {
            eprintln!("warning: unknown icon {name:?}; using missing-icon");
        }
    }
}

// A quiet neutral box, deliberately distinct from a business status or action.
const MISSING: SvgIcon = SvgIcon {
    name: "missing-icon",
    view_box: "0 0 24 24",
    fill: "none",
    stroke_width: "1.5",
    size: 24,
    body:
        "<rect x=\"4.5\" y=\"4.5\" width=\"15\" height=\"15\" rx=\"2\" stroke-dasharray=\"3 3\"/>",
};
