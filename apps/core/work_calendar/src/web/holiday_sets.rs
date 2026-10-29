//! `/holiday-sets`: the tenant's holiday sets a page at a time, searched by
//! code or name, with their country and division. A person who may manage
//! calendars creates one in a dialog, whose divisions follow the chosen
//! country, and lands on it to enter its holidays.
use super::*;
use crate::{contributions::SETS_PATH, HolidaySetEntry, Listing, MANAGE, VIEW};
use bilibi_ui::{
    list_state::FILTERS_FORM, params, BuiltinColumn, Card, Choice, ColumnsOutcome, Combobox,
    CreateButton, Field, FilterToolbar, FlexRow, FlexTable, Header, ListColumns, ListSpec,
    ListState, Modal, PageCounts, Pagination, RecordLink, TableEmpty, ToolbarControl,
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
    .expect("holiday set list spec")
});
fn columns() -> Vec<BuiltinColumn> {
    vec![
        BuiltinColumn::new("code", "Code"),
        BuiltinColumn::new("name", "Name"),
        BuiltinColumn::new("country", "Country"),
        BuiltinColumn::new("division", "Division"),
    ]
}
/// The imported countries (`"Name (ISO)"`, ISO). An empty choice is a set
/// kept for no one place.
pub(super) async fn countries(geonames: &Geonames) -> Result<Vec<Choice>> {
    Ok(geonames
        .country_options()
        .await?
        .into_iter()
        .map(|(label, iso)| Choice::new(label, iso))
        .collect())
}
/// The divisions of `country`, name first and code after; none without one.
pub(super) async fn divisions(geonames: &Geonames, country: &str) -> Result<Vec<Choice>> {
    if country.is_empty() {
        return Ok(Vec::new());
    }
    Ok(geonames
        .admin1_options(country, false)
        .await?
        .into_iter()
        .map(|(name, code)| Choice::new(name, code.clone()).detail(code))
        .collect())
}
/// A shown choice's label: the name for a value, a dash for none.
pub(super) fn shown(choices: &[Choice], value: &str) -> String {
    if value.is_empty() {
        return "—".into();
    }
    choices
        .iter()
        .find(|choice| choice.value == value)
        .map_or(value.to_owned(), |choice| choice.label.clone())
}

pub(super) struct Sets {
    pages: Pages,
    list: ListState,
    columns: ListColumns,
    rows_changed: bool,
    entries: Vec<HolidaySetEntry>,
    total: u64,
    draft: Option<Draft>,
    /// The dialog's place choices, read once a dialog opens and again only
    /// when its country changes.
    countries: Vec<Choice>,
    divisions: (String, Vec<Choice>),
    flash: Flash,
    redirect: Option<String>,
}
impl Sets {
    pub fn new(pages: Pages, request: &PageRequest<'_, Scope>) -> Result<Self> {
        Ok(Self {
            pages,
            list: ListState::parse(request.query, SPEC.clone()).map_err(|_| Error::Internal)?,
            columns: ListColumns::mount("holiday-sets", columns()).map_err(|_| Error::Internal)?,
            rows_changed: true,
            entries: Vec::new(),
            total: 0,
            draft: None,
            countries: Vec::new(),
            divisions: (String::new(), Vec::new()),
            flash: Flash::default(),
            redirect: None,
        })
    }
    async fn place_choices(&mut self, country: &str) -> Result<()> {
        if self.countries.is_empty() {
            self.countries = countries(&self.pages.geonames).await?;
        }
        if self.divisions.0 != country {
            self.divisions = (
                country.to_owned(),
                divisions(&self.pages.geonames, country).await?,
            );
        }
        Ok(())
    }
    fn listing(&self) -> Listing {
        Listing {
            search: self.list.search.trim().chars().take(200).collect(),
            page: self.list.page as i64,
            page_size: self.list.page_size as i64,
        }
    }
    async fn load(&mut self, scope: &Scope) -> Result<()> {
        let mut page = self
            .pages
            .calendars
            .holiday_sets(scope, &self.listing())
            .await;
        if let Ok(rows) = &page {
            let last = (rows.total as u64).div_ceil(self.list.page_size);
            if self.list.clamp_to_last_page(last, true) {
                page = self
                    .pages
                    .calendars
                    .holiday_sets(scope, &self.listing())
                    .await;
            }
        }
        let page = page?;
        self.total = page.total as u64;
        self.entries = page.rows;
        if self.rows_changed {
            self.columns.load(&self.entries, |s| s.code.clone());
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
                self.flash.clear();
                self.place_choices("").await?;
                self.draft = Some(Draft::default());
            }
            "close_new" => self.draft = None,
            // The open dialog follows its values: a new country offers its
            // divisions. A change arriving after Cancel opens nothing.
            "set_draft" => {
                let Some(draft) = &self.draft else {
                    return Ok(());
                };
                let mut values = form(value, "set")?;
                if draft.values.get("country_iso") != values.get("country_iso") {
                    values.remove("admin1_code");
                }
                let country = values.get("country_iso").cloned().unwrap_or_default();
                self.place_choices(&country).await?;
                self.draft = Some(Draft {
                    values,
                    errors: BTreeMap::new(),
                });
            }
            "create_holiday_set" => self.create(scope, credential, value).await?,
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }
    async fn create(&mut self, scope: &Scope, credential: &str, value: &Value) -> Result<()> {
        let posted = form(value, "set")?;
        let field = |key: &str| posted.get(key).map_or("", String::as_str).trim().to_owned();
        let code = field("code");
        let mut input = json!({"code": code, "name": field("name")});
        if !field("country_iso").is_empty() {
            input["country_iso"] = json!(field("country_iso"));
            if !field("admin1_code").is_empty() {
                input["admin1_code"] = json!(field("admin1_code"));
            }
        }
        match self
            .pages
            .submit(
                scope,
                credential,
                "create_holiday_set",
                "core.work_calendar.holiday_set.create",
                input,
            )
            .await?
        {
            Ok(_) => {
                self.draft = None;
                self.redirect = Some(format!("{SETS_PATH}/{code}"));
            }
            Err(refusal @ Refusal::Invalid(_)) => {
                let (errors, rest) = refusal
                    .fields(|key| ["code", "name", "country_iso", "admin1_code"].contains(&key));
                if rest.is_empty() {
                    self.flash.clear();
                } else {
                    self.flash.put(StatusKind::Error, rest);
                }
                self.draft = Some(Draft {
                    values: posted,
                    errors,
                });
            }
            Err(refusal) => {
                self.flash
                    .put(StatusKind::Error, refusal.sentence("holiday set"));
                self.draft = Some(Draft {
                    values: posted,
                    errors: BTreeMap::new(),
                });
            }
        }
        Ok(())
    }

    fn dialog(&self, draft: &Draft) -> Result<Html> {
        let value = |key: &str| draft.values.get(key).cloned().unwrap_or_default();
        let errors = |key: &str| draft.errors.get(key).cloned().unwrap_or_default();
        let field = |key: &str, label: &str| {
            Field::in_form("set", key)
                .id(format!("holiday-set-new-{}", key.replace('_', "-")))
                .label(label)
                .value(value(key))
                .errors(errors(key))
        };
        let combo = |key: &str, label: &str, options: Vec<Choice>, placeholder: &str| {
            Combobox::new(
                format!("holiday-set-new-{}", key.replace('_', "-")),
                format!("set[{key}]"),
            )
            .label(label)
            .options(options)
            .value(value(key))
            .placeholder(placeholder)
            .errors(errors(key))
        };
        let country = value("country_iso");
        let mut place = vec![render(
            combo(
                "country_iso",
                "Country",
                self.countries.clone(),
                "Choose a country",
            )
            .hint("Optional. Countries come from the imported place names."),
        )?];
        if !country.is_empty() {
            place.push(render(
                combo(
                    "admin1_code",
                    "Division",
                    self.divisions.1.clone(),
                    "Choose a division",
                )
                .hint("Optional: a state's own holidays."),
            )?);
        }
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
            place,
        ];
        let body = render(DialogForm {
            id: "holiday-set-new-form",
            event: "set_draft",
            change: true,
            submit: "create_holiday_set",
            rows,
            actions: dialog_actions("Create set", "Creating…", "close_new")?,
            ..DialogForm::default()
        })?;
        render(Modal {
            id: "holiday-set-new-dialog",
            title: "New holiday set",
            description: "Whole-day holidays for a country or a state. Enter them after.",
            cancel_event: "close_new",
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
                noun: "Holiday Set",
                id: "holiday-set-new",
                event: "new",
                button_type: "button",
                ..CreateButton::default()
            })?
        } else {
            Html::EMPTY
        };
        let header = render(Header {
            title: Html::text("Holiday Sets"),
            subtitle: "Public holidays by country or state, which calendars include.",
            actions,
            ..Header::default()
        })?;
        let filters = self.list.filters_form();
        let toolbar = render(FilterToolbar::new(
            "holiday-set-filters",
            vec![ToolbarControl::search(
                "holiday-set-search",
                "search",
                "Search holiday sets",
                &filters["search"],
                "Search by code or name...",
            )],
        ))?;
        let mut rows = Vec::with_capacity(self.entries.len());
        for set in &self.entries {
            let code = Html::text(&set.code);
            // A set is no workspace record kind: those are module ids, and
            // `core/work_calendar` names the calendars.
            let code = if can_view {
                render(RecordLink {
                    workspace: false,
                    kind: "",
                    record_id: &set.code,
                    href: &format!("{SETS_PATH}/{}", set.code),
                    class: "whitespace-nowrap font-mono text-xs font-medium text-action hover:underline",
                    body: code,
                })?
            } else {
                code
            };
            let mut row = FlexRow::new(set.code.clone())
                .slot("code", code)
                .slot("name", Html::text(&set.name))
                .slot(
                    "country",
                    Html::text(set.country_iso.as_deref().unwrap_or("—")),
                )
                .slot(
                    "division",
                    Html::text(set.admin1_code.as_deref().unwrap_or("—")),
                );
            row.dom_id = Some(format!("holiday-sets-row-{}", set.code));
            rows.push(row);
        }
        let mut table = FlexTable::new("holiday-sets", self.columns.flex_columns(), rows);
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.caption = "Holiday sets";
        table.framed = false;
        if self.entries.is_empty() {
            table.empty = Some(TableEmpty::Content(Html::text(
                if !self.list.search.trim().is_empty() {
                    "No holiday set matches this search."
                } else if can_manage {
                    "No holiday sets yet. Create one for each country or state you observe."
                } else {
                    "No holiday sets yet."
                },
            )));
        }
        let mut list = render(table)?;
        list.push(&render(Pagination::new(
            "holiday-set-pagination",
            PageCounts::new(self.list.page, self.list.page_size, self.total),
            &PAGE_SIZES,
        ))?);
        let card = render(Card {
            id: "holiday-sets-card",
            inner_class: "p-0",
            body: list,
            ..Card::default()
        })?;
        let dialog = match &self.draft {
            Some(draft) => self.dialog(draft)?,
            None => Html::EMPTY,
        };
        View {
            id: "holiday-sets-page",
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
impl Titled for Sets {}
impl LiveState<Scope, Error> for Sets {
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
                    flash: Some((StatusKind::Success, "Holiday set created.".into())),
                    ..LiveBody::default()
                });
            }
            self.load(request.scope).await?;
            let html = self.view(request.scope).await?;
            Ok(body(html, self.list.to_params(), self.entries.len(), ""))
        })
    }
}
