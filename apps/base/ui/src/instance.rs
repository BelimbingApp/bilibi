//! Which Bilibi installation a page belongs to, drawn the same everywhere:
//! the small badge in the shell's top bar and the banner at the top of a
//! review or confirmation. Base Instance (`bilibi_instance`) owns the facts
//! and the wording; these models own the DOM (`templates/feedback.html`). No
//! Bilimbi counterpart: Bilimbi shows only a "dev" status-bar mark.
use askama::Template;

/// The kind of installation, as the badge and banner colour it. Development
/// and staging stand out (info, warning); production is the sober default.
/// The label always says the kind in words, so colour is never the only cue.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InstanceKind {
    Development,
    Staging,
    #[default]
    Production,
}

impl InstanceKind {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Development => "development",
            Self::Staging => "staging",
            Self::Production => "production",
        }
    }
    pub fn class(&self) -> &'static str {
        match self {
            Self::Development => "border-info-line bg-info-surface text-info-ink",
            Self::Staging => "border-warning-line bg-warning-surface text-warning-ink",
            Self::Production => "border-high-contrast-line bg-surface text-ink",
        }
    }
    pub fn icon(&self) -> &'static str {
        match self {
            Self::Development => "hero-wrench-screwdriver",
            Self::Staging => "hero-beaker",
            Self::Production => "hero-signal",
        }
    }
}

/// The top-bar badge: the kind and, when set, the installation's name.
/// `title` is the full sentence a hover or screen reader gets.
#[derive(Template, Debug, Default)]
#[template(
    source = r#"{% import "feedback.html" as ui %}{% call ui::instance_badge(badge=self) %}{% endcall %}"#,
    ext = "html"
)]
pub struct InstanceBadge<'a> {
    pub kind: InstanceKind,
    pub label: &'a str,
    pub name: Option<&'a str>,
    pub title: &'a str,
}

/// The banner at the top of a review or confirmation: the kind and name in
/// large type, then one sentence on what a change here does.
#[derive(Template, Debug, Default)]
#[template(
    source = r#"{% import "feedback.html" as ui %}{% call ui::instance_banner(banner=self) %}{% endcall %}"#,
    ext = "html"
)]
pub struct InstanceBanner<'a> {
    pub id: &'a str,
    pub kind: InstanceKind,
    pub label: &'a str,
    pub name: Option<&'a str>,
    pub message: &'a str,
    pub class: &'a str,
}
