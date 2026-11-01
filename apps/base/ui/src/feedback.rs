//! Typed vocabularies and view models behind `templates/feedback.html`,
//! `templates/dialog.html` and `templates/tabs.html`: flash, the connection
//! banners, inline alerts and panel notices, the modal and confirmation
//! dialogs, and the tab strip. The templates own the DOM; the browser half is
//! `assets/ui.js`.
use crate::Html;
use askama::Template;

/// How long a success flash stays before the browser dismisses it. Info,
/// warning and error never time out: a message someone must act on cannot
/// disappear on a timer.
pub const AUTO_DISMISS_MS: u32 = 8_000;

/// What a message does, not how it is worded: a completed write is `Success`,
/// a statement that confirms nothing is `Info`, and a refusal or failure is
/// `Error`. The kind picks the color role, the icon and the ARIA role.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StatusKind {
    #[default]
    Info,
    Success,
    Warning,
    Error,
}

impl StatusKind {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Success => "success",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            Self::Info => "information",
            Self::Success => "success",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }

    /// Success and info are a polite `status` that waits for the reader;
    /// warning and error interrupt as an assertive `alert`.
    pub fn role(&self) -> &'static str {
        match self {
            Self::Success | Self::Info => "status",
            Self::Warning | Self::Error => "alert",
        }
    }

    /// Stated even though each role implies it, so the message keeps its own
    /// politeness inside any live region that contains it.
    pub fn live(&self) -> &'static str {
        match self {
            Self::Success | Self::Info => "polite",
            Self::Warning | Self::Error => "assertive",
        }
    }

    /// The page-level flash at banner strength.
    pub fn flash_class(&self) -> &'static str {
        match self {
            Self::Success => "border-success-line bg-success-surface/95 text-success-ink",
            Self::Info => "border-info-line bg-info-surface/95 text-info-ink",
            Self::Warning => "border-warning-line bg-warning-surface/95 text-warning-ink",
            Self::Error => "border-danger-line bg-danger-surface/95 text-danger-ink",
        }
    }

    /// The one inline colouring per status role, shared by alert and panel
    /// notice so a success reads as the same idea wherever it lands.
    pub fn surface_class(&self) -> &'static str {
        match self {
            Self::Info => "border-info-line bg-info-surface text-info-ink",
            Self::Success => "border-success-line bg-success-surface text-success-ink",
            Self::Warning => "border-warning-line bg-warning-surface text-warning-ink",
            Self::Error => "border-danger-line bg-danger-surface text-danger-ink",
        }
    }

    /// Whether the browser dismisses this kind after [`AUTO_DISMISS_MS`].
    pub fn auto_dismisses(&self) -> bool {
        matches!(self, Self::Success)
    }
}

/// The kinds a panel's outcome notice can take: a refusal or failure is
/// `Error`; there is no warning.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NoticeKind {
    #[default]
    Info,
    Success,
    Error,
}

impl NoticeKind {
    pub fn status(&self) -> StatusKind {
        match self {
            Self::Info => StatusKind::Info,
            Self::Success => StatusKind::Success,
            Self::Error => StatusKind::Error,
        }
    }
}

/// The messages a server wants shown, one per kind; empty means none. The
/// layout's flash group is the single production outlet.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FlashMap<'a> {
    pub error: &'a str,
    pub warning: &'a str,
    pub success: &'a str,
    pub info: &'a str,
}

/// No messages: the default of the macros' `flash` argument.
pub const NO_FLASH: FlashMap<'static> = FlashMap {
    error: "",
    warning: "",
    success: "",
    info: "",
};

impl FlashMap<'_> {
    pub fn message(&self, kind: StatusKind) -> &str {
        match kind {
            StatusKind::Error => self.error,
            StatusKind::Warning => self.warning,
            StatusKind::Success => self.success,
            StatusKind::Info => self.info,
        }
    }
}

/// Which banner a [`ConnectionBanners`] pair renders revealed and unbound, for
/// presentation only.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Revealed {
    #[default]
    None,
    Client,
    Server,
}

impl Revealed {
    pub fn is_client(&self) -> bool {
        matches!(self, Self::Client)
    }

    pub fn is_server(&self) -> bool {
        matches!(self, Self::Server)
    }

    /// The `data-connection` binding of the banner for `kind`: a presentation
    /// copy is bound to no connection event.
    pub fn binding(&self, kind: &'static str) -> &'static str {
        match self {
            Self::None => kind,
            Self::Client | Self::Server => "",
        }
    }
}

/// A page-level message. Nothing renders while `message` is empty.
#[derive(Template, Debug, Default)]
#[template(
    source = r#"{% import "feedback.html" as ui %}{% call ui::flash(kind=kind, id=id, title=title, message=message, spinner=spinner, hidden=hidden, auto_dismiss=auto_dismiss, connection=connection) %}{% endcall %}"#,
    ext = "html"
)]
pub struct Flash<'a> {
    pub kind: StatusKind,
    pub id: &'a str,
    pub title: &'a str,
    pub message: &'a str,
    pub spinner: bool,
    pub hidden: bool,
    pub auto_dismiss: bool,
    pub connection: &'a str,
}

impl<'a> Flash<'a> {
    /// A flash with the auto-dismiss its kind carries.
    pub fn new(kind: StatusKind, message: &'a str) -> Self {
        Self {
            kind,
            message,
            auto_dismiss: kind.auto_dismisses(),
            ..Self::default()
        }
    }
}

/// The two banners the browser reveals when the connection drops. `id` derives
/// both banner ids, so a container carries its own pair.
#[derive(Template, Debug, Default)]
#[template(
    source = r#"{% import "feedback.html" as ui %}{% call ui::connection_banners(id=id, revealed=revealed) %}{% endcall %}"#,
    ext = "html"
)]
pub struct ConnectionBanners<'a> {
    pub id: &'a str,
    pub revealed: Revealed,
}

/// The layout's flash outlet: a permanent polite live region at the top right
/// stacking messages most severe first, with the connection pair.
#[derive(Template, Debug, Default)]
#[template(
    source = r#"{% import "feedback.html" as ui %}{% call ui::flash_group(id=id, messages=flash) %}{% endcall %}"#,
    ext = "html"
)]
pub struct FlashGroup<'a> {
    pub id: &'a str,
    pub flash: FlashMap<'a>,
}

/// The flash outlet that arrives after the app shell, outside any page body:
/// pinned to the top right so a message that came with a redirect stays in view.
#[derive(Template, Debug, Default)]
#[template(
    source = r#"{% import "feedback.html" as ui %}{% call ui::flash_group(id=id, messages=flash, floating=true) %}{% endcall %}"#,
    ext = "html"
)]
pub struct FloatingFlashGroup<'a> {
    pub id: &'a str,
    pub flash: FlashMap<'a>,
}

/// An inline status alert.
#[derive(Template, Debug, Default)]
#[template(
    source = r#"{% import "feedback.html" as ui %}{% call ui::alert(kind=kind, id=id, class=class) %}{{ body|safe }}{% endcall %}"#,
    ext = "html"
)]
pub struct Alert<'a> {
    pub kind: StatusKind,
    pub id: &'a str,
    pub class: &'a str,
    pub body: Html,
}

/// The problems that stopped a save, listed above the form once the person
/// presses its submit button (the pattern is
/// GOV.UK's error summary). Each item links to its field and repeats the
/// field's own error sentence; the inline errors stay. An empty summary
/// renders nothing, so the owner can always render it.
#[derive(Template, Clone, Debug, Default)]
#[template(
    source = r#"{% import "feedback.html" as ui %}{% call ui::error_summary(summary=self) %}{% endcall %}"#,
    ext = "html"
)]
pub struct ErrorSummary {
    pub id: String,
    pub items: Vec<SummaryItem>,
}

/// One problem: the id of the field it is about and its error sentence.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SummaryItem {
    pub target: String,
    pub message: String,
}

impl ErrorSummary {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            items: Vec::new(),
        }
    }

    pub fn push(&mut self, target: impl Into<String>, message: impl Into<String>) {
        self.items.push(SummaryItem {
            target: target.into(),
            message: message.into(),
        });
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn heading(&self) -> String {
        match self.items.len() {
            1 => "1 thing to fix before saving".to_owned(),
            count => format!("{count} things to fix before saving"),
        }
    }
}

/// What the last action on a panel did; `dismiss_event` is the browser event
/// the Dismiss control raises.
#[derive(Template, Debug, Default)]
#[template(
    source = r#"{% import "feedback.html" as ui %}{% call ui::panel_notice(id=id, kind=kind, dismiss_event=dismiss_event, class=class) %}{{ body|safe }}{% endcall %}"#,
    ext = "html"
)]
pub struct PanelNotice<'a> {
    pub id: &'a str,
    pub kind: NoticeKind,
    pub dismiss_event: &'a str,
    pub class: &'a str,
    pub body: Html,
}

/// `Narrow` for a single-column form, `Wide` for a two-column one, `Compact`
/// for a confirmation that holds no form.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ModalWidth {
    Compact,
    #[default]
    Narrow,
    Wide,
}

impl ModalWidth {
    pub fn class(&self) -> &'static str {
        match self {
            Self::Compact => "max-w-md",
            Self::Narrow => "max-w-lg",
            Self::Wide => "max-w-2xl",
        }
    }
    /// The rounded dialog surface every modal shares, at this width.
    pub fn dialog_class(&self) -> String {
        format!(
            "mx-auto mt-16 mb-4 max-h-[calc(100%-5rem)] w-[calc(100%-2rem)] flex-col overflow-hidden \
             open:flex rounded-xl border border-line bg-surface py-2 text-sm text-ink shadow-lg \
             focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-brand-strong/30 \
             backdrop:bg-ink/40 {}",
            self.class()
        )
    }
}

/// A modal dialog. The server decides whether it exists: render it while the
/// workflow is in progress and stop rendering it whatever the outcome.
/// `cancel_event` is raised on Escape or any close request. Pass `flash` only
/// when the page's flash must render inside the dialog, because the page behind
/// an open modal is inert.
#[derive(Template, Debug, Default)]
#[template(
    source = r#"{% import "dialog.html" as ui %}{% call ui::modal(id=id, title=title, cancel_event=cancel_event, width=width, description=description, role=role, owns_flash=flash.is_some(), messages=flash.unwrap_or_default()) %}{{ body|safe }}{% endcall %}"#,
    ext = "html"
)]
pub struct Modal<'a> {
    pub id: &'a str,
    pub title: &'a str,
    pub cancel_event: &'a str,
    pub width: ModalWidth,
    pub description: &'a str,
    pub role: &'a str,
    pub flash: Option<FlashMap<'a>>,
    pub body: Html,
}

/// A confirmation for an action that cannot be undone. `consequence` is the
/// title and accessible name, `detail` the description; Cancel comes first and
/// takes focus, and `confirm` is the verb alone. `working` replaces the
/// confirm's label for the round trip.
#[derive(Template, Debug, Default)]
#[template(
    source = r#"{% import "dialog.html" as ui %}{% call ui::confirm_dialog(id=id, consequence=consequence, detail=detail, confirm=confirm, working=working, confirm_event=confirm_event, cancel_event=cancel_event) %}{% endcall %}"#,
    ext = "html"
)]
pub struct ConfirmDialog<'a> {
    pub id: &'a str,
    pub consequence: &'a str,
    pub detail: &'a str,
    pub confirm: &'a str,
    pub working: &'a str,
    pub confirm_event: &'a str,
    pub cancel_event: &'a str,
}

/// One tab. A tab with an `href` is a link; one without is a button that
/// raises `event` with `value`. A `patch` link names its own page with other
/// params: live.js reads it in place as a params update
/// and the address is pushed, so each tab is addressable and Back returns to it.
#[derive(Clone, Debug, Default)]
pub struct Tab<'a> {
    pub id: &'a str,
    pub href: &'a str,
    pub patch: bool,
    pub current: bool,
    pub event: &'a str,
    pub value: &'a str,
    pub label: Html,
}

impl<'a> Tab<'a> {
    pub fn link(href: &'a str, label: &str, current: bool) -> Self {
        Self {
            href,
            current,
            label: Html::text(label),
            ..Self::default()
        }
    }

    /// A link to this page's own address with the tab's params.
    pub fn patch(href: &'a str, label: &str, current: bool) -> Self {
        Self {
            patch: true,
            ..Self::link(href, label, current)
        }
    }

    pub fn button(event: &'a str, value: &'a str, label: &str, current: bool) -> Self {
        Self {
            event,
            value,
            current,
            label: Html::text(label),
            ..Self::default()
        }
    }

    /// The class list of the tab, selected or not.
    pub fn class(&self) -> &'static str {
        if self.current {
            "-mb-px shrink-0 whitespace-nowrap border-b-2 px-2 py-1.5 text-sm transition focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-brand-strong/40 border-brand-strong font-medium text-ink-strong"
        } else {
            "-mb-px shrink-0 whitespace-nowrap border-b-2 px-2 py-1.5 text-sm transition focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-brand-strong/40 border-transparent text-ink-muted hover:text-ink"
        }
    }
}

/// A compact tab strip for sibling views of one page.
#[derive(Template, Debug, Default)]
#[template(
    source = r#"{% import "tabs.html" as ui %}{% call ui::tabs(id=id, label=label, items=tabs, class=class) %}{% endcall %}"#,
    ext = "html"
)]
pub struct Tabs<'a> {
    pub id: &'a str,
    pub label: &'a str,
    pub class: &'a str,
    pub tabs: Vec<Tab<'a>>,
}
