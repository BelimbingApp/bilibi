//! The error documents: the production card for every failed page request
//! (the credential layout, one sentence and a way back)
//! and, for debug builds only, the developer page that explains an internal
//! error (Phoenix's debug error page). Both are whole documents that link only
//! the stylesheet, so a broken script or live session cannot break them.
use crate::{AuthLayout, Html, NO_FLASH};
use askama::Template;

/// What a visitor is told about one status: the heading, the sentence and the
/// way back. `slug` names the DOM ids (`error-<slug>`, `error-<slug>-home`),
/// for the two statuses it words (404 and the rest as 500).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ErrorCopy {
    pub slug: &'static str,
    pub heading: &'static str,
    pub message: &'static str,
    pub action: &'static str,
    pub href: &'static str,
}

const HOME: &str = "Return to Bilibi";

/// The words for `status`. Internals never appear here: a production error
/// says that the page could not be shown and nothing more.
pub fn copy_for(status: u16) -> ErrorCopy {
    let (slug, heading, message) = match status {
        400 => (
            "bad-request",
            "Request not understood",
            "That request could not be processed. Check the address and try again.",
        ),
        401 => (
            "unauthorized",
            "Sign in required",
            "Your session has ended. Sign in again to continue.",
        ),
        403 => (
            "forbidden",
            "Access denied",
            "You do not have access to this page. Ask an administrator if you need it.",
        ),
        404 => (
            "not-found",
            "Page not found",
            "That address is not a page in this workspace.",
        ),
        405 => (
            "method-not-allowed",
            "Request not supported",
            "That address does not accept this kind of request.",
        ),
        409 => (
            "conflict",
            "The record changed",
            "Someone else updated it first. Reload the page and try again.",
        ),
        413 => (
            "too-large",
            "Request too large",
            "That request was larger than this workspace accepts.",
        ),
        423 => (
            "file-held",
            "File not available",
            "This file is being checked for malware, or it failed the check. Try again in a moment; if it stays unavailable, ask an administrator.",
        ),
        429 => (
            "rate-limited",
            "Too many requests",
            "Slow down for a moment, then try again.",
        ),
        503 => (
            "temporarily-unavailable",
            "Temporarily unavailable",
            "Bilibi cannot complete this request right now. Try again in a moment.",
        ),
        status if (400..500).contains(&status) => (
            "request-failed",
            "Request could not be completed",
            "That request could not be completed. Check the address and try again.",
        ),
        _ => (
            "unavailable",
            "Something went wrong",
            "This page could not be shown. Try again in a moment.",
        ),
    };
    let (action, href) = if status == 401 {
        ("Go to sign in", "/login")
    } else {
        (HOME, "/")
    };
    ErrorCopy {
        slug,
        heading,
        message,
        action,
        href,
    }
}

/// [`copy_for`] on a portal page (ADR 16): the same words, with the way back
/// into the portal (or its sign-in) instead of the staff application.
pub fn portal_copy_for(status: u16) -> ErrorCopy {
    let (action, href) = if status == 401 {
        ("Go to sign in", "/portal/sign-in")
    } else {
        ("Return to the portal", "/portal")
    };
    ErrorCopy {
        action,
        href,
        ..copy_for(status)
    }
}

#[derive(Template)]
#[template(path = "error_card.html")]
struct ErrorCard {
    copy: ErrorCopy,
}

#[derive(Template)]
#[template(path = "error_document.html")]
struct ErrorDocument<'a> {
    title: &'a str,
    staff: bool,
    body: Html,
}

/// The whole production document for `status`.
pub fn error_document(status: u16) -> askama::Result<String> {
    let copy = copy_for(status);
    let card = Html::render(&ErrorCard { copy })?;
    let body = Html::render(&AuthLayout {
        body: card,
        instance: Html::default(),
        flash: NO_FLASH,
    })?;
    ErrorDocument {
        title: &format!("{} · Bilibi", copy.heading),
        staff: true,
        body,
    }
    .render()
}

/// The production document for `status` on a portal page: the portal card,
/// with no Bilibi branding and the way back into the portal. A failed request
/// may have no session, so the card names no tenant.
pub fn portal_error_document(status: u16) -> askama::Result<String> {
    let copy = portal_copy_for(status);
    let card = Html::render(&ErrorCard { copy })?;
    let body = Html::render(&crate::PortalCard {
        title: "Customer portal",
        logo: None,
        body: card,
        flash: NO_FLASH,
    })?;
    ErrorDocument {
        title: copy.heading,
        staff: false,
        body,
    }
    .render()
}

/// One stack frame; `app` marks Bilibi's own code, which the page shows first
/// and open while the framework's frames stay folded.
#[derive(Clone, Debug)]
pub struct DebugFrame {
    pub symbol: String,
    pub location: String,
    pub app: bool,
}

/// Everything the developer page shows. The caller fills it only in debug
/// builds; nothing here is reachable from a production build's pages.
#[derive(Clone, Debug, Default)]
pub struct DebugReport {
    pub status: u16,
    pub reason: String,
    /// The error as the user would read it, then each cause below it.
    pub message: String,
    pub causes: Vec<String>,
    /// The error's `Debug` form, which names the variant and carries fields
    /// the message leaves out.
    pub detail: String,
    pub frames: Vec<DebugFrame>,
    pub method: String,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub params: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
    pub route: Vec<(String, String)>,
}

#[derive(Template)]
#[template(path = "error_debug.html")]
struct DebugDocument<'a> {
    report: &'a DebugReport,
    app_frames: Vec<&'a DebugFrame>,
    other_frames: Vec<&'a DebugFrame>,
}

/// The developer page for an internal error.
pub fn debug_document(report: &DebugReport) -> askama::Result<String> {
    DebugDocument {
        report,
        app_frames: report.frames.iter().filter(|f| f.app).collect(),
        other_frames: report.frames.iter().filter(|f| !f.app).collect(),
    }
    .render()
}
