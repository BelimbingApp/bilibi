//! `/reports`: the reports the reader may run, and the New report dialog
//! (`?new=1` opens it, as Contacts' does). A new report starts small (a few
//! of the record type's own columns, or a row count) and opens on its
//! Definition tab, where the author shapes it in place.
use super::{clear_flash, flash_map, instant, refusal, render, report_path, Pages, Refusal};
use crate::{Kind, Report};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    choices, error_sentence,
    list_state::{PageCounts, FILTERS_FORM},
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    params, Alert, Badge, BadgeKind, BuiltinColumn, Button, ButtonVariant, Card, ColumnType,
    ColumnsOutcome, CreateButton, ErrorSummary, Field, FilterToolbar, FlashMap, FlexRow, FlexTable,
    Header, Html, InputKind, ListColumns, ListSpec, ListState, Modal, Pagination, RecordLink,
    StatusKind, TableEmpty, ToolbarControl,
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, LazyLock},
};

const PAGE_SIZES: [u64; 3] = [25, 50, 100];
const DENIED: &str = "You do not have permission to build reports.";
/// Own columns a new rows report starts with.
const STARTER_COLUMNS: usize = 5;

static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        page_sizes: PAGE_SIZES.to_vec(),
        default_page_size: 25,
        page_size_param: "per_page".into(),
        omit_defaults: true,
        ..ListSpec::default()
    }
    .shared()
    .expect("report list spec")
});

fn builtins() -> Vec<BuiltinColumn> {
    let mut updated = BuiltinColumn::new("updated", "Updated");
    updated.kind = ColumnType::Datetime;
    vec![
        BuiltinColumn::new("name", "Name"),
        BuiltinColumn::new("record_type", "Records"),
        BuiltinColumn::new("kind", "Kind"),
        BuiltinColumn::new("sharing", "Shared"),
        updated,
    ]
}

fn parse(query: &HashMap<String, String>) -> Result<ListState> {
    ListState::parse(query, SPEC.clone()).map_err(|_| Error::Internal)
}

/// The New report dialog's draft; errors stay until it closes.
#[derive(Default)]
struct NewDraft {
    name: String,
    record_type: String,
    kind: String,
    errors: Vec<(&'static str, String)>,
    notice: String,
}

pub(crate) struct Index {
    pages: Pages,
    state: ListState,
    columns: ListColumns,
    rows_changed: bool,
    rows: Vec<Report>,
    new: Option<NewDraft>,
    flash: Option<(StatusKind, String)>,
}

#[derive(Template)]
#[template(path = "reports_index.html")]
struct IndexView<'a> {
    flash: FlashMap<'a>,
    header: Html,
    toolbar: Html,
    card: Html,
    dialog: Html,
}

#[derive(Template)]
#[template(
    source = r#"{% import "flex_table.html" as flex %}{% call flex::instant(id=id, i=i) %}{% endcall %}"#,
    ext = "html"
)]
pub(crate) struct UpdatedCell<'a> {
    pub id: &'a str,
    pub i: bilibi_ui::Instant,
}

impl Index {
    pub fn new(pages: Pages, query: &HashMap<String, String>) -> Result<Self> {
        Ok(Self {
            pages,
            state: parse(query)?,
            columns: ListColumns::mount("reports", builtins()).map_err(|_| Error::Internal)?,
            rows_changed: true,
            rows: Vec::new(),
            new: (query.get("new").map(String::as_str) == Some("1")).then(NewDraft::default),
            flash: None,
        })
    }

    /// Answers `Some(path)` when the event created a report to open.
    async fn event(
        &mut self,
        scope: &Scope,
        credential: &str,
        value: &Value,
    ) -> Result<Option<String>> {
        let get = |key: &str| value.get(key).and_then(Value::as_str);
        match get("type").ok_or(Error::Invalid)? {
            "filters" => self
                .state
                .apply_filters(&params::form_values(value, FILTERS_FORM)),
            "page" => self.state.put_page(get("page")),
            "grid" => match self.columns.handle(&params::string_values(value)) {
                ColumnsOutcome::Sort(_) => return Err(Error::Invalid),
                ColumnsOutcome::Update | ColumnsOutcome::Noop => self.rows_changed = false,
            },
            "clear-flash" => clear_flash(&mut self.flash, value),
            "open_new" => self.new = Some(NewDraft::default()),
            "close_modal" => self.new = None,
            "create" => return self.create(scope, credential, value).await,
            _ => return Err(Error::Invalid),
        }
        Ok(None)
    }

    async fn create(
        &mut self,
        scope: &Scope,
        credential: &str,
        value: &Value,
    ) -> Result<Option<String>> {
        let Some(draft) = self.new.as_mut() else {
            return Ok(None);
        };
        let fields = params::form_values(value, "report");
        draft.name = fields.get("name").cloned().unwrap_or_default();
        draft.record_type = fields.get("record_type").cloned().unwrap_or_default();
        draft.kind = fields.get("kind").cloned().unwrap_or_default();
        draft.errors.clear();
        draft.notice.clear();
        if draft.name.trim().is_empty() {
            draft.errors.push(("name", "can't be blank".into()));
        }
        if draft.record_type.is_empty() {
            draft.errors.push(("record_type", "can't be blank".into()));
        }
        let Some(kind) = Kind::from_key(&draft.kind) else {
            draft.errors.push(("kind", "can't be blank".into()));
            return Ok(None);
        };
        if !draft.errors.is_empty() {
            return Ok(None);
        }
        let definition = match kind {
            Kind::Summary => json!({"chart": "kpi"}),
            Kind::Rows | Kind::Matrix => {
                let columns = match self.pages.reports.columns(scope, &draft.record_type).await {
                    Ok(columns) => columns,
                    Err(_) => {
                        draft
                            .errors
                            .push(("record_type", "is not a record type you can read".into()));
                        return Ok(None);
                    }
                };
                let own: Vec<_> = columns
                    .iter()
                    .filter(|c| !c.spec.contains('.'))
                    .take(STARTER_COLUMNS)
                    .map(|c| c.spec.clone())
                    .collect();
                if kind == Kind::Rows {
                    json!({"columns": own})
                } else {
                    // A matrix starts down and across the first two of the
                    // record's own text or choice fields.
                    let groups: Vec<_> = columns
                        .iter()
                        .filter(|c| {
                            !c.spec.contains('.') && matches!(c.ty, "string" | "choice" | "boolean")
                        })
                        .take(2)
                        .map(|c| json!({"column": c.spec}))
                        .collect();
                    if groups.len() < 2 {
                        draft.errors.push(("kind", "needs two text or choice fields to group by; this record type has fewer".into()));
                        return Ok(None);
                    }
                    json!({"groupings": groups})
                }
            }
        };
        let input = json!({"name": draft.name.trim(), "description": "", "record_type": draft.record_type, "kind": kind.key(), "definition": definition});
        match self
            .pages
            .submit(scope, credential, "create", "base.reports.create", input)
            .await
        {
            Ok(result) => {
                let id = result["id"].as_i64().ok_or(Error::Internal)?;
                self.new = None;
                Ok(Some(format!("{}?tab=definition", report_path(id))))
            }
            Err(error) => {
                match refusal(error, DENIED)? {
                    Refusal::Fields(fields) => {
                        for (path, messages) in fields {
                            let key = ["name", "record_type", "kind"]
                                .into_iter()
                                .find(|key| path == *key);
                            match key {
                                Some(key) => {
                                    draft.errors.extend(messages.into_iter().map(|m| (key, m)))
                                }
                                None => draft.notice = messages.join(" "),
                            }
                        }
                    }
                    Refusal::Message(message) => draft.notice = message,
                }
                Ok(None)
            }
        }
    }

    pub async fn render(&mut self, scope: &Scope) -> Result<String> {
        let pages = self.pages.clone();
        let can = pages.can(scope).await?;
        let display = pages.display(scope).await?;
        let types: HashMap<String, String> = pages
            .reports
            .record_types(scope)
            .await
            .map_err(super::page_error)?
            .into_iter()
            .collect();
        // Stored reports page in one query with their search; the built-ins
        // that match follow them, so both count toward the pages.
        let search = self.state.search.trim().to_owned();
        let needle = search.to_lowercase();
        let builtins: Vec<_> = pages
            .reports
            .builtins(scope)
            .await
            .map_err(super::page_error)?
            .into_iter()
            .filter(|r| {
                needle.is_empty()
                    || r.name.to_lowercase().contains(&needle)
                    || r.description.to_lowercase().contains(&needle)
            })
            .collect();
        let size = self.state.page_size.max(1);
        let read = |offset: u64| pages.reports.page(scope, &search, offset, size);
        let (mut entries, mut stored) =
            read(self.state.offset()).await.map_err(super::page_error)?;
        let mut total = stored + builtins.len() as u64;
        if self.state.clamp_to_last_page(total.div_ceil(size), true) {
            (entries, stored) = read(self.state.offset()).await.map_err(super::page_error)?;
            total = stored + builtins.len() as u64;
        }
        let skip = self.state.offset().saturating_sub(stored) as usize;
        let room = (size as usize).saturating_sub(entries.len());
        entries.extend(builtins.into_iter().skip(skip).take(room));
        let counts = PageCounts::new(self.state.page, self.state.page_size, total);
        self.rows = entries;
        if self.rows_changed {
            self.columns.load(&self.rows, row_key);
        }
        let actions = if can.author {
            render(CreateButton {
                noun: "Report",
                id: "reports-new",
                event: "open_new",
                button_type: "button",
                ..CreateButton::default()
            })?
        } else {
            Html::EMPTY
        };
        let header = render(Header {
            title: Html::text("Reports"),
            subtitle: "Saved questions about your records. Each person who runs one sees only what they may see.",
            actions,
            ..Header::default()
        })?;
        let form = self.state.filters_form();
        let toolbar = render(FilterToolbar::new(
            "reports-filters",
            vec![ToolbarControl::search(
                "reports-search",
                "search",
                "Search reports",
                &form["search"],
                "Search by name...",
            )],
        ))?;
        let mut rows = Vec::with_capacity(self.rows.len());
        for report in &self.rows {
            let href = match report.builtin {
                Some(key) => format!("{}/{key}", super::BUILTIN_BASE),
                None => report_path(report.id),
            };
            let id = row_key(report);
            let name = render(RecordLink {
                workspace: false,
                kind: "report",
                record_id: &id,
                href: &href,
                class: "font-medium text-link hover:underline",
                body: Html::text(&report.name),
            })?;
            let sharing = if report.builtin.is_some() {
                render(Badge::status(BadgeKind::Neutral, "Built-in"))?
            } else if report.owned {
                if report.shared_with_everyone {
                    render(Badge::status(BadgeKind::Plain, "Everyone"))?
                } else if !report.role_ids.is_empty() {
                    let label = format!(
                        "{} role{}",
                        report.role_ids.len(),
                        if report.role_ids.len() == 1 { "" } else { "s" }
                    );
                    render(Badge::status(BadgeKind::Plain, &label))?
                } else {
                    render(Badge::status(BadgeKind::Neutral, "Only you"))?
                }
            } else {
                render(Badge::status(BadgeKind::Neutral, "Shared with you"))?
            };
            let updated = match instant(&display, &report.updated_at) {
                Some(i) => {
                    let cell = format!("reports-updated-{}", report.id);
                    render(UpdatedCell { id: &cell, i })?
                }
                None => Html::text("—"),
            };
            let label = types
                .get(&report.record_type)
                .cloned()
                .unwrap_or_else(|| report.record_type.clone());
            let mut row = FlexRow::new(id.clone())
                .slot("name", name)
                .slot("record_type", Html::text(&label))
                .slot("kind", Html::text(kind_label(report.kind)))
                .slot("sharing", sharing)
                .slot("updated", updated);
            row.dom_id = Some(format!("report-{id}"));
            rows.push(row);
        }
        let mut table = FlexTable::new("reports", self.columns.flex_columns(), rows);
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.caption = "Reports";
        table.framed = false;
        if counts.total_entries == 0 {
            table.empty = Some(TableEmpty::Content(Html::text(if !search.is_empty() {
                "No reports match this search."
            } else if can.author {
                "No reports yet. Build one with New report."
            } else {
                "No reports are shared with you yet."
            })));
        }
        let mut body = render(table)?;
        body.push(&render(Pagination::new(
            "reports-pagination",
            counts,
            &PAGE_SIZES,
        ))?);
        let card = render(bilibi_ui::Card {
            id: "reports-card",
            inner_class: "p-0",
            body,
            ..Card::default()
        })?;
        let dialog = match &self.new {
            Some(draft) if can.author => new_dialog(draft, &types)?,
            _ => Html::EMPTY,
        };
        IndexView {
            flash: flash_map(&self.flash),
            header,
            toolbar,
            card,
            dialog,
        }
        .render()
        .map_err(|_| Error::Internal)
    }
}

/// A list row's key: a stored report's id, or `b-<key>` for a built-in.
fn row_key(report: &Report) -> String {
    match report.builtin {
        Some(key) => format!("b-{key}"),
        None => report.id.to_string(),
    }
}

pub(crate) fn kind_label(kind: Kind) -> &'static str {
    match kind {
        Kind::Rows => "List",
        Kind::Summary => "Summary",
        Kind::Matrix => "Matrix",
    }
}

fn label_for(key: &str) -> &'static str {
    match key {
        "record_type" => "Records",
        "kind" => "Kind",
        _ => "Name",
    }
}

fn new_dialog(draft: &NewDraft, types: &HashMap<String, String>) -> Result<Html> {
    let errors = |key: &str| -> Vec<String> {
        draft
            .errors
            .iter()
            .filter(|(k, _)| *k == key)
            .map(|(k, m)| error_sentence(label_for(k), m))
            .collect()
    };
    let mut options: Vec<(&String, &String)> =
        types.iter().map(|(id, label)| (label, id)).collect();
    options.sort();
    let mut body = Html::EMPTY;
    if !draft.errors.is_empty() {
        let mut summary = ErrorSummary::new("report-new-errors");
        for (key, message) in &draft.errors {
            summary.push(
                format!("report_{key}"),
                error_sentence(label_for(key), message),
            );
        }
        body.push(&render(summary)?);
    }
    if !draft.notice.is_empty() {
        body.push(&render(Alert {
            kind: StatusKind::Error,
            id: "report-new-notice",
            body: Html::text(&draft.notice),
            ..Alert::default()
        })?);
    }
    let fields = [
        Field::in_form("report", "name")
            .label("Name")
            .value(&draft.name)
            .required()
            .errors(errors("name"))
            .placeholder("Open orders by month"),
        Field::in_form("report", "record_type")
            .label("Records")
            .kind(InputKind::Select)
            .prompt("Choose what to report on")
            .options(
                options
                    .iter()
                    .map(|(label, id)| bilibi_ui::Choice::new(label.as_str(), id.as_str()))
                    .collect(),
            )
            .value(&draft.record_type)
            .required()
            .errors(errors("record_type")),
        Field::in_form("report", "kind")
            .label("Kind")
            .kind(InputKind::Select)
            .options(choices(&[
                ("List: chosen columns, row by row", "rows"),
                ("Summary: totals by group, with a chart", "summary"),
                ("Matrix: one group down, one across", "matrix"),
            ]))
            .value(if draft.kind.is_empty() {
                "rows"
            } else {
                &draft.kind
            })
            .required()
            .errors(errors("kind")),
    ];
    for field in fields {
        body.push(&render(field)?);
    }
    let actions = {
        let mut actions = render(Button {
            id: "report-new-cancel",
            button_type: "button",
            event: "close_modal",
            body: Html::text("Cancel"),
            ..Button::default()
        })?;
        actions.push(&render(Button {
            id: "report-new-create",
            button_type: "submit",
            variant: ButtonVariant::Primary,
            working: "Creating…",
            body: Html::text("Create report"),
            ..Button::default()
        })?);
        actions
    };
    let form = render(bilibi_ui::Form {
        id: "report-new-form",
        event: "create",
        change: false,
        class: "",
        body,
        actions,
        submit_event: "",
    })?;
    render(Modal {
        id: "report-new",
        title: "New report",
        cancel_event: "close_modal",
        description: "Choose what to report on. You pick its columns, filters and groups next.",
        body: form,
        ..Modal::default()
    })
}

impl LiveState<Scope, crate::web::Error> for Index {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            self.rows_changed = true;
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) => {
                    self.state = parse(values)?;
                    if values.get("new").map(String::as_str) == Some("1") && self.new.is_none() {
                        self.new = Some(NewDraft::default());
                    }
                }
                LiveInput::Event(value) => {
                    if let Some(path) = self.event(request.scope, request.credential, value).await?
                    {
                        return Ok(LiveBody {
                            redirect: Some(path),
                            flash: Some((
                                StatusKind::Success,
                                "Report created. Choose what it shows, then save.".into(),
                            )),
                            ..LiveBody::default()
                        });
                    }
                }
            }
            let html = self.render(request.scope).await?;
            Ok(LiveBody {
                html,
                query: self.state.to_params(),
                rows: self.rows.len(),
                ..LiveBody::default()
            })
        })
    }
}
