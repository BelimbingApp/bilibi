//! Personal settings navigation, mirroring Bilimbi
//! `Core.User.Web.SettingsComponents.settings_layout`. All six pages use the
//! same layout and typed active tab, including pages owned by other modules.
use crate::Tab;

/// The active section of the signed-in account's settings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettingsTab {
    Profile,
    Password,
    TwoFactor,
    Appearance,
    ChatgptPlugin,
    AgentConnections,
}

impl SettingsTab {
    /// One navigation source for every personal settings page.
    pub fn tabs(&self) -> [Tab<'static>; 6] {
        [
            (
                Self::Profile,
                "settings-nav-profile",
                "Profile",
                "/settings/profile",
            ),
            (
                Self::Password,
                "settings-nav-password",
                "Password",
                "/settings/password",
            ),
            (
                Self::TwoFactor,
                "settings-nav-two-factor",
                "2FA",
                "/settings/two-factor",
            ),
            (
                Self::Appearance,
                "settings-nav-appearance",
                "Appearance",
                "/settings/appearance",
            ),
            (
                Self::ChatgptPlugin,
                "settings-nav-chatgpt-plugin",
                "Plugin for ChatGPT",
                "/settings/chatgpt-plugin",
            ),
            (
                Self::AgentConnections,
                "settings-nav-agent-connections",
                "Agent connections",
                "/settings/agent-connections",
            ),
        ]
        .map(|(section, id, label, href)| Tab {
            id,
            ..Tab::link(href, label, *self == section)
        })
    }
}
