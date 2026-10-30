//! The customer portal's chrome (ADR 16): a tenant's customers see their own
//! layout, never the staff shell. No pins, tiles, search, bell, badges,
//! company switcher or status bar; the tenant's title and logo, the portal
//! menu and the account's links. Bilimbi has no customer portal; the card
//! follows the credential layout (`auth.html`) and the top bar the shell's.
use crate::Html;
use askama::Template;

/// One entry of the portal menu, flattened by the host from
/// `Menu::visible_portal_tree`. A portal menu is short, so it is one row of
/// links with no branches to open.
#[derive(Clone, Debug)]
pub struct PortalLink {
    pub id: String,
    pub label: String,
    pub icon: String,
    pub route: String,
    pub active: bool,
}

/// The signed-in portal layout. The host supplies the tenant's portal title
/// (the tenant's name until its site names another) and logo address, the
/// menu the customer may see and the account's name and CSRF secret for
/// sign-out; the page body is the owner's.
#[derive(Template)]
#[template(path = "portal.html")]
pub struct PortalLayout<'a> {
    pub title: &'a str,
    pub logo: Option<&'a str>,
    pub links: &'a [PortalLink],
    pub user: &'a str,
    pub email: &'a str,
    pub csrf: &'a str,
    pub live: bool,
    pub body: &'a str,
}
impl PortalLayout<'_> {
    pub fn initials(&self) -> String {
        self.user
            .split_whitespace()
            .take(2)
            .filter_map(|part| part.chars().next())
            .collect::<String>()
            .to_uppercase()
    }
}

/// The HTML document around a portal page: the tab reads "page · portal
/// title", its icon is the tenant's logo (none until one is set), and none of
/// the staff shell's scripts load.
#[derive(Template)]
#[template(path = "portal_document.html")]
pub struct PortalDocument<'a> {
    pub title: &'a str,
    pub site: &'a str,
    pub logo: Option<&'a str>,
    pub body: &'a str,
}

/// The portal's anonymous card (sign-in, password recovery, invitation,
/// registration): the credential layout branded with the tenant's portal
/// title and logo instead of Bilibi's.
#[derive(Template)]
#[template(path = "portal_card.html")]
pub struct PortalCard<'a> {
    pub title: &'a str,
    pub logo: Option<&'a str>,
    pub body: Html,
    pub flash: crate::FlashMap<'a>,
}

/// One demoted link under a portal card's form ("Forgot your password?").
#[derive(Clone, Copy, Debug)]
pub struct PortalCardLink<'a> {
    pub id: &'a str,
    pub href: &'a str,
    pub label: &'a str,
}

/// The body of a portal credential card (sign-in, forgotten password, reset,
/// invitation), inside a [`PortalCard`]: heading, intro, the outcome notice,
/// the form posting back to its own address and the links under it. The host
/// renders the fields from the shared form models; a password is never
/// echoed back. With no fields the card is a notice only.
#[derive(Template)]
#[template(path = "portal_form.html")]
pub struct PortalForm<'a> {
    pub id: &'a str,
    pub heading: &'a str,
    pub intro: &'a str,
    pub success: &'a str,
    pub error: &'a str,
    pub fields: Vec<Html>,
    pub submit: &'a str,
    pub working: &'a str,
    pub links: &'a [PortalCardLink<'a>],
}
