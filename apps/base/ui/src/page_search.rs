//! The shell's page search: the top bar's search button (`shell.html`), or `/`
//! outside a field, opens this dialog, and `assets/shell.js` shows the pages
//! whose labels match as the person types. Bilibi's own chrome. The host decides which pages the person may open and
//! where each lives; this module only renders them.
//!
//! Records come from the module that answers the `shell.search.records` shell
//! embed (Base Search): the host renders it into the dialog between Pages and
//! Account, and the module draws its rows through [`SearchRecords`], so a
//! record row and a page row are one element.
use crate::forms::{field_base_class, Field, InputKind};
use crate::{Html, ModalWidth};
use askama::Template;

/// The classes of one row of the dialog, a page or a record.
const OPTION_CLASS: &str = "flex min-w-0 flex-col rounded-md px-2 py-1 text-link hover:bg-surface-muted aria-selected:bg-brand-surface aria-selected:text-brand-strong max-sm:py-3";

/// One page the signed-in person may open.
#[derive(Clone, Debug)]
pub struct SearchPage {
    pub label: String,
    /// Where it lives, outermost first and ending with the page itself
    /// (`Administration`, `Authorization`, `Roles`); empty for a page that
    /// sits in no section.
    pub location: Vec<String>,
    pub href: String,
    /// A personal settings page, listed under Account.
    pub account: bool,
}

impl SearchPage {
    /// The location as shown: `Administration › Authorization › Roles`.
    pub fn path(&self) -> String {
        self.location.join(" › ")
    }
    /// What a query is matched against, one label per line: the page's own
    /// and each place it lives under, so a matching section finds its pages.
    pub fn labels(&self) -> String {
        std::iter::once(&self.label)
            .chain(&self.location)
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// The search dialog over `pages`: Pages first, then `records` (the rendered
/// `shell.search.records` embed, empty when no module answers it), then
/// Account.
pub fn page_search(pages: &[SearchPage], records: Html) -> askama::Result<Html> {
    let label = if records.is_empty() {
        "Search pages"
    } else {
        "Search pages and records"
    };
    // A plain text field beside the search icon: a search field would add the
    // browser's own clear "×" next to the dialog's Close.
    let input = Field::named("app-search-input")
        .kind(InputKind::Text)
        .placeholder("Search…")
        .aria_label(label)
        .autocomplete("off")
        .wrapper_class("mb-0")
        .class(field_base_class(false, "pl-8 pr-3"))
        .attr("role", "combobox")
        .attr("aria-autocomplete", "list")
        .attr("aria-controls", "app-search-results")
        .attr("aria-expanded", "false");
    let group = |account: bool| {
        pages
            .iter()
            .filter(|page| page.account == account)
            .collect()
    };
    Html::render(&PageSearch {
        input,
        label,
        dialog_class: ModalWidth::Narrow.dialog_class(),
        option_class: OPTION_CLASS,
        groups: [
            ("pages", "Pages", group(false)),
            ("account", "Account", group(true)),
        ],
        records,
    })
}

#[derive(Template)]
#[template(path = "page_search.html")]
struct PageSearch<'a> {
    input: Field,
    label: &'static str,
    dialog_class: String,
    option_class: &'static str,
    groups: [(&'static str, &'static str, Vec<&'a SearchPage>); 2],
    records: Html,
}

/// The records the page search's query found: the root element of the
/// `shell.search.records` panel, one group per kind of record, then a row that
/// opens every result. The module that answers the embed decides what was
/// found and may be opened; this only draws it. `shell.js` sends the panel the
/// query once it is `min_chars` long and never filters these rows itself.
#[derive(Template, Debug, Default)]
#[template(path = "page_search_records.html")]
pub struct SearchRecords<'a> {
    /// Whether this person may search records at all. Off, the root stays in
    /// place, empty and hidden, and the dialog searches pages only.
    pub enabled: bool,
    /// The query these rows answer, as the panel read it: `shell.js` compares
    /// it with the field to know whether an answer is still on its way.
    pub query: &'a str,
    pub min_chars: usize,
    pub groups: Vec<RecordGroup>,
    /// Where every result for `query` opens; empty adds no such row.
    pub all_href: &'a str,
    /// Why nothing is listed, when that is not plain "no match" (too many
    /// matches to rank); empty for none.
    pub notice: &'a str,
}

impl SearchRecords<'_> {
    fn option_class(&self) -> &'static str {
        OPTION_CLASS
    }
}

/// The records of one kind, in the order the search ranked them.
#[derive(Clone, Debug, Default)]
pub struct RecordGroup {
    /// A DOM-safe id for the kind (`persons`).
    pub id: String,
    /// The kind as a person names it (`Contacts`).
    pub label: String,
    pub hits: Vec<RecordHit>,
}

/// One record the person may open.
#[derive(Clone, Debug, Default)]
pub struct RecordHit {
    pub title: String,
    /// The facts shown under the title (`ACME-01 · Acme Sdn Bhd`); empty for none.
    pub detail: String,
    /// The record's state when it is no longer current (`Retired`); empty while current.
    pub state: String,
    pub href: String,
}
