//! Dragging a record onto a page that can act on it (issue 265). The source
//! carries a reference only (`kind:id`, `FlexRow::record`); a target page
//! wraps itself in a [`DropZone`] (`ui::page`'s `drop`) and receives a
//! `drop_records` event; the same shortcut is always also an [`AddToMenu`]
//! item, a plain link, so a phone, the keyboard, a screen reader and an AI
//! agent reach it without a drag. Which pages accept which kinds is
//! `base/records`' business (`bilibi_records::DropTarget`); this module only
//! draws it. Browser half: `assets/drops.js`. Contract: `DESIGN.md`.
use crate::Html;
use askama::Template;

/// A page that takes dropped records. Render [`DropZone::attributes`] on the
/// page container (`ui::page(drop=…)`); the browser lights the tile while a
/// record it accepts is dragged and sends the drop to the page as the
/// `drop_records` event.
#[derive(Clone, Debug, Default)]
pub struct DropZone {
    /// The record kinds the page accepts.
    pub accepts: Vec<String>,
    /// The most records one drop may carry.
    pub max: usize,
    /// What a drop does, as the tile says it while a record is over it.
    pub label: String,
}
impl DropZone {
    pub fn new(accepts: &[&str], max: usize, label: &str) -> Self {
        Self {
            accepts: accepts.iter().map(|kind| (*kind).to_owned()).collect(),
            max,
            label: label.to_owned(),
        }
    }

    /// The container's attributes; nothing for an inactive zone.
    pub fn attributes(&self) -> Html {
        if self.accepts.is_empty() {
            return Html::EMPTY;
        }
        let escape = |text: &str| Html::text(text).to_string();
        Html::raw(format!(
            " data-drop-zone data-drop-accepts=\"{}\" data-drop-max=\"{}\" data-drop-label=\"{}\"",
            escape(&self.accepts.join(" ")),
            self.max,
            escape(&self.label),
        ))
    }
}

/// One shortcut in the "Add to…" menu: a link to the target page that opens
/// its review on the records. A target that takes fewer records than are
/// ticked stays in the menu, disabled, with the reason as its title.
#[derive(Clone, Debug)]
pub struct AddToItem {
    pub label: String,
    pub href: String,
    pub disabled: Option<String>,
}

/// The "Add to…" menu of a row or of a selection bar.
#[derive(Template, Clone, Debug)]
#[template(
    source = r##"{%- import "components.html" as ui -%}
{%- if !items.is_empty() -%}
<span id="{{ id }}-wrap" data-hook="DisclosureDismiss" class="floating-scope relative inline-block"><button id="{{ id }}" type="button" aria-expanded="false" aria-controls="{{ id }}-items" title="{{ title }}" class="peer floating-anchor inline-flex h-6 items-center gap-1 whitespace-nowrap rounded-md border border-line bg-surface px-2 text-xs text-ink hover:bg-surface-sunken focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-brand-strong/40">{% call ui::icon(name="create", class="size-3.5") %}{% endcall %}Add to…</button><div id="{{ id }}-items" class="floating-list z-40 mt-0.5 hidden min-w-40 rounded-md border border-line bg-surface p-1 text-left shadow-lg peer-aria-expanded:block">{% for item in items %}{% if let Some(reason) = item.disabled %}<span id="{{ id }}-{{ loop.index0 }}" aria-disabled="true" title="{{ reason }}" class="block cursor-not-allowed rounded-sm px-2 py-1 text-xs text-ink-faint">{{ item.label }}</span>{% else %}<a id="{{ id }}-{{ loop.index0 }}" href="{{ item.href }}" class="block rounded-sm px-2 py-1 text-xs text-ink hover:bg-surface-muted focus-visible:bg-surface-muted focus-visible:outline-none">{{ item.label }}</a>{% endif %}{% endfor %}</div></span>
{%- endif -%}"##,
    ext = "html"
)]
pub struct AddToMenu {
    /// A stable id for this menu on the page.
    pub id: String,
    pub items: Vec<AddToItem>,
    /// The menu button's title: what it adds ("Add this contact to…").
    pub title: String,
}
impl AddToMenu {
    pub fn new(id: impl Into<String>, title: impl Into<String>, items: Vec<AddToItem>) -> Self {
        Self {
            id: id.into(),
            items,
            title: title.into(),
        }
    }
}

/// The drag attributes of a record row: `kind:id`, the one spelling the drop
/// event and the link carry. The browser (`assets/drops.js`) reads them.
pub fn drag_attributes(kind: &str, id: &str) -> Html {
    Html::raw(format!(
        " draggable=\"true\" data-drag-record=\"{}\"",
        Html::text(&format!("{kind}:{id}"))
    ))
}
