//! The shell's page search: the top bar's search button (`shell.html`), or `/`
//! outside a field, opens this dialog, and `assets/shell.js` shows the pages
//! whose labels match as the person types. Bilibi's own chrome, with no Bilimbi
//! module counterpart. The host decides which pages the person may open and
//! where each lives; this module only renders them.
use crate::forms::{field_base_class, Field, InputKind};
use crate::{Html, ModalWidth};
use askama::Template;

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

/// The search dialog over `pages`: Pages first, then Account.
pub fn page_search(pages: &[SearchPage]) -> askama::Result<Html> {
    // A plain text field beside the search icon: a search field would add the
    // browser's own clear "×" next to the dialog's Close.
    let input = Field::named("app-search-input")
        .kind(InputKind::Text)
        .placeholder("Search…")
        .aria_label("Search pages")
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
        dialog_class: ModalWidth::Narrow.dialog_class(),
        groups: [
            ("pages", "Pages", group(false)),
            ("account", "Account", group(true)),
        ],
    })
}

#[derive(Template)]
#[template(path = "page_search.html")]
struct PageSearch<'a> {
    input: Field,
    dialog_class: String,
    groups: [(&'static str, &'static str, Vec<&'a SearchPage>); 2],
}
