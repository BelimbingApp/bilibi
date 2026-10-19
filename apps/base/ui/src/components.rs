//! Typed variants and view models behind the shared macros in
//! `templates/components.html`. The macros own the DOM; these types own the
//! closed vocabularies (kind, variant, context) and the class decisions that
//! are logic rather than markup. Each wrapper struct renders through the same
//! macro, so there is one markup source per component.
//!
//! Counterparts in Bilimbi `apps/base/ui/lib/ui/components.ex` at the pinned
//! source: `page`, `header`, `card`, `section_heading`, `stat_strip`, `list`
//! (here `facts`), `button`, `icon_button`, `create_button`, `back_link`,
//! `action_link`, `badge` and `Components.Icon.icon`.
use crate::Html;
use askama::Template;

/// A button is a control, so it wears the ledger geometry every control wears.
/// Each variant owns every color property it sets, including the focus ring.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ButtonVariant {
    #[default]
    Secondary,
    Primary,
    Danger,
}

impl ButtonVariant {
    pub fn class(&self) -> &'static str {
        match self {
            Self::Primary => {
                "border-transparent bg-action text-action-ink hover:bg-action-hover shadow-xs focus-visible:ring-brand-strong/30"
            }
            Self::Danger => {
                "border-transparent text-danger hover:bg-danger-surface hover:text-danger-ink hover:underline focus-visible:ring-brand-strong/30"
            }
            Self::Secondary => {
                "border-high-contrast-line bg-surface text-ink hover:bg-surface-sunken shadow-xs focus-visible:ring-brand-strong/30"
            }
        }
    }
}

/// Neutral by default; a status color for entity states such as archived.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BadgeKind {
    #[default]
    Neutral,
    Success,
    Warning,
    Danger,
    /// The pill with no colour: the source's `badge` paints only the four
    /// kinds above, and a kind outside them (Audit's `:info` for an updated
    /// record) renders the bare pill. Parity keeps that rendering.
    Plain,
}

impl BadgeKind {
    pub fn class(&self) -> &'static str {
        match self {
            Self::Neutral => "bg-surface-muted text-ink-muted",
            Self::Success => "bg-success-surface text-success-ink",
            Self::Warning => "bg-warning-surface text-warning-ink",
            Self::Danger => "bg-danger-surface text-danger-ink",
            Self::Plain => "",
        }
    }
}

/// `Inline` sits beside a heading or label; `Table` is the repeated table and
/// toolbar action.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IconContext {
    Inline,
    #[default]
    Table,
}

impl IconContext {
    pub fn is_inline(&self) -> bool {
        matches!(self, Self::Inline)
    }

    /// The glyph size of an icon button in this context.
    pub fn icon_class(&self) -> &'static str {
        match self {
            Self::Inline => "size-3.5",
            Self::Table => "size-4",
        }
    }

    /// The glyph of a busy icon button: the same size, spinning.
    pub fn spinner_class(&self) -> &'static str {
        match self {
            Self::Inline => "size-3.5 motion-safe:animate-spin",
            Self::Table => "size-4 motion-safe:animate-spin",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IconKind {
    #[default]
    Neutral,
    Danger,
}

impl IconKind {
    pub fn is_neutral(&self) -> bool {
        matches!(self, Self::Neutral)
    }
}

/// `Nav` is the sidebar pin and tile control: it renders the `nav-icon-button`
/// utility from `assets/css/app.css` instead of repeating that utility list on
/// every button. Other icon buttons keep `Utilities`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Chrome {
    #[default]
    Utilities,
    Nav,
}

impl Chrome {
    pub fn is_nav(&self) -> bool {
        matches!(self, Self::Nav)
    }
}

/// Every screen is one of three kinds, and the width is chosen here and
/// nowhere else: `List` and `Detail` share `max-w-7xl`, `Form` is `max-w-2xl`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PageVariant {
    #[default]
    List,
    Form,
    Detail,
}

impl PageVariant {
    /// The `data-page` value the stylesheet's list-fill rules read.
    pub fn name(&self) -> &'static str {
        match self {
            Self::List => "list",
            Self::Form => "form",
            Self::Detail => "detail",
        }
    }

    /// Every variant shares the list's column, so title, subtitle and Back
    /// sit in the same place on every page; a form keeps its header that
    /// wide and stops the form below it at about two fields side by side.
    pub fn width(&self) -> &'static str {
        match self {
            Self::List | Self::Detail => "max-w-7xl",
            Self::Form => "max-w-7xl [&>:not([data-page-header])]:max-w-2xl",
        }
    }
}

/// Numeric values use the larger tabular treatment; `Text` is a storage or
/// scope description.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StatKind {
    #[default]
    Number,
    Text,
}

impl StatKind {
    pub fn class(&self) -> &'static str {
        match self {
            Self::Text => "truncate text-sm",
            Self::Number => "text-xl tabular-nums",
        }
    }
}

/// One label and value of a stat strip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatItem {
    pub label: String,
    pub value: String,
    pub kind: StatKind,
}

impl StatItem {
    pub fn number(label: impl Into<String>, value: impl ToString) -> Self {
        Self {
            label: label.into(),
            value: value.to_string(),
            kind: StatKind::Number,
        }
    }

    pub fn text(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
            kind: StatKind::Text,
        }
    }
}

/// The one treatment for a demoted secondary action: a quiet labelled control
/// in `text-link` that darkens on hover. `back_link` and `action_link` apply it
/// themselves; it is public for the one member that is structurally not a
/// link, a disclosure button such as the record-history trigger. Do not use it
/// to style a control that changes data: that control is a button.
pub const DEMOTED_ACTION_CLASS: &str = "inline-flex items-center gap-1 whitespace-nowrap text-sm text-link transition-colors hover:text-ink focus-visible:outline-none focus-visible:rounded-sm focus-visible:ring-1 focus-visible:ring-brand-strong/40";

/// The default `count` of `section_heading`: no badge.
pub const NO_COUNT: Option<usize> = None;

fn class_tokens(class: &str) -> impl Iterator<Item = &str> {
    class.split_whitespace()
}

/// `inner_class` carrying `p-0` is the flat-corner signal: a card framing a
/// table and its pager drops its radius so the table picks up no corner.
pub fn card_flat(inner_class: &str) -> bool {
    class_tokens(inner_class).any(|token| token == "p-0")
}

/// The shared card owns its padding: 12px, or zero for a flat table frame.
/// Retain layout classes but discard legacy padding overrides, including
/// responsive ones, so an existing caller cannot undo the compact scale.
pub fn card_body_class(inner_class: &str) -> String {
    let mut classes = vec![if card_flat(inner_class) {
        "p-0"
    } else {
        "p-panel"
    }];
    classes.extend(class_tokens(inner_class).filter(|token| {
        let utility = token.rsplit(':').next().unwrap_or(token);
        !["p-", "px-", "py-", "pt-", "pr-", "pb-", "pl-", "ps-", "pe-"]
            .iter()
            .any(|prefix| utility.starts_with(prefix))
    }));
    classes.join(" ")
}

/// The shared button owns its size: 24 px with 8 px side padding, on every
/// pointer and width (`DESIGN.md`, Compact scale). Retain a caller's layout
/// classes (`w-full`, `ml-auto`) but discard its padding, height and text-size
/// utilities, including responsive and state-prefixed ones, so a page cannot
/// give itself a larger or smaller button. The `button` macro applies it.
pub fn button_class(class: &str) -> String {
    const SIZE_PREFIXES: [&str; 13] = [
        "p-", "px-", "py-", "pt-", "pr-", "pb-", "pl-", "ps-", "pe-", "h-", "min-h-", "max-h-",
        "leading-",
    ];
    const TEXT_SIZES: [&str; 10] = [
        "text-xs",
        "text-sm",
        "text-base",
        "text-lg",
        "text-xl",
        "text-2xl",
        "text-3xl",
        "text-4xl",
        "text-5xl",
        "text-[",
    ];
    class_tokens(class)
        .filter(|token| {
            let utility = token.rsplit(':').next().unwrap_or(token);
            !SIZE_PREFIXES
                .iter()
                .any(|prefix| utility.starts_with(prefix))
                && !TEXT_SIZES.iter().any(|size| {
                    utility == *size
                        || utility.starts_with(&format!("{size}/"))
                        || (size.ends_with('[') && utility.starts_with(size))
                })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn stat_strip_columns(count: usize) -> &'static str {
    match count {
        1 => "grid-cols-1",
        2 => "grid-cols-2",
        _ => "grid-cols-3",
    }
}

/// A named action icon or Heroicon; see `icons`.
#[derive(Template, Debug, Default)]
#[template(
    source = r#"{% import "components.html" as ui %}{% call ui::icon(name=name, class=class) %}{% endcall %}"#,
    ext = "html"
)]
pub struct Icon<'a> {
    pub name: &'a str,
    pub class: &'a str,
}

impl<'a> Icon<'a> {
    pub fn new(name: &'a str) -> Self {
        Self {
            name,
            class: "size-4",
        }
    }
}

/// A compact status badge. The dot marks a status; a badge that names a type,
/// role or category passes `dot: false`.
#[derive(Template, Debug, Default)]
#[template(
    source = r#"{% import "components.html" as ui %}{% call ui::badge(kind=kind, dot=dot, class=class) %}{{ body|safe }}{% endcall %}"#,
    ext = "html"
)]
pub struct Badge<'a> {
    pub kind: BadgeKind,
    pub dot: bool,
    pub class: &'a str,
    pub body: Html,
}

impl Badge<'_> {
    pub fn status(kind: BadgeKind, text: &str) -> Self {
        Self {
            kind,
            dot: true,
            class: "",
            body: Html::text(text),
        }
    }
}

/// A button, or the same control as a link when `href` is set. `busy` renders
/// `aria-busy` and disables the control; a link ignores it. `event` is the
/// browser adapter's `data-event` name; `working` is the label the browser
/// shows while the click's round trip is in flight.
#[derive(Template, Debug, Default)]
#[template(
    source = r#"{% import "components.html" as ui %}{% call ui::button(variant=variant, class=class, href=href, id=id, button_type=button_type, name=name, value=value, disabled=disabled, busy=busy, aria_busy=aria_busy, event=event, working=working, download=download) %}{{ body|safe }}{% endcall %}"#,
    ext = "html"
)]
pub struct Button<'a> {
    pub variant: ButtonVariant,
    pub class: &'a str,
    pub href: &'a str,
    pub id: &'a str,
    pub button_type: &'a str,
    pub name: &'a str,
    pub value: &'a str,
    pub disabled: bool,
    pub busy: bool,
    /// Announces an in-flight control while retaining its disabled appearance.
    /// `busy` also adds a spinner; Bilimbi's in-flight specimen does not.
    pub aria_busy: bool,
    pub event: &'a str,
    pub working: &'a str,
    /// With `href`, the link saves its target under this file name instead
    /// of opening it (a `data:` text the page itself built, such as
    /// recovery codes that exist nowhere else).
    pub download: &'a str,
    pub body: Html,
}

/// A compact icon-only action with an accessible label.
#[derive(Template, Debug)]
#[template(
    source = r#"{% import "components.html" as ui %}{% call ui::icon_button(icon=icon, label=label, context=context, kind=kind, chrome=chrome, busy=busy, disabled=disabled, class=class, id=id, href=href, title=title, button_type=button_type, event=event, name=name, value=value, draggable=draggable, data_role=data_role, nav_pin=nav_pin, method=method) %}{% endcall %}"#,
    ext = "html"
)]
pub struct IconButton<'a> {
    pub icon: &'a str,
    pub label: &'a str,
    pub context: IconContext,
    pub kind: IconKind,
    pub chrome: Chrome,
    pub busy: bool,
    pub disabled: bool,
    pub class: &'a str,
    pub id: &'a str,
    pub href: &'a str,
    pub title: &'a str,
    pub button_type: &'a str,
    pub event: &'a str,
    /// Sent as `{name: value}` with the event, like a form button's own value.
    pub name: &'a str,
    pub value: &'a str,
    /// A drag source; `data_role` names it to a controller (`drag-handle`).
    pub draggable: bool,
    pub data_role: &'a str,
    /// A page's own "Pin to sidebar" control: the navigation item ID
    /// (`nav-admin-system-settings`) that `assets/shell.js` pins and marks.
    pub nav_pin: &'a str,
    /// With `href`, the HTTP method of a submitting action link (`post`);
    /// `ui.js` sends it with the CSRF token and follows the redirect.
    pub method: &'a str,
}

impl<'a> IconButton<'a> {
    pub fn new(icon: &'a str, label: &'a str) -> Self {
        Self {
            icon,
            label,
            context: IconContext::Table,
            kind: IconKind::Neutral,
            chrome: Chrome::Utilities,
            busy: false,
            disabled: false,
            class: "",
            id: "",
            href: "",
            title: "",
            button_type: "button",
            event: "",
            name: "",
            value: "",
            draggable: false,
            data_role: "",
            nav_pin: "",
            method: "",
        }
    }
}

/// The primary create button of a page header: "+ Noun".
#[derive(Template, Debug, Default)]
#[template(
    source = r#"{% import "components.html" as ui %}{% call ui::create_button(noun=noun, href=href, id=id, class=class, event=event, button_type=button_type) %}{% endcall %}"#,
    ext = "html"
)]
pub struct CreateButton<'a> {
    pub noun: &'a str,
    pub href: &'a str,
    pub id: &'a str,
    pub class: &'a str,
    pub event: &'a str,
    pub button_type: &'a str,
}

/// The demoted "← Back" navigation of a page header.
#[derive(Template, Debug, Default)]
#[template(
    source = r#"{% import "components.html" as ui %}{% call ui::back_link(href=href, id=id, title=title, class=class) %}{% endcall %}"#,
    ext = "html"
)]
pub struct BackLink<'a> {
    pub href: &'a str,
    pub id: &'a str,
    pub title: &'a str,
    pub class: &'a str,
}

/// A demoted secondary action as a plain link; `method` marks an action that
/// submits a request rather than navigating.
#[derive(Template, Debug, Default)]
#[template(
    source = r#"{% import "components.html" as ui %}{% call ui::action_link(id=id, icon=icon, title=title, href=href, method=method) %}{{ body|safe }}{% endcall %}"#,
    ext = "html"
)]
pub struct ActionLink<'a> {
    pub id: &'a str,
    pub icon: &'a str,
    pub title: &'a str,
    pub href: &'a str,
    pub method: &'a str,
    pub body: Html,
}

/// The page content container at the width of its workflow kind.
#[derive(Template, Debug, Default)]
#[template(
    source = r#"{% import "components.html" as ui %}{% call ui::page(id=id, variant=variant, class=class) %}{{ body|safe }}{% endcall %}"#,
    ext = "html"
)]
pub struct Page<'a> {
    pub id: &'a str,
    pub variant: PageVariant,
    pub class: &'a str,
    pub body: Html,
}

/// The page header: title, optional subtitle, title actions and trailing actions.
#[derive(Template, Debug, Default)]
#[template(
    source = r#"{% import "components.html" as ui %}{% call ui::header(subtitle=subtitle, title_actions=title_actions, actions=actions) %}{{ title|safe }}{% endcall %}"#,
    ext = "html"
)]
pub struct Header<'a> {
    pub title: Html,
    pub subtitle: &'a str,
    pub title_actions: Html,
    pub actions: Html,
}

/// The heading row of one section on a detail page.
#[derive(Template, Debug, Default)]
#[template(
    source = r#"{% import "components.html" as ui %}{% call ui::section_heading(title=title, id=id, count=count, class=class, title_actions=title_actions, description=description, actions=actions) %}{% endcall %}"#,
    ext = "html"
)]
pub struct SectionHeading<'a> {
    pub title: &'a str,
    pub id: &'a str,
    pub count: Option<usize>,
    pub class: &'a str,
    pub title_actions: Html,
    pub description: &'a str,
    pub actions: Html,
}

/// A card container with a subtle border.
#[derive(Template, Debug, Default)]
#[template(
    source = r#"{% import "components.html" as ui %}{% call ui::card(id=id, title=title, class=class, inner_class=inner_class, role=role, labelledby=labelledby) %}{{ body|safe }}{% endcall %}"#,
    ext = "html"
)]
pub struct Card<'a> {
    pub id: &'a str,
    pub title: &'a str,
    pub class: &'a str,
    pub inner_class: &'a str,
    /// A landmark role (`region`) with the heading that labels it.
    pub role: &'a str,
    pub labelledby: &'a str,
    pub body: Html,
}

/// A part of a form most people never need, folded under one line (no
/// Bilimbi counterpart): `title` names it and `summary` says what it holds
/// now ("Source: manual · Unverified"), so a closed fold still reads true.
/// It is a native `<details>`; the live patcher keeps the open state the
/// person gave it, so `open` only decides the first render, and the `Fold`
/// hook (`assets/hooks.js`) opens it whenever `open` turns true later, which
/// is how an error inside it is never hidden.
#[derive(Template, Debug, Default)]
#[template(
    source = r#"{% import "components.html" as ui %}{% call ui::fold(id=id, title=title, summary=summary, open=open) %}{{ body|safe }}{% endcall %}"#,
    ext = "html"
)]
pub struct Fold<'a> {
    pub id: &'a str,
    pub title: &'a str,
    pub summary: &'a str,
    pub open: bool,
    pub body: Html,
}

/// One row of a record's facts; `id` names the value cell.
#[derive(Clone, Debug, Default)]
pub struct Fact<'a> {
    pub title: &'a str,
    pub id: &'a str,
    pub value: Html,
}

/// The facts of one record as a definition list.
#[derive(Template, Debug, Default)]
#[template(
    source = r#"{% import "components.html" as ui %}{% call ui::facts(id=id) %}{% for item in items %}{% call ui::fact(title=item.title, id=item.id) %}{{ item.value|safe }}{% endcall %}{% endfor %}{% endcall %}"#,
    ext = "html"
)]
pub struct Facts<'a> {
    pub id: &'a str,
    pub items: Vec<Fact<'a>>,
}

/// A compact strip of related statistics; `href` makes it a quiet link.
#[derive(Template, Debug, Default)]
#[template(
    source = r#"{% import "components.html" as ui %}{% call ui::stat_strip(id=id, title=title, items=items, href=href) %}{% endcall %}"#,
    ext = "html"
)]
pub struct StatStrip<'a> {
    pub id: &'a str,
    pub title: &'a str,
    pub href: &'a str,
    pub items: Vec<StatItem>,
}
