//! Server-driven multi-record choice, combining the `combobox` and
//! `multi_select` form components. The owner searches and owns the selection.
use askama::Template;
use sha2::{Digest, Sha256};

/// A selected record or one bounded search result.
#[derive(Clone, Debug)]
pub struct SearchMultiOption {
    pub value: String,
    pub label: String,
    pub detail: String,
    pub disabled: bool,
    pub note: String,
    note_href: Option<String>,
}
impl SearchMultiOption {
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
            detail: String::new(),
            disabled: false,
            note: String::new(),
            note_href: None,
        }
    }
    /// Stable DOM key without embedding a record value in an identifier.
    fn key(&self) -> String {
        let digest = Sha256::digest(self.value.as_bytes());
        digest.iter().map(|byte| format!("{byte:02x}")).collect()
    }
    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }
    pub fn disabled(mut self, note: impl Into<String>, href: Option<String>) -> Self {
        self.disabled = true;
        self.note = note.into();
        self.note_href = href.filter(|url| {
            ((url.starts_with('/') && !url.starts_with("//")) || url.starts_with('#'))
                && !url.contains(|ch: char| ch == '\\' || ch.is_control())
        });
        self
    }
}

/// The owner handles search (`query`), pick/remove (`value`) and optional
/// create (`query`) events, and renders selected records independently of hits.
/// Search and selection must be authorized again by the owner.
#[derive(Template, Clone, Debug)]
#[template(path = "search_multi.html")]
pub struct SearchMultiSelect {
    pub id: String,
    pub name: String,
    pub label: String,
    pub query: String,
    pub selected: Vec<SearchMultiOption>,
    results: Vec<SearchMultiOption>,
    pub search_event: String,
    pub pick_event: String,
    pub remove_event: String,
    pub create_event: String,
    pub create_label: String,
    pub wrap_chips: bool,
}
impl SearchMultiSelect {
    /// `name` is the submitted array name; event names are owner vocabulary.
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        label: impl Into<String>,
        search: impl Into<String>,
        pick: impl Into<String>,
        remove: impl Into<String>,
    ) -> Self {
        let name = name.into();
        Self {
            id: id.into(),
            name: if name.ends_with("[]") {
                name
            } else {
                format!("{name}[]")
            },
            label: label.into(),
            query: String::new(),
            selected: Vec::new(),
            results: Vec::new(),
            search_event: search.into(),
            pick_event: pick.into(),
            remove_event: remove.into(),
            create_event: String::new(),
            create_label: String::new(),
            wrap_chips: false,
        }
    }
    pub fn query(mut self, query: impl Into<String>) -> Self {
        self.query = query.into();
        self
    }
    pub fn selected(mut self, selected: Vec<SearchMultiOption>) -> Self {
        self.selected = selected;
        self
    }
    /// Bounds rendering as well as requiring the owner to bound its search.
    pub fn results(mut self, mut results: Vec<SearchMultiOption>) -> Self {
        results.truncate(50);
        self.results = results;
        self
    }
    pub fn create(mut self, label: impl Into<String>, event: impl Into<String>) -> Self {
        self.create_label = label.into();
        self.create_event = event.into();
        self
    }
    pub fn wrap_chips(mut self) -> Self {
        self.wrap_chips = true;
        self
    }
    pub fn is_selected(&self, value: &str) -> bool {
        self.selected.iter().any(|item| item.value == value)
    }
}
