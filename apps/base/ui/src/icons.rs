//! Product glyphs and the named action vocabulary rendered through the shared
//! `icon` macro in `templates/components.html`.
//!
//! Custom SVGs cover glyphs Heroicons does not provide. Action names map each
//! familiar user-facing meaning onto one Heroicon so call sites name the action
//! rather than a raw `hero-*` string. Unknown names beginning with `hero-`
//! still pass through to the generated Heroicon utilities.
//!
//! `resolve` panics for a name that is neither registered here nor a `hero-*`
//! passthrough, instead of silently rendering a generic fallback glyph: a
//! fail-soft registry is how a misspelled action name renders the same
//! meaningless glyph for every caller without anyone noticing. A name that
//! comes from stored data rather than source is filtered through `renderable`
//! first. Tables are transcribed from Bilimbi `ui/icon_registry.ex` at the
//! pinned source; `assets/vendor/product-icons/` holds the same glyphs as files.

/// One custom SVG: an inline `<svg>` with one `<path>` per entry.
pub struct Glyph {
    pub name: &'static str,
    pub view_box: &'static str,
    pub fill: &'static str,
    pub paths: &'static [&'static str],
}

impl Glyph {
    /// Outline glyphs stroke `currentColor`; filled ones draw their own fill.
    pub fn outlined(&self) -> bool {
        self.fill == "none"
    }
}

/// A resolved icon name: a registered glyph, or the Heroicon utility class.
pub struct Resolved<'a> {
    pub glyph: Option<&'static Glyph>,
    pub hero: &'a str,
}

/// Resolves a name to a registered custom SVG or a chosen Heroicon.
///
/// # Panics
///
/// Panics when `name` is neither registered nor a `hero-*` passthrough.
pub fn resolve(name: &str) -> Resolved<'_> {
    if let Some(glyph) = GLYPHS.iter().find(|glyph| glyph.name == name) {
        return Resolved {
            glyph: Some(glyph),
            hero: "",
        };
    }
    if let Some(hero) = action(name) {
        return Resolved { glyph: None, hero };
    }
    if name.starts_with("hero-") {
        return Resolved {
            glyph: None,
            hero: name,
        };
    }
    panic!(
        "bilibi_ui::icons has no icon named {name:?}. Register it in GLYPHS or ACTIONS in \
         apps/base/ui/src/icons.rs, or use a raw \"hero-*\" Heroicon name if no semantic \
         action name applies."
    );
}

/// The Heroicon chosen for an action name.
pub fn action(name: &str) -> Option<&'static str> {
    ACTIONS
        .iter()
        .find(|(action, _)| *action == name)
        .map(|(_, hero)| *hero)
}

/// Returns true when the icon macro renders `name` instead of panicking.
///
/// The one safe entry point for a name that reaches the icon from data rather
/// than from source, so the caller can substitute its own default.
pub fn renderable(name: &str) -> bool {
    GLYPHS.iter().any(|glyph| glyph.name == name)
        || action(name).is_some()
        || name.starts_with("hero-")
}

/// The accepted Heroicon for a shell action (`clock`, `light`, `dark`,
/// `system`, `chevron`, `settings`, `logout`, `warning`), so the top bar, the
/// account menu and the Design Library cannot drift apart on the same action.
///
/// # Panics
///
/// Panics for an unknown shell action; the vocabulary is closed.
pub fn shell(action: &str) -> &'static str {
    SHELL
        .iter()
        .find(|(name, _)| *name == action)
        .map(|(_, hero)| *hero)
        .unwrap_or_else(|| panic!("bilibi_ui::icons has no shell action named {action:?}"))
}

pub const GLYPHS: &[Glyph] = &[
    // Iconoir `design-nib` (regular), which mirrors the shared Design Library
    // page icon used by Bilimbi's UI icon registry.
    Glyph {
        name: "iconoir-design-nib",
        view_box: "0 0 24 24",
        fill: "none",
        paths: &[
            "M17.6744 11.4075L15.7691 17.1233C15.7072 17.309 15.5586 17.4529 15.3709 17.5087L3.69348 20.9803C3.22819 21.1186 2.79978 20.676 2.95328 20.2155L6.74467 8.84131C6.79981 8.67588 6.92419 8.54263 7.08543 8.47624L12.472 6.25822C12.696 6.166 12.9535 6.21749 13.1248 6.38876L17.5294 10.7935C17.6901 10.9542 17.7463 11.1919 17.6744 11.4075Z",
            "M3.2959 20.6016L9.65986 14.2376",
            "M17.7917 11.0557L20.6202 8.22724C21.4012 7.44619 21.4012 6.17986 20.6202 5.39881L18.4989 3.27749C17.7178 2.49645 16.4515 2.49645 15.6704 3.27749L12.842 6.10592",
            "M11.7814 12.1163C11.1956 11.5305 10.2458 11.5305 9.66004 12.1163C9.07426 12.7021 9.07426 13.6519 9.66004 14.2376C10.2458 14.8234 11.1956 14.8234 11.7814 14.2376C12.3671 13.6519 12.3671 12.7021 11.7814 12.1163Z",
        ],
    },
    Glyph {
        name: "bilimbi-impersonate",
        view_box: "0 0 24 24",
        fill: "none",
        paths: &["M21 15.75A2.25 2.25 0 0 1 18.75 18H5.25A2.25 2.25 0 0 1 3 15.75V5.25A2.25 2.25 0 0 1 5.25 3h13.5A2.25 2.25 0 0 1 21 5.25v10.5Z", "M10.5 18v3.25h-2.25a.75.75 0 0 0 0 1.5h7.5a.75.75 0 0 0 0-1.5h-2.25V18", "M12 6.75a2.75 2.75 0 1 1 0 5.5 2.75 2.75 0 0 1 0-5.5Z", "M6 17a7.5 7.5 0 0 1 12 0"],
    },
    Glyph {
        name: "bilimbi-pin",
        view_box: "0 0 24 24",
        fill: "none",
        paths: &["M16.114 1.553l6.333 6.333a1.75 1.75 0 0 1-.603 2.869l-1.63.633a5.67 5.67 0 0 0-3.395 3.725l-1.131 3.959a1.75 1.75 0 0 1-2.92.757L9 16.061 7.939 15l-3.768-3.768a1.75 1.75 0 0 1 .757-2.92l3.959-1.131a5.666 5.666 0 0 0 3.725-3.395l.633-1.63a1.75 1.75 0 0 1 2.869-.603Z", "M9 16.061L3.405 21.655"],
    },
    Glyph {
        name: "hero-impersonate",
        view_box: "0 0 24 24",
        fill: "none",
        paths: &["M21 15.75A2.25 2.25 0 0 1 18.75 18H5.25A2.25 2.25 0 0 1 3 15.75V5.25A2.25 2.25 0 0 1 5.25 3h13.5A2.25 2.25 0 0 1 21 5.25v10.5Z", "M10.5 18v3.25h-2.25a.75.75 0 0 0 0 1.5h7.5a.75.75 0 0 0 0-1.5h-2.25V18", "M12 6.75a2.75 2.75 0 1 1 0 5.5 2.75 2.75 0 0 1 0-5.5Z", "M6 17a7.5 7.5 0 0 1 12 0"],
    },
    Glyph {
        name: "rows-compact",
        view_box: "0 0 24 24",
        fill: "currentColor",
        paths: &["M2 5H4V7H2V5M5 5H7V7H5V5M8 5H10V7H8V5M11 5H13V7H11V5M14 5H16V7H14V5M17 5H19V7H17V5M20 5H22V7H20V5M2 8H4V10H2V8M5 8H7V10H5V8M8 8H10V10H8V8M11 8H13V10H11V8M14 8H16V10H14V8M17 8H19V10H17V8M20 8H22V10H20V8M2 11H4V13H2V11M5 11H7V13H5V11M8 11H10V13H8V11M11 11H13V13H11V11M14 11H16V13H14V11M17 11H19V13H17V11M20 11H22V13H20V11M2 14H4V16H2V14M5 14H7V16H5V14M8 14H10V16H8V14M11 14H13V16H11V14M14 14H16V16H14V14M17 14H19V16H17V14M20 14H22V16H20V14M2 17H4V19H2V17M5 17H7V19H5V17M8 17H10V19H8V17M11 17H13V19H11V17M14 17H16V19H14V17M17 17H19V19H17V17M20 17H22V19H20V17"],
    },
    Glyph {
        name: "rows-normal",
        view_box: "0 0 24 24",
        fill: "currentColor",
        paths: &["M3,11H11V3H3M3,21H11V13H3M13,21H21V13H13M13,3V11H21V3"],
    },
    Glyph {
        name: "text-indent-left",
        view_box: "0 0 16 16",
        fill: "currentColor",
        paths: &["M2 3.5a.5.5 0 0 1 .5-.5h11a.5.5 0 0 1 0 1h-11a.5.5 0 0 1-.5-.5m.646 2.146a.5.5 0 0 1 .708 0l2 2a.5.5 0 0 1 0 .708l-2 2a.5.5 0 0 1-.708-.708L4.293 8 2.646 6.354a.5.5 0 0 1 0-.708M7 6.5a.5.5 0 0 1 .5-.5h6a.5.5 0 0 1 0 1h-6a.5.5 0 0 1-.5-.5m0 3a.5.5 0 0 1 .5-.5h6a.5.5 0 0 1 0 1h-6a.5.5 0 0 1-.5-.5m-5 3a.5.5 0 0 1 .5-.5h11a.5.5 0 0 1 0 1h-11a.5.5 0 0 1-.5-.5"],
    },
    Glyph {
        name: "text-indent-right",
        view_box: "0 0 16 16",
        fill: "currentColor",
        paths: &["M2 3.5a.5.5 0 0 1 .5-.5h11a.5.5 0 0 1 0 1h-11a.5.5 0 0 1-.5-.5m10.646 2.146a.5.5 0 0 1 .708.708L11.707 8l1.647 1.646a.5.5 0 0 1-.708.708l-2-2a.5.5 0 0 1 0-.708zM2 6.5a.5.5 0 0 1 .5-.5h6a.5.5 0 0 1 0 1h-6a.5.5 0 0 1-.5-.5m0 3a.5.5 0 0 1 .5-.5h6a.5.5 0 0 1 0 1h-6a.5.5 0 0 1-.5-.5m0 3a.5.5 0 0 1 .5-.5h11a.5.5 0 0 1 0 1h-11a.5.5 0 0 1-.5-.5"],
    },
];

pub const ACTIONS: &[(&str, &str)] = &[
    ("archive", "hero-archive-box"),
    ("attach", "hero-paper-clip"),
    ("back", "hero-arrow-left"),
    ("clock", "hero-clock"),
    ("close", "hero-x-mark"),
    ("collapse", "hero-chevron-down"),
    ("conceal", "hero-eye-slash"),
    ("confirm", "hero-check"),
    ("copy", "hero-clipboard"),
    ("create", "hero-plus"),
    ("customize", "hero-chevron-double-up-micro"),
    ("customizing", "hero-chevron-double-down-micro"),
    ("dashboard", "hero-squares-2x2"),
    ("delete", "hero-trash"),
    ("duplicate", "hero-document-duplicate"),
    ("edit", "hero-pencil"),
    ("error", "hero-exclamation-circle"),
    ("expand", "hero-chevron-right"),
    ("export", "hero-arrow-down-tray"),
    ("external", "hero-arrow-top-right-on-square"),
    ("filter", "hero-funnel"),
    ("forward", "hero-arrow-right"),
    ("fullscreen", "hero-arrows-pointing-out"),
    ("fullscreen-exit", "hero-arrows-pointing-in"),
    ("help", "hero-question-mark-circle"),
    ("history", "hero-clock"),
    ("import", "hero-inbox-arrow-down"),
    ("information", "hero-information-circle"),
    ("inspect", "hero-document-magnifying-glass"),
    ("link", "hero-link"),
    ("manage", "hero-cog-6-tooth"),
    ("notify", "hero-bell"),
    ("page-next", "hero-chevron-right"),
    ("page-previous", "hero-chevron-left"),
    ("pause", "hero-pause"),
    ("play", "hero-play"),
    ("refresh", "hero-arrow-path"),
    ("restricted", "hero-lock-closed"),
    ("retain", "hero-bookmark"),
    ("retained", "hero-bookmark-solid"),
    ("reveal", "hero-eye"),
    ("save", "hero-check-circle"),
    ("search", "hero-magnifying-glass"),
    ("settings", "hero-cog-6-tooth"),
    ("share", "hero-share"),
    ("sort", "hero-chevron-up-down"),
    ("sort-asc", "hero-chevron-up"),
    ("sort-desc", "hero-chevron-down"),
    ("status", "hero-signal"),
    ("stop", "hero-stop"),
    ("success", "hero-check-circle"),
    ("theme-dark", "hero-moon"),
    ("theme-light", "hero-sun"),
    ("theme-system", "hero-computer-desktop"),
    ("tile", "hero-squares-plus"),
    ("unarchive", "hero-archive-box-x-mark"),
    ("unlink", "hero-link-slash"),
    ("view", "hero-eye"),
    ("warning", "hero-exclamation-triangle"),
    ("zoom-in", "hero-plus"),
    ("zoom-out", "hero-minus"),
];

pub const SHELL: &[(&str, &str)] = &[
    ("chevron", "hero-chevron-down-mini"),
    ("clock", "hero-clock"),
    ("dark", "hero-moon"),
    ("light", "hero-sun"),
    ("logout", "hero-arrow-right-on-rectangle"),
    ("settings", "hero-cog-6-tooth"),
    ("system", "hero-computer-desktop"),
    ("warning", "hero-exclamation-triangle"),
];
