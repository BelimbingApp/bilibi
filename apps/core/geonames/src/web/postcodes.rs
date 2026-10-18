//! `/geonames/postcodes` (Bilimbi `PostcodesLive`): the per-country totals,
//! the searchable Postcodes index with an inline place-name correction per
//! row, and the operator's New/Edit Postcode dialog. Corrections go through
//! `Geonames::{create_postcode, update_postcode}`, which keep the sidecar,
//! the compatible row and the audit trail together and refuse a stale
//! revision; the index row carries that revision in the fact's `data-id`
//! (`{id}|{revision}`, Bilimbi's `id_value`), so a redrawn row commits with
//! the current one.
use super::*;
use crate::{
    FieldError, PostcodeCountrySummary, PostcodeDraft, PostcodeIndex, PostcodeWriteError,
    Provenance,
};
use bilibi_ui::{
    list_state::FILTERS_FORM,
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest},
    params, Card, Choice, ColumnType, ColumnsOutcome, CommitStates, CommitStatus, CreateButton,
    Field, FilterToolbar, FlexRow, FlexTable, Header, IconButton, IconContext, InlineEdit,
    InlineField, InputKind, ListColumns, Modal, ModalWidth, PageCounts, Pagination, RowMode,
    TableEmpty, ToolbarControl,
};
use std::sync::LazyLock;

const DENIED: &str = "You do not have permission to update postcodes.";
const NOT_CHANGED: &str = "Postcode was not changed. Review the values and try again.";
const GONE: &str = "Postcode is no longer available.";
const FORM: &str = "postcode";

static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    spec(
        &[
            ("country_name", SortDir::Asc),
            ("postcode", SortDir::Asc),
            ("place_name", SortDir::Asc),
            ("admin1_code", SortDir::Asc),
            ("updated_at", SortDir::Desc),
        ],
        "country_name",
        &[],
    )
});
/// The totals table sorts on its own `summarySortBy`/`summarySortDir`; its
/// other names are prefixed too so they never collide with the index's.
static SUMMARY: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        sortable: BTreeMap::from([
            ("country_name".into(), SortDir::Asc),
            ("country_iso".into(), SortDir::Asc),
            ("record_count".into(), SortDir::Desc),
        ]),
        default_sort: Some("country_name".into()),
        page_sizes: PAGE_SIZES.to_vec(),
        default_page_size: 25,
        page_size_param: "PerPage".into(),
        param_prefix: "summary".into(),
        param_names: ParamNames {
            search: "Search".into(),
            page: "Page".into(),
            sort_by: "SortBy".into(),
            sort_dir: "SortDir".into(),
        },
        ..ListSpec::default()
    }
    .shared()
    .expect("geonames summary spec")
});
fn builtins() -> Vec<BuiltinColumn> {
    vec![
        sortable("country_name", "Country", "postcodes-sort-country"),
        sortable("postcode", "Postcode", "postcodes-sort-postcode"),
        sortable("place_name", "Place Name", "postcodes-sort-place"),
        sortable("admin1_code", "Admin1 Code", "postcodes-sort-admin1"),
        sortable("updated_at", "Updated", "postcodes-sort-updated"),
    ]
}
fn summary_builtins() -> Vec<BuiltinColumn> {
    let mut count = sortable("record_count", "Records", "postcodes-summary-sort-count").right();
    count.kind = ColumnType::Integer;
    vec![
        sortable("country_name", "Country", "postcodes-summary-sort-country"),
        sortable("country_iso", "ISO", "postcodes-summary-sort-iso"),
        count,
    ]
}
fn parse_summary(query: &HashMap<String, String>) -> Result<ListState> {
    ListState::parse(query, SUMMARY.clone()).map_err(|_| Error::Internal)
}
/// Only the totals' sort travels in the URL (Bilimbi `postcodes_path/2`).
fn postcodes_query(list: &ListState, summary: &ListState) -> HashMap<String, String> {
    let sort = summary.to_params();
    query(
        list,
        ["summarySortBy", "summarySortDir"]
            .into_iter()
            .filter_map(|key| Some((key.to_owned(), sort.get(key)?.clone())))
            .collect(),
    )
}

#[derive(Template)]
#[template(path = "geonames_postcodes.html")]
struct View<'a> {
    flash: FlashMap<'a>,
    header: Html,
    summary: Html,
    toolbar: Html,
    card: Html,
    modal: Html,
}
#[derive(Template)]
#[template(path = "geonames_postcode_form.html")]
struct FormView {
    country: Field,
    postcode: Field,
    place_name: Field,
    admin1: Field,
    latitude: Field,
    longitude: Field,
    accuracy: Field,
}

struct Shown {
    page: crate::Page<PostcodeIndex>,
    summaries: Vec<PostcodeCountrySummary>,
    /// Every country as the form offers it.
    countries: Vec<Choice>,
    reader: Reader,
}
enum ModalAction {
    New,
    Edit { id: i64, revision: String },
}
/// The open dialog's draft: what the form last posted and the refusals the
/// last check or save answered with.
struct Draft {
    action: ModalAction,
    values: HashMap<String, String>,
    errors: Vec<FieldError>,
    admin1_options: Vec<Choice>,
}

pub(super) struct Postcodes {
    pages: Pages,
    list: ListState,
    summary: ListState,
    columns: ListColumns,
    summary_columns: ListColumns,
    flash: Flash,
    statuses: CommitStates,
    shown: Option<Shown>,
    draft: Option<Draft>,
}

impl Postcodes {
    pub fn new(pages: Pages, query: &HashMap<String, String>) -> Result<Self> {
        Ok(Self {
            pages,
            list: parse(query, &SPEC)?,
            summary: parse_summary(query)?,
            columns: ListColumns::mount("postcodes-table", builtins())
                .map_err(|_| Error::Internal)?,
            summary_columns: ListColumns::mount(
                "postcodes-country-summary-rows",
                summary_builtins(),
            )
            .map_err(|_| Error::Internal)?,
            flash: Flash::default(),
            statuses: CommitStates::default(),
            shown: None,
            draft: None,
        })
    }

    async fn load(&mut self, scope: &Scope) -> Result<()> {
        let reader = reader(&self.pages, scope).await?;
        let geonames = &self.pages.geonames;
        let mut page = geonames.page_postcodes(&index_query(&self.list)).await?;
        if self.list.clamp_to_last_page(page.total_pages, true) {
            page = geonames.page_postcodes(&index_query(&self.list)).await?;
        }
        let summaries = geonames
            .list_postcode_country_summaries(&crate::IndexQuery {
                sort_by: self.summary.sort_by.clone(),
                sort_dir: self.summary.sort_dir.map(|dir| dir.key().to_owned()),
                ..crate::IndexQuery::default()
            })
            .await?;
        let countries = geonames
            .country_options()
            .await?
            .into_iter()
            .map(|(label, iso)| Choice::new(label, iso))
            .collect();
        self.columns
            .load(&page.entries, |postcode| postcode.id.to_string());
        self.summary_columns
            .load(&summaries, |summary| summary.country_iso.clone());
        self.shown = Some(Shown {
            page,
            summaries,
            countries,
            reader,
        });
        Ok(())
    }

    async fn admin1_options(&self, country_iso: &str) -> Result<Vec<Choice>> {
        Ok(self
            .pages
            .geonames
            .admin1_options(country_iso, true)
            .await?
            .into_iter()
            .map(|(label, code)| Choice::new(label, code))
            .collect())
    }

    async fn open_new(&mut self) -> Result<()> {
        let country = self
            .shown
            .as_ref()
            .and_then(|shown| shown.countries.first())
            .map(|choice| choice.value.clone())
            .unwrap_or_default();
        let admin1_options = self.admin1_options(&country).await?;
        self.flash.clear();
        self.draft = Some(Draft {
            action: ModalAction::New,
            values: HashMap::from([
                ("country_iso".into(), country),
                ("accuracy".into(), "4".into()),
            ]),
            errors: Vec::new(),
            admin1_options,
        });
        Ok(())
    }

    async fn open_edit(&mut self, id: Option<&str>) -> Result<()> {
        let found = params::positive_id(id).and_then(|id| {
            self.shown
                .as_ref()?
                .page
                .entries
                .iter()
                .find(|postcode| postcode.id == id)
                .cloned()
        });
        let Some(postcode) = found else {
            self.flash.put(StatusKind::Error, GONE);
            return Ok(());
        };
        let admin1_options = self.admin1_options(&postcode.country_iso).await?;
        // The form selects the full `CC.code` identity; the index row keeps
        // GeoNames' raw value.
        let admin1 = postcode.admin1_code.as_deref().map(|code| {
            if code.contains('.') {
                code.to_owned()
            } else {
                format!("{}.{code}", postcode.country_iso)
            }
        });
        let number = |value: Option<f64>| value.map(|n| n.to_string()).unwrap_or_default();
        self.flash.clear();
        self.draft = Some(Draft {
            action: ModalAction::Edit {
                id: postcode.id,
                revision: postcode.revision.clone(),
            },
            values: HashMap::from([
                ("country_iso".into(), postcode.country_iso.clone()),
                ("postcode".into(), postcode.postcode.clone()),
                ("place_name".into(), postcode.place_name.clone()),
                ("admin1_code".into(), admin1.unwrap_or_default()),
                ("latitude".into(), number(postcode.latitude)),
                ("longitude".into(), number(postcode.longitude)),
                (
                    "accuracy".into(),
                    postcode.accuracy.map(|n| n.to_string()).unwrap_or_default(),
                ),
            ]),
            errors: Vec::new(),
            admin1_options,
        });
        Ok(())
    }

    /// The form changed: keep what it posted, offer the chosen country's
    /// divisions and show what a save would refuse (Bilimbi `validate-postcode`).
    async fn validate(&mut self, posted: HashMap<String, String>) -> Result<()> {
        let Some(draft) = &self.draft else {
            return Ok(());
        };
        let country = posted
            .get("country_iso")
            .cloned()
            .or_else(|| draft.values.get("country_iso").cloned())
            .unwrap_or_default();
        let admin1_options = self.admin1_options(&country).await?;
        let errors = self
            .pages
            .geonames
            .check_postcode(&PostcodeDraft::from_params(&posted))
            .await?;
        if let Some(draft) = &mut self.draft {
            draft.values.extend(posted);
            draft.errors = errors;
            draft.admin1_options = admin1_options;
        }
        Ok(())
    }

    fn write_forbidden(&mut self) {
        self.statuses.clear_saved();
        if let Some(shown) = &mut self.shown {
            shown.reader.can_update = false;
        }
        self.draft = None;
        self.flash.put(StatusKind::Error, DENIED);
    }

    /// Returns whether the list must be read again.
    async fn save(&mut self, scope: &Scope, posted: HashMap<String, String>) -> Result<bool> {
        let Some(draft) = &mut self.draft else {
            self.flash.put(StatusKind::Error, GONE);
            return Ok(true);
        };
        draft.values.extend(posted);
        let submitted = PostcodeDraft::from_params(&draft.values);
        let (result, created) = match &draft.action {
            ModalAction::New => (
                self.pages.geonames.create_postcode(scope, &submitted).await,
                true,
            ),
            ModalAction::Edit { id, revision } => (
                self.pages
                    .geonames
                    .update_postcode(scope, *id, revision, &submitted)
                    .await,
                false,
            ),
        };
        match result {
            Ok(postcode) => {
                self.draft = None;
                self.flash.put(
                    StatusKind::Success,
                    format!(
                        "Postcode {} {}.",
                        postcode.postcode,
                        if created { "created" } else { "updated" }
                    ),
                );
                Ok(true)
            }
            Err(PostcodeWriteError::Invalid(errors)) => {
                if let Some(draft) = &mut self.draft {
                    draft.errors = errors;
                }
                Ok(false)
            }
            Err(PostcodeWriteError::Stale) => {
                self.draft = None;
                self.flash.put(
                    StatusKind::Error,
                    "Postcode changed while you were editing. Review the current values and try again.",
                );
                Ok(true)
            }
            Err(PostcodeWriteError::NotFound) => {
                self.draft = None;
                self.flash.put(StatusKind::Error, GONE);
                Ok(true)
            }
            Err(PostcodeWriteError::Access(Error::Unauthorized)) => Err(Error::Unauthorized),
            Err(PostcodeWriteError::Access(Error::Forbidden)) => {
                self.write_forbidden();
                Ok(false)
            }
            Err(PostcodeWriteError::Access(_)) => {
                self.flash.put(StatusKind::Error, NOT_CHANGED);
                Ok(false)
            }
        }
    }

    /// An inline place-name correction; `reference` is `{id}|{revision}`.
    async fn save_place(&mut self, scope: &Scope, reference: &str, value: &str) -> Result<bool> {
        let Some((id, revision)) = reference
            .split_once('|')
            .and_then(|(id, revision)| Some((params::positive_id(Some(id))?, revision)))
        else {
            self.flash.put(StatusKind::Error, NOT_CHANGED);
            return Ok(false);
        };
        let field = format!("postcode-{id}-place-name");
        let draft = PostcodeDraft {
            place_name: Some(value.to_owned()),
            ..PostcodeDraft::default()
        };
        match self
            .pages
            .geonames
            .update_postcode(scope, id, revision, &draft)
            .await
        {
            Ok(postcode) => {
                if let Some(shown) = &mut self.shown {
                    for entry in &mut shown.page.entries {
                        if entry.id == postcode.id {
                            *entry = postcode.clone();
                        }
                    }
                }
                self.statuses.put(field, CommitStatus::Saved);
                self.flash.put(
                    StatusKind::Success,
                    format!("Postcode {} updated.", postcode.postcode),
                );
                Ok(false)
            }
            Err(PostcodeWriteError::Invalid(errors)) => {
                let reasons: Vec<&str> =
                    errors.iter().map(|error| error.message.as_str()).collect();
                self.statuses
                    .put(field, CommitStatus::refusal("Place name", value, &reasons));
                self.flash.put(StatusKind::Error, NOT_CHANGED);
                Ok(false)
            }
            Err(PostcodeWriteError::Stale) => {
                self.flash.put(
                    StatusKind::Error,
                    "Postcode changed elsewhere. Review the current value and try again.",
                );
                Ok(true)
            }
            Err(PostcodeWriteError::Access(Error::Unauthorized)) => Err(Error::Unauthorized),
            Err(PostcodeWriteError::Access(Error::Forbidden)) => {
                self.write_forbidden();
                Ok(false)
            }
            Err(_) => {
                self.statuses.put(field, CommitStatus::failure());
                self.flash.put(StatusKind::Error, NOT_CHANGED);
                Ok(false)
            }
        }
    }

    /// Typed controls debounce so the dialog validates as the person types,
    /// as Bilimbi's `phx-change` does, not only when a control loses focus.
    fn form(&self, draft: &Draft, countries: &[Choice]) -> Result<Html> {
        let field = |key: &str| {
            Field::in_form(FORM, key)
                .value(draft.values.get(key).cloned().unwrap_or_default())
                .errors(
                    draft
                        .errors
                        .iter()
                        .filter(|error| error.field == key)
                        .map(|error| error.message.clone())
                        .collect(),
                )
                .attr("data-debounce", "300")
        };
        let mut postcode = field("postcode")
            .id("postcode-code")
            .label("Postcode")
            .required();
        postcode.attrs.maxlength = Some("20".into());
        let mut place_name = field("place_name")
            .id("postcode-place-name")
            .label("Place Name")
            .required();
        place_name.attrs.maxlength = Some("180".into());
        let mut latitude = field("latitude")
            .id("postcode-latitude")
            .kind(InputKind::Number)
            .label("Latitude");
        latitude.attrs.step = Some("0.0000001".into());
        let mut longitude = field("longitude")
            .id("postcode-longitude")
            .kind(InputKind::Number)
            .label("Longitude");
        longitude.attrs.step = Some("0.0000001".into());
        render(FormView {
            country: field("country_iso")
                .id("postcode-country")
                .kind(InputKind::Select)
                .label("Country")
                .prompt("Select country...")
                .options(countries.to_vec())
                .required(),
            postcode,
            place_name,
            admin1: field("admin1_code")
                .id("postcode-admin1")
                .kind(InputKind::Select)
                .label("Admin1 Division")
                .prompt("None")
                .options(draft.admin1_options.clone()),
            latitude,
            longitude,
            accuracy: field("accuracy")
                .id("postcode-accuracy")
                .kind(InputKind::Select)
                .label("Accuracy")
                .prompt("Unknown")
                .options(
                    (1..=6)
                        .map(|n| Choice::new(n.to_string(), n.to_string()))
                        .collect(),
                ),
        })
    }

    pub async fn view(&mut self, scope: &Scope, requery: bool) -> Result<String> {
        if requery || self.shown.is_none() {
            self.load(scope).await?;
        }
        let shown = self.shown.as_ref().ok_or(Error::Internal)?;
        let can_update = shown.reader.can_update;
        let header = render(Header {
            title: Html::text("Geonames Postcodes"),
            title_actions: render(IconButton {
                context: IconContext::Inline,
                id: "postcodes-pin",
                nav_pin: "nav-admin-geonames-postcode",
                ..IconButton::new("bilimbi-pin", "Pin Postcodes to sidebar")
            })?,
            actions: if can_update {
                render(CreateButton {
                    noun: "Postcode",
                    id: "postcodes-new",
                    event: "new-postcode",
                    button_type: "button",
                    ..CreateButton::default()
                })?
            } else {
                Html::EMPTY
            },
            ..Header::default()
        })?;

        let summary = if shown.summaries.is_empty() {
            Html::EMPTY
        } else {
            let rows = shown
                .summaries
                .iter()
                .map(|summary| {
                    let mut row = FlexRow::new(summary.country_iso.clone());
                    row.dom_id = Some(format!("postcode-country-{}", summary.country_iso));
                    row.slot(
                        "country_name",
                        Html::raw(format!(
                            "<span class=\"whitespace-nowrap text-ink\">{}</span>",
                            Html::text(&summary.country_name)
                        )),
                    )
                    .slot(
                        "country_iso",
                        Html::raw(format!(
                            "<span class=\"whitespace-nowrap font-mono text-xs text-ink-muted\">{}</span>",
                            Html::text(&summary.country_iso)
                        )),
                    )
                    .slot(
                        "record_count",
                        Html::raw(format!(
                            "<span class=\"whitespace-nowrap tabular-nums text-ink\">{}</span>",
                            format_integer(summary.record_count)
                        )),
                    )
                })
                .collect();
            let mut table = FlexTable::new(
                "postcodes-country-summary-rows",
                self.summary_columns.flex_columns(),
                rows,
            )
            .sorted(self.summary.sort_by.as_deref(), self.summary.sort_dir);
            table.event = "summary_grid";
            table.zoom = self.summary_columns.zoom;
            table.suggestions = self.summary_columns.flex_suggestions();
            table.add_query = &self.summary_columns.add_query;
            table.framed = false;
            table.caption = "Postcodes by country";
            let mut body = Html::raw(
                "<div class=\"border-b border-line px-4 py-3\"><h2 class=\"text-sm font-semibold text-ink\">Postcodes by country</h2></div>",
            );
            body.push(&render(table)?);
            render(Card {
                id: "postcodes-country-summary",
                inner_class: "p-0",
                body,
                ..Card::default()
            })?
        };

        let form = self.list.filters_form();
        let toolbar = render(FilterToolbar::new(
            "postcodes-filters",
            vec![ToolbarControl::search(
                "postcodes-search",
                "search",
                "Search postcodes",
                &form["search"],
                "Search by postcode, place name, or country...",
            )],
        ))?;

        let normal = self.columns.mode() == RowMode::Normal;
        let mut rows = Vec::with_capacity(shown.page.entries.len());
        for postcode in &shown.page.entries {
            let id = postcode.id;
            let mut row = FlexRow::new(id.to_string());
            row.dom_id = Some(format!("postcode-{id}"));
            let local = if normal && postcode.provenance == Provenance::Operator {
                "<span class=\"rounded-md border border-line bg-brand-surface px-1 py-0.5 text-[0.6875rem] text-ink-muted\">Local</span>"
            } else {
                ""
            };
            let place = if can_update {
                let field_id = format!("postcode-{id}-place-name");
                let mut field = InlineField::new(
                    &field_id,
                    "place_name",
                    format!("Edit place name for {}", postcode.postcode),
                    &postcode.place_name,
                )
                .status(self.statuses.get(&field_id));
                field.id_value = format!("{id}|{}", postcode.revision);
                field.save_event = "save-postcode-place".into();
                render(InlineEdit { field })?
            } else {
                muted(Some(&postcode.place_name), false)
            };
            if can_update {
                let button_id = format!("postcode-{id}-edit");
                let label = format!("Edit {}", postcode.postcode);
                let value = id.to_string();
                row.actions = render(IconButton {
                    id: &button_id,
                    event: "edit-postcode",
                    name: "id",
                    value: &value,
                    ..IconButton::new("edit", &label)
                })?;
            }
            row = row
                .slot(
                    "country_name",
                    country_cell(&postcode.country_iso, postcode.country_name.as_deref()),
                )
                .slot(
                    "postcode",
                    Html::raw(format!(
                        "<div class=\"flex items-center gap-1.5 whitespace-nowrap\"><span class=\"font-medium tabular-nums text-ink\">{}</span>{local}</div>",
                        Html::text(&postcode.postcode)
                    )),
                )
                .slot("place_name", place)
                .slot("admin1_code", muted(postcode.admin1_code.as_deref(), true))
                .slot(
                    "updated_at",
                    date_cell(
                        &shown.reader.display,
                        &format!("postcode-{id}-updated"),
                        postcode.updated_at,
                    )?,
                );
            rows.push(row);
        }
        let mut table = FlexTable::new("postcodes-table", self.columns.flex_columns(), rows)
            .sorted(self.list.sort_by.as_deref(), self.list.sort_dir);
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.framed = false;
        table.actions = can_update;
        table.caption = "Geonames postcodes";
        if shown.page.entries.is_empty() {
            table.empty = Some(TableEmpty::Content(Html::text("No postcodes found.")));
        }
        let mut body = render(table)?;
        body.push(&render(Pagination::new(
            "postcodes-pagination",
            PageCounts::new(
                self.list.page,
                self.list.page_size,
                shown.page.total_entries,
            ),
            &PAGE_SIZES,
        ))?);
        let card = render(Card {
            id: "postcodes-card",
            inner_class: "p-0",
            body,
            ..Card::default()
        })?;

        let modal = match &self.draft {
            Some(draft) => render(Modal {
                id: "postcode-modal",
                title: match draft.action {
                    ModalAction::New => "New Postcode",
                    ModalAction::Edit { .. } => "Edit Postcode",
                },
                cancel_event: "close-postcode-modal",
                width: ModalWidth::Wide,
                description: "Your changes are kept when postcodes are next updated from GeoNames.",
                role: "",
                flash: Some(self.flash.map()),
                body: self.form(draft, &shown.countries)?,
            })?,
            None => Html::EMPTY,
        };
        View {
            flash: self.flash.map(),
            header,
            summary,
            toolbar,
            card,
            modal,
        }
        .render()
        .map_err(|_| Error::Internal)
    }
}

impl LiveState<Scope, Error> for Postcodes {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            let mut requery = true;
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) => {
                    self.list = parse(values, &SPEC)?;
                    self.summary = parse_summary(values)?;
                }
                LiveInput::Event(event) => {
                    let get = |key: &str| event.get(key).and_then(Value::as_str);
                    match get("type") {
                        Some("filters") => {
                            apply_filters(&mut self.list, &params::form_values(event, FILTERS_FORM))
                        }
                        Some("sort") => {
                            if !self.list.next_sort(get("sort").unwrap_or_default()) {
                                return Err(Error::Invalid);
                            }
                        }
                        Some("sort-summary") => {
                            if !self.summary.next_sort(get("sort").unwrap_or_default()) {
                                return Err(Error::Invalid);
                            }
                        }
                        Some("grid") => match self.columns.handle(&grid_values(event)) {
                            ColumnsOutcome::Sort(key) => {
                                if !self.list.next_sort(&key) {
                                    return Err(Error::Invalid);
                                }
                            }
                            ColumnsOutcome::Update | ColumnsOutcome::Noop => requery = false,
                        },
                        Some("summary_grid") => {
                            match self.summary_columns.handle(&grid_values(event)) {
                                ColumnsOutcome::Sort(key) => {
                                    if !self.summary.next_sort(&key) {
                                        return Err(Error::Invalid);
                                    }
                                }
                                ColumnsOutcome::Update | ColumnsOutcome::Noop => requery = false,
                            }
                        }
                        Some("page") => self.list.put_page(get("page")),
                        Some("new-postcode") => {
                            self.open_new().await?;
                            requery = false;
                        }
                        Some("edit-postcode") => {
                            self.open_edit(get("id")).await?;
                            requery = false;
                        }
                        Some("close-postcode-modal") => {
                            self.draft = None;
                            requery = false;
                        }
                        Some("validate-postcode") => {
                            self.validate(params::form_values(event, FORM)).await?;
                            requery = false;
                        }
                        Some("save-postcode") => {
                            requery = self
                                .save(request.scope, params::form_values(event, FORM))
                                .await?;
                        }
                        Some("save-postcode-place") => {
                            let value = get("value").ok_or(Error::Invalid)?;
                            if get("field") != Some("place_name") || value.len() > 4096 {
                                return Err(Error::Invalid);
                            }
                            requery = self
                                .save_place(request.scope, get("id").unwrap_or_default(), value)
                                .await?;
                        }
                        Some("clear-flash") => {
                            self.flash.dismiss(get("key"));
                            requery = false;
                        }
                        _ => return Err(Error::Invalid),
                    }
                }
            }
            let html = self.view(request.scope, requery).await?;
            Ok(LiveBody {
                html,
                query: postcodes_query(&self.list, &self.summary),
                ..LiveBody::default()
            })
        })
    }
}
