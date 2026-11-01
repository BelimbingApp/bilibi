//! Typed models behind the operational-list macros in `templates/lists.html`:
//! `filter_toolbar`, `pagination`, `table` with its sort heading and
//! `empty_state`.
//!
//! The macros own the DOM. State stays in [`crate::list_state::ListState`]:
//! the toolbar and the page-size form post `FILTERS_FORM[key]` fields with
//! the `filters` event, the pager raises `page` with `page`, and a sortable
//! heading raises `sort` with `sort`; the owning page applies them and returns
//! `to_params()` as its canonical query.
use crate::forms::{field_base_class, Choice, Field, InputKind, MultiSelect};
use crate::list_state::{PageCounts, SortDir, FILTERS_FORM};
use crate::Html;
use askama::Template;

/// Pagination controls under a list: the summary and, with more than one page,
/// the rows-per-page select and previous/numbers/next. An empty list shows none.
#[derive(Template, Clone, Debug)]
#[template(
    source = r#"{% import "lists.html" as lists %}{% call lists::pagination(p=self) %}{% endcall %}"#,
    ext = "html"
)]
pub struct Pagination<'a> {
    pub id: &'a str,
    pub counts: PageCounts,
    pub page_sizes: &'a [u64],
    pub filters_event: &'a str,
    pub page_event: &'a str,
    /// `data-target` of a mounted panel; empty for the page itself.
    pub target: &'a str,
}

impl<'a> Pagination<'a> {
    pub fn new(id: &'a str, counts: PageCounts, page_sizes: &'a [u64]) -> Self {
        Self {
            id,
            counts,
            page_sizes,
            filters_event: "filters",
            page_event: "page",
            target: "",
        }
    }

    /// The `perPage` select; its form is the pagination's own small form.
    pub fn size_field(&self) -> Field {
        Field::in_form(FILTERS_FORM, "perPage")
            .id(format!("{}-page-size", self.id))
            .kind(InputKind::Select)
            .label("Rows per page")
            .label_class("sr-only")
            .wrapper_class("mb-0")
            .value(self.counts.page_size.to_string())
            .options(
                self.page_sizes
                    .iter()
                    .map(|size| Choice::new(size.to_string(), size.to_string()))
                    .collect(),
            )
            .class("h-7 w-auto rounded-md border border-line bg-surface py-0 pl-2 pr-6 text-xs tabular-nums text-ink focus:border-brand-strong focus:outline-none focus:ring-1 focus:ring-brand-strong/30")
    }

    pub fn steps(&self) -> Vec<Option<u64>> {
        self.counts.steps()
    }

    pub fn previous(&self) -> u64 {
        self.counts.page.saturating_sub(1)
    }

    pub fn next(&self) -> u64 {
        self.counts.page.saturating_add(1)
    }
}

#[derive(Clone, Debug)]
pub enum ControlKind {
    Search,
    Select,
    MultiSelect(Box<MultiSelect>),
    Date,
}

/// One toolbar cell. Framing, the magnifier, debounce, length cap and the
/// screen-reader-only label are the toolbar's, never the caller's.
#[derive(Clone, Debug)]
pub struct ToolbarControl {
    pub kind: ControlKind,
    pub id: String,
    pub label: String,
    pub hint: String,
    pub field: Field,
}

impl ToolbarControl {
    /// A search box posting `filters[key]`.
    pub fn search(id: &str, key: &str, label: &str, value: &str, placeholder: &str) -> Self {
        let field = Field::in_form(FILTERS_FORM, key)
            .id(id)
            .kind(InputKind::Search)
            .wrapper_class("mb-0")
            .value(value)
            .placeholder(placeholder)
            .attr("data-debounce", "300")
            .autocomplete("off")
            .class(field_base_class(false, "pl-8 pr-3"));
        let mut field = field;
        field.attrs.maxlength = Some("255".into());
        Self::new(ControlKind::Search, id, label, field)
    }

    /// A select posting `filters[key]`; `options` are label/value pairs.
    pub fn select(id: &str, key: &str, label: &str, value: &str, options: Vec<Choice>) -> Self {
        let field = Field::in_form(FILTERS_FORM, key)
            .id(id)
            .kind(InputKind::Select)
            .wrapper_class("mb-0")
            .value(value)
            .options(options);
        Self::new(ControlKind::Select, id, label, field)
    }

    pub fn date(id: &str, key: &str, label: &str, value: &str) -> Self {
        let field = Field::in_form(FILTERS_FORM, key)
            .id(id)
            .kind(InputKind::Date)
            .wrapper_class("mb-0")
            .value(value);
        Self::new(ControlKind::Date, id, label, field)
    }

    /// A multi-select; its label is rendered by the control, screen-reader only.
    pub fn multi_select(control: MultiSelect) -> Self {
        let mut control = control;
        control.label_class = "sr-only".into();
        control.wrapper_class = "mb-0".into();
        let (id, label) = (control.id.clone(), control.label.clone());
        Self {
            kind: ControlKind::MultiSelect(Box::new(control)),
            id,
            label,
            hint: String::new(),
            field: Field::default(),
        }
    }

    /// Helper text below the control.
    pub fn hint(mut self, hint: &str) -> Self {
        match &mut self.kind {
            ControlKind::Search => self.hint = hint.into(),
            ControlKind::MultiSelect(control) => control.hint = hint.into(),
            ControlKind::Select | ControlKind::Date => self.field.hint = hint.into(),
        }
        self
    }

    fn new(kind: ControlKind, id: &str, label: &str, field: Field) -> Self {
        Self {
            kind,
            id: id.into(),
            label: label.into(),
            hint: String::new(),
            field,
        }
    }

    pub fn multi(&self) -> Option<&MultiSelect> {
        match &self.kind {
            ControlKind::MultiSelect(control) => Some(control),
            _ => None,
        }
    }

    pub fn is_search(&self) -> bool {
        matches!(self.kind, ControlKind::Search)
    }

    pub fn is_date(&self) -> bool {
        matches!(self.kind, ControlKind::Date)
    }
}

/// The filter toolbar framed above a list. The form raises `event` on change
/// and on submit (Enter filters in place), carrying `filters[...]` values.
/// Search comes first, with filters to its right and wrapping below on narrow screens.
#[derive(Template, Clone, Debug)]
#[template(
    source = r#"{% import "lists.html" as lists %}{% call lists::filter_toolbar(toolbar=self) %}{% endcall %}"#,
    ext = "html"
)]
pub struct FilterToolbar<'a> {
    pub id: &'a str,
    pub event: &'a str,
    /// Page context only (such as `mt-4` below tabs).
    pub class: &'a str,
    pub target: &'a str,
    pub controls: Vec<ToolbarControl>,
}

impl<'a> FilterToolbar<'a> {
    /// A toolbar needs at least one control; an empty one panics on purpose.
    pub fn new(id: &'a str, mut controls: Vec<ToolbarControl>) -> Self {
        assert!(!controls.is_empty(), "a filter toolbar needs a control");
        controls.sort_by_key(|control| !control.is_search());
        Self {
            id,
            event: "filters",
            class: "",
            target: "",
            controls,
        }
    }
}

/// A table column; `sort` is the key the heading raises.
#[derive(Clone, Debug, Default)]
pub struct Column<'a> {
    pub label: &'a str,
    pub sort: &'a str,
    pub sort_id: &'a str,
    pub align_right: bool,
}

impl<'a> Column<'a> {
    pub fn new(label: &'a str) -> Self {
        Self {
            label,
            ..Self::default()
        }
    }

    pub fn sort(mut self, key: &'a str) -> Self {
        self.sort = key;
        self
    }

    pub fn sort_id(mut self, id: &'a str) -> Self {
        self.sort_id = id;
        self
    }

    pub fn right(mut self) -> Self {
        self.align_right = true;
        self
    }
}

/// One row: a stable `id` keeps a row's identity across patches.
#[derive(Clone, Debug, Default)]
pub struct Row {
    pub id: String,
    pub cells: Vec<Html>,
    pub actions: Html,
}

/// The empty row's content, shown when the caller decides the table is empty.
#[derive(Clone, Debug)]
pub enum TableEmpty {
    Content(Html),
    State(EmptyState),
}

/// The sort heading's view of the table, shared with the flex table.
#[derive(Clone, Copy, Debug, Default)]
pub struct SortView<'a> {
    pub table_id: &'a str,
    pub sort_by: Option<&'a str>,
    pub sort_dir: Option<SortDir>,
    pub event: &'a str,
    pub target: &'a str,
    /// The `op` a flex table's own event carries for sorting; empty for a
    /// list's `sort` event.
    pub op: &'a str,
}

impl SortView<'_> {
    fn active(&self, sort: &str) -> bool {
        self.sort_by == Some(sort)
    }

    /// `aria-sort` of a heading; `None` for a column that does not sort.
    pub fn aria_sort(&self, sort: &str) -> Option<&'static str> {
        if sort.is_empty() {
            return None;
        }
        Some(match (self.active(sort), self.sort_dir) {
            (true, Some(SortDir::Asc)) => "ascending",
            (true, Some(SortDir::Desc)) => "descending",
            _ => "none",
        })
    }

    pub fn icon(&self, sort: &str) -> &'static str {
        match (self.active(sort), self.sort_dir) {
            (true, Some(SortDir::Asc)) => "sort-asc",
            (true, Some(SortDir::Desc)) => "sort-desc",
            _ => "sort",
        }
    }

    pub fn icon_class(&self, sort: &str) -> &'static str {
        if self.active(sort) {
            "size-3.5 text-action"
        } else {
            "size-3.5 text-ink-faint"
        }
    }

    pub fn button_id(&self, column: &Column) -> String {
        if column.sort_id.is_empty() {
            format!("{}-sort-{}", self.table_id, column.sort)
        } else {
            column.sort_id.into()
        }
    }
}

/// A compact table with a flat outer frame. A row is not clickable: the cell
/// that opens a record holds a real link. `data-table-region` is the scroll box.
#[derive(Template, Clone, Debug)]
#[template(
    source = r#"{% import "lists.html" as lists %}{% call lists::table(t=self) %}{% endcall %}"#,
    ext = "html"
)]
pub struct Table<'a> {
    pub id: &'a str,
    pub caption: &'a str,
    pub columns: Vec<Column<'a>>,
    pub rows: Vec<Row>,
    pub sort: SortView<'a>,
    /// When false the table sits in an existing panel without its own frame.
    pub framed: bool,
    /// Renders the trailing actions column, whose cells come from `Row::actions`.
    pub actions: bool,
    pub empty: Option<TableEmpty>,
}

impl<'a> Table<'a> {
    pub fn new(id: &'a str, columns: Vec<Column<'a>>) -> Self {
        Self {
            id,
            caption: "",
            columns,
            rows: Vec::new(),
            sort: SortView {
                table_id: id,
                event: "sort",
                ..SortView::default()
            },
            framed: true,
            actions: false,
            empty: None,
        }
    }

    pub fn sorted(mut self, sort_by: Option<&'a str>, sort_dir: Option<SortDir>) -> Self {
        self.sort.sort_by = sort_by;
        self.sort.sort_dir = sort_dir;
        self
    }

    pub fn empty_colspan(&self) -> usize {
        self.columns.len() + usize::from(self.actions)
    }
}

#[derive(Clone, Debug)]
enum EmptyMode {
    Missing { title: String, reason: String },
    Forbidden(String),
}

/// What is missing and why, or that the viewer may not look; the permission
/// wording is owned here so every out-of-reach region says the same thing.
#[derive(Template, Clone, Debug)]
#[template(
    source = r#"{% import "lists.html" as lists %}{% call lists::empty_state(e=self) %}{% endcall %}"#,
    ext = "html"
)]
pub struct EmptyState {
    pub id: String,
    pub class: String,
    mode: EmptyMode,
    /// An optional recovery, such as clearing the search.
    pub action: Html,
}

impl EmptyState {
    /// Nothing exists yet, or a search matched nothing: the caller's two
    /// sentences. `reason` may be empty.
    pub fn missing(title: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::with(EmptyMode::Missing {
            title: title.into(),
            reason: reason.into(),
        })
    }

    /// The viewer lacks permission to `action` (such as "view companies").
    pub fn forbidden(action: impl Into<String>) -> Self {
        Self::with(EmptyMode::Forbidden(action.into()))
    }

    fn with(mode: EmptyMode) -> Self {
        Self {
            id: String::new(),
            class: String::new(),
            mode,
            action: Html::EMPTY,
        }
    }

    pub fn id(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self
    }

    pub fn action(mut self, action: Html) -> Self {
        self.action = action;
        self
    }

    pub fn heading(&self) -> String {
        match &self.mode {
            EmptyMode::Missing { title, .. } => title.clone(),
            EmptyMode::Forbidden(action) => format!("You do not have permission to {action}."),
        }
    }

    pub fn details(&self) -> Vec<&str> {
        match &self.mode {
            EmptyMode::Missing { reason, .. } if reason.is_empty() => vec![],
            EmptyMode::Missing { reason, .. } => vec![reason],
            EmptyMode::Forbidden(_) => vec!["Ask your administrator if you need access."],
        }
    }
}

/// A record opens directly alone and raises `workspace:select` when embedded.
/// The owning workspace validates the destination and decides follow/navigation.
#[derive(Template, Debug)]
#[template(
    source = r#"{% import "lists.html" as list %}{% call list::record_link(workspace=workspace, kind=kind, record_id=record_id, href=href, class=class) %}{{ body }}{% endcall %}"#,
    ext = "html"
)]
pub struct RecordLink<'a> {
    pub workspace: bool,
    pub kind: &'a str,
    pub record_id: &'a str,
    pub href: &'a str,
    pub class: &'a str,
    pub body: Html,
}
