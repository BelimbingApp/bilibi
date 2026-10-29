//! `/work-calendars`: the tenant's calendars a page at a time, searched by
//! code or name, with the zone in force and the included holiday sets. A
//! person who may manage them creates one in a dialog (its first week, zone
//! and holiday substitution) and lands on it.
use super::*;
use crate::{contributions::CALENDARS_PATH, CalendarEntry, Listing, MANAGE, VIEW};
use bilibi_ui::{
    list_state::FILTERS_FORM, params, Badge, BadgeKind, BuiltinColumn, Card, ColumnsOutcome,
    CreateButton, Field, FilterToolbar, FlexRow, FlexTable, Header, InputKind, ListColumns,
    ListSpec, ListState, Modal, ModalWidth, PageCounts, Pagination, RecordLink, TableEmpty,
    ToolbarControl,
};
use serde_json::json;
use std::sync::LazyLock;

static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        page_sizes: PAGE_SIZES.to_vec(),
        default_page_size: 25,
        page_size_param: "per_page".into(),
        omit_blank: vec!["search".into()],
        omit_defaults: true,
        ..ListSpec::default()
    }
    .shared()
    .expect("work calendar list spec")
});
fn columns() -> Vec<BuiltinColumn> {
    vec![
        BuiltinColumn::new("code", "Code"),
        BuiltinColumn::new("name", "Name"),
        BuiltinColumn::new("status", "Status"),
        BuiltinColumn::new("timezone", "Zone"),
        BuiltinColumn::new("holiday_sets", "Holiday sets"),
    ]
}
/// The substitution choices, as the record offers them too.
pub(super) fn substitutes() -> Vec<bilibi_ui::Choice> {
    bilibi_ui::choices(&[
        ("Stay where they fall", "none"),
        ("Move off rest days", "rest"),
        ("Move off rest and off days", "rest_and_off"),
    ])
}
pub(super) fn status_badge(status: &str) -> Result<Html> {
    render(if status == "active" {
        Badge::status(BadgeKind::Success, "Active")
    } else {
        Badge::status(BadgeKind::Neutral, "Retired")
    })
}

pub(super) struct Calendars {
    pages: Pages,
    list: ListState,
    columns: ListColumns,
    framed: bool,
    rows_changed: bool,
    entries: Vec<CalendarEntry>,
    total: u64,
    draft: Option<Draft>,
    flash: Flash,
    redirect: Option<String>,
}
impl Calendars {
    pub fn new(pages: Pages, request: &PageRequest<'_, Scope>) -> Result<Self> {
        Ok(Self {
            pages,
            list: ListState::parse(request.query, SPEC.clone()).map_err(|_| Error::Internal)?,
            columns: ListColumns::mount("work-calendars", columns())
                .map_err(|_| Error::Internal)?,
            framed: request.framed,
            rows_changed: true,
            entries: Vec::new(),
            total: 0,
            draft: None,
            flash: Flash::default(),
            redirect: None,
        })
    }
    fn listing(&self) -> Listing {
        Listing {
            search: self.list.search.trim().chars().take(200).collect(),
            page: self.list.page as i64,
            page_size: self.list.page_size as i64,
        }
    }
    async fn load(&mut self, scope: &Scope) -> Result<()> {
        let mut page = self.pages.calendars.calendars(scope, &self.listing()).await;
        // A page past the last (a calendar went, or a stale link) shows the last.
        if let Ok(rows) = &page {
            let last = (rows.total as u64).div_ceil(self.list.page_size);
            if self.list.clamp_to_last_page(last, true) {
                page = self.pages.calendars.calendars(scope, &self.listing()).await;
            }
        }
        let page = page?;
        self.total = page.total as u64;
        self.entries = page.rows;
        if self.rows_changed {
            self.columns.load(&self.entries, |c| c.code.clone());
        }
        Ok(())
    }

    async fn event(&mut self, scope: &Scope, credential: &str, value: &Value) -> Result<()> {
        match event(value)? {
            "filters" => self.list.apply_filters(&form(value, FILTERS_FORM)?),
            "page" => self
                .list
                .put_page(value.get("page").and_then(Value::as_str)),
            "grid" => match self.columns.handle(
                &params::string_values_checked(value, params::FIELD_LIMIT)
                    .map_err(Error::refused)?,
            ) {
                ColumnsOutcome::Sort(_) => return Err(Error::Invalid),
                ColumnsOutcome::Update | ColumnsOutcome::Noop => self.rows_changed = false,
            },
            "clear-flash" => self.flash.dismiss(text(value, "key")),
            "new" => {
                if !self.pages.allowed(scope, MANAGE).await? {
                    self.flash.put(StatusKind::Error, DENIED);
                    return Ok(());
                }
                let mut values = week::defaults();
                values.insert("timezone".into(), self.pages.zone(scope).await?);
                values.insert("substitute".into(), "none".into());
                values.insert(
                    "effective_from".into(),
                    self.pages.today(scope).await?.to_string(),
                );
                self.flash.clear();
                self.draft = Some(Draft {
                    values,
                    errors: BTreeMap::new(),
                });
            }
            "close_new" => self.draft = None,
            "create_calendar" => self.create(scope, credential, value).await?,
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }
    async fn create(&mut self, scope: &Scope, credential: &str, value: &Value) -> Result<()> {
        let posted = form(value, "calendar")?;
        let field = |key: &str| posted.get(key).map_or("", String::as_str).trim().to_owned();
        let code = field("code");
        let mut input = json!({
            "code": code,
            "name": field("name"),
            "timezone": field("timezone"),
            "substitute": field("substitute"),
            "week": week::input(&posted),
        });
        if !field("effective_from").is_empty() {
            input["effective_from"] = json!(field("effective_from"));
        }
        match self
            .pages
            .submit(
                scope,
                credential,
                "create_calendar",
                "core.work_calendar.create",
                input,
            )
            .await?
        {
            Ok(_) => {
                self.draft = None;
                self.redirect = Some(format!("{CALENDARS_PATH}/{code}"));
            }
            Err(refusal @ Refusal::Invalid(_)) => {
                let (errors, rest) = refusal.fields(|key| {
                    ["code", "name", "timezone", "effective_from", "substitute"].contains(&key)
                        || week::is_field(key)
                });
                self.flash_rest(rest);
                self.draft = Some(Draft {
                    values: posted,
                    errors,
                });
            }
            Err(refusal) => {
                self.flash
                    .put(StatusKind::Error, refusal.sentence("calendar"));
                self.draft = Some(Draft {
                    values: posted,
                    errors: BTreeMap::new(),
                });
            }
        }
        Ok(())
    }

    /// Refusals the dialog has no field for, said on the page.
    fn flash_rest(&mut self, rest: String) {
        if rest.is_empty() {
            self.flash.clear();
        } else {
            self.flash.put(StatusKind::Error, rest);
        }
    }

    fn dialog(&self, draft: &Draft) -> Result<Html> {
        let value = |key: &str| draft.values.get(key).cloned().unwrap_or_default();
        let errors = |key: &str| draft.errors.get(key).cloned().unwrap_or_default();
        let field = |key: &str, label: &str| {
            Field::in_form("calendar", key)
                .id(format!("work-calendar-new-{}", key.replace('_', "-")))
                .label(label)
                .value(value(key))
                .errors(errors(key))
        };
        let rows = vec![
            vec![
                render(
                    field("code", "Code")
                        .hint("Lowercase letters, digits, _ and -; it cannot change later.")
                        .attr("maxlength", "64")
                        .required(),
                )?,
                render(field("name", "Name").attr("maxlength", "120").required())?,
            ],
            vec![
                render(
                    field("timezone", "Zone")
                        .hint("Opening hours are read in this time zone.")
                        .required(),
                )?,
                render(
                    field("effective_from", "Applies from")
                        .kind(InputKind::Date)
                        .hint("Earlier dates follow this week too.")
                        .required(),
                )?,
            ],
            vec![render(
                field("substitute", "Holidays on a day off")
                    .kind(InputKind::Select)
                    .options(substitutes())
                    .hint("Where a holiday moves when it falls on a rest or off day."),
            )?],
        ];
        let actions = dialog_actions("Create calendar", "Creating…", "close_new")?;
        let body = render(DialogForm {
            id: "work-calendar-new-form",
            event: "create_calendar",
            rows,
            week_heading: "work-calendar-new-week",
            week: week::editor("calendar", &draft.values, &draft.errors)?,
            actions,
            ..DialogForm::default()
        })?;
        render(Modal {
            id: "work-calendar-new-dialog",
            title: "New work calendar",
            description:
                "Its working days and hours, from a date. Add holidays and exceptions after.",
            cancel_event: "close_new",
            width: ModalWidth::Wide,
            flash: Some(self.flash.map()),
            body,
            ..Modal::default()
        })
    }

    async fn view(&self, scope: &Scope) -> Result<String> {
        let (can_view, can_manage) = {
            let client = self.pages.db.get().await?;
            let held = effective_capabilities(&**client, scope).await?;
            (held.contains(VIEW), held.contains(MANAGE))
        };
        let actions = if can_manage {
            render(CreateButton {
                noun: "Calendar",
                id: "work-calendar-new",
                event: "new",
                button_type: "button",
                ..CreateButton::default()
            })?
        } else {
            Html::EMPTY
        };
        let header = render(Header {
            title: Html::text("Work Calendars"),
            subtitle: "When the business is open: working days and hours, holidays and exceptions.",
            actions,
            ..Header::default()
        })?;
        let filters = self.list.filters_form();
        let toolbar = render(FilterToolbar::new(
            "work-calendar-filters",
            vec![ToolbarControl::search(
                "work-calendar-search",
                "search",
                "Search calendars",
                &filters["search"],
                "Search by code or name...",
            )],
        ))?;
        let mut rows = Vec::with_capacity(self.entries.len());
        for calendar in &self.entries {
            let href = format!("{CALENDARS_PATH}/{}", calendar.code);
            let code = Html::text(&calendar.code);
            let code = if can_view {
                render(RecordLink {
                    workspace: self.framed,
                    kind: "core/work_calendar",
                    record_id: &calendar.code,
                    href: &href,
                    class: "whitespace-nowrap font-mono text-xs font-medium text-action hover:underline",
                    body: code,
                })?
            } else {
                code
            };
            let mut sets = calendar.holiday_sets.rows.join(", ");
            let more = calendar.holiday_sets.total - calendar.holiday_sets.rows.len() as i64;
            if more > 0 {
                sets.push_str(&format!(" and {more} more"));
            }
            let mut row = FlexRow::new(calendar.code.clone())
                .slot("code", code)
                .slot("name", Html::text(&calendar.name))
                .slot("status", status_badge(&calendar.status)?)
                .slot("timezone", Html::text(&calendar.timezone))
                .slot(
                    "holiday_sets",
                    Html::text(if sets.is_empty() { "—" } else { &sets }),
                );
            row.dom_id = Some(format!("work-calendars-row-{}", calendar.code));
            rows.push(row);
        }
        let mut table = FlexTable::new("work-calendars", self.columns.flex_columns(), rows);
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.caption = "Work calendars";
        table.framed = false;
        if self.entries.is_empty() {
            table.empty = Some(TableEmpty::Content(Html::text(
                if !self.list.search.trim().is_empty() {
                    "No calendar matches this search."
                } else if can_manage {
                    "No work calendars yet. Create one to set working days and hours."
                } else {
                    "No work calendars yet."
                },
            )));
        }
        let mut list = render(table)?;
        list.push(&render(Pagination::new(
            "work-calendar-pagination",
            PageCounts::new(self.list.page, self.list.page_size, self.total),
            &PAGE_SIZES,
        ))?);
        let card = render(Card {
            id: "work-calendars-card",
            inner_class: "p-0",
            body: list,
            ..Card::default()
        })?;
        let dialog = match &self.draft {
            Some(draft) => self.dialog(draft)?,
            None => Html::EMPTY,
        };
        View {
            id: "work-calendars-page",
            variant: PageVariant::List,
            flash: self.flash.map(),
            record_kind: "",
            record_id: "",
            header,
            sections: vec![toolbar, card],
            dialog,
        }
        .render()
        .map_err(|_| Error::Internal)
    }
}
impl Titled for Calendars {}
impl LiveState<Scope, Error> for Calendars {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            self.rows_changed = true;
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) => {
                    self.list =
                        ListState::parse(values, SPEC.clone()).map_err(|_| Error::Internal)?
                }
                LiveInput::Event(value) => {
                    self.event(request.scope, request.credential, value).await?
                }
            }
            if let Some(path) = self.redirect.take() {
                return Ok(LiveBody {
                    redirect: Some(path),
                    flash: Some((StatusKind::Success, "Work calendar created.".into())),
                    ..LiveBody::default()
                });
            }
            self.load(request.scope).await?;
            let html = self.view(request.scope).await?;
            Ok(body(html, self.list.to_params(), self.entries.len(), ""))
        })
    }
}
