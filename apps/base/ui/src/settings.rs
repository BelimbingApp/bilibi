//! Personal settings navigation, mirroring Bilimbi
//! `Core.User.Web.SettingsComponents.settings_layout`. All five pages use the
//! same layout and typed active tab, including the page owned by
//! `core/agent_access`.
use crate::Tab;

/// The active section of the signed-in account's settings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettingsTab {
    Profile,
    Password,
    TwoFactor,
    Appearance,
    AiConnections,
}

/// One navigation source for every personal settings page: its tab, id, label
/// and address.
const PAGES: [(SettingsTab, &str, &str, &str); 5] = [
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
        SettingsTab::AiConnections,
        "settings-nav-ai-connections",
        "AI connections",
        "/settings/ai-connections",
    ),
];

/// Where the personal settings pages live, as the page search names it.
pub const SETTINGS_LOCATION: [&str; 2] = ["Account", "My Settings"];

impl SettingsTab {
    pub fn tabs(&self) -> [Tab<'static>; 5] {
        PAGES.map(|(section, id, label, href)| Tab {
            id,
            ..Tab::link(href, label, *self == section)
        })
    }
    /// Every personal settings page as (label, address), in tab order.
    pub fn pages() -> [(&'static str, &'static str); 5] {
        PAGES.map(|(_, _, label, href)| (label, href))
    }
}
