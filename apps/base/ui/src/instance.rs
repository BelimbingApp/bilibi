//! Which Bilibi installation a page belongs to, drawn the same everywhere:
//! the small badge on sign-in and the banner at the top of a
//! review or confirmation. `bilibi_system::Instance` owns the facts (label,
//! colour, wording); these models own the DOM (`templates/feedback.html`).
//! The operator's colour is one of the shared status roles (`StatusKind`).
//! No Bilimbi counterpart: Bilimbi shows only a "dev" status-bar mark.
use crate::StatusKind;
use askama::Template;

/// The sign-in badge: the operator's label for this installation. `role` is
/// the deployment role (`development`, `staging`, `production`) and `title`
/// the full sentence a hover or screen reader gets. The label always says
/// where you are in words, so colour is never the only cue.
#[derive(Template, Debug, Default)]
#[template(
    source = r#"{% import "feedback.html" as ui %}{% call ui::instance_badge(badge=self) %}{% endcall %}"#,
    ext = "html"
)]
pub struct InstanceBadge<'a> {
    pub tone: StatusKind,
    pub role: &'a str,
    pub label: &'a str,
    pub title: &'a str,
}

/// The banner at the top of a review or confirmation: the label in large
/// type, then one sentence on what a change here does.
#[derive(Template, Debug, Default)]
#[template(
    source = r#"{% import "feedback.html" as ui %}{% call ui::instance_banner(banner=self) %}{% endcall %}"#,
    ext = "html"
)]
pub struct InstanceBanner<'a> {
    pub id: &'a str,
    pub tone: StatusKind,
    pub role: &'a str,
    pub label: &'a str,
    pub message: &'a str,
    pub class: &'a str,
}
