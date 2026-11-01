//! Personal settings navigation. All six pages use the
//! same layout and typed active tab, including the pages owned by
//! `core/agent_access` and `base/search`.
use crate::Tab;

/// The active section of the signed-in account's settings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettingsTab {
    Profile,
    Password,
    TwoFactor,
    Appearance,
    Search,
    AiConnections,
}

/// One navigation source for every personal settings page: its tab, id, label
/// and address.
const PAGES: [(SettingsTab, &str, &str, &str); 6] = [
    (
        SettingsTab::Profile,
        "settings-nav-profile",
        "Profile",
        "/settings/profile",
    ),
    (
        SettingsTab::Password,
        "settings-nav-password",
        "Password",
        "/settings/password",
    ),
    (
        SettingsTab::TwoFactor,
        "settings-nav-two-factor",
        "2FA",
        "/settings/two-factor",
    ),
    (
        SettingsTab::Appearance,
        "settings-nav-appearance",
        "Appearance",
        "/settings/appearance",
    ),
    (
        SettingsTab::Search,
        "settings-nav-search",
        "Search",
        "/settings/search",
    ),
    (
        SettingsTab::AiConnections,
        "settings-nav-ai-connections",
        "AI connections",
        "/settings/ai-connections",
    ),
];

/// Where the personal settings pages live, as the page search names it.
pub const SETTINGS_LOCATION: [&str; 2] = ["Account", "My Settings"];

impl SettingsTab {
    pub fn tabs(&self) -> [Tab<'static>; 6] {
        PAGES.map(|(section, id, label, href)| Tab {
            id,
            ..Tab::link(href, label, *self == section)
        })
    }
    /// Every personal settings page as (label, address), in tab order.
    pub fn pages() -> [(&'static str, &'static str); 6] {
        PAGES.map(|(_, _, label, href)| (label, href))
    }
}
