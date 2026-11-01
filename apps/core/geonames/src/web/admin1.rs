//! `/geonames/admin1`: the Admin1 index with its
//! country filter (form field `countryIso`, URL key `filterCountryIso`) and
//! an inline local-name correction per row and the operator's Update
//! (Belimbing `Geonames\Livewire\Admin1\Index::update`),
//! which refreshes the Admin1 dataset from GeoNames in the background.
use super::{update::Update, *};
use crate::{Admin1Index, Dataset, NameWriteError};
use bilibi_ui::{
    list_state::FILTERS_FORM,
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, Subscription},
    params, Card, Choice, ColumnsOutcome, CommitStates, CommitStatus, FilterToolbar, FlexRow,
    FlexTable, Header, InlineEdit, InlineField, ListColumns, PageCounts, Pagination, TableEmpty,
    ToolbarControl,
};
use std::sync::LazyLock;

const DENIED: &str = "You do not have permission to update first-level divisions.";
const NOT_SAVED: &str = "Failed to save division name.";
const FILTER: &str = "countryIso";
const FILTER_PARAM: &str = "filterCountryIso";

static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    spec(
        &[
            ("country_name", SortDir::Asc),
            ("code", SortDir::Asc),
            ("name", SortDir::Asc),
            ("alt_name", SortDir::Asc),
            ("updated_at", SortDir::Desc),
        ],
        "country_name",
        &[(FILTER, Filter::string(""))],
    )
});
fn builtins() -> Vec<BuiltinColumn> {
    vec![
        sortable("country_name", "Country", "admin1-sort-country"),
        sortable("code", "Code", "admin1-sort-code"),
        sortable("name", "Name", "admin1-sort-name"),
        sortable("alt_name", "Alt Name", "admin1-sort-alt-name"),
        sortable("updated_at", "Updated", "admin1-sort-updated"),
    ]
}
/// The form field stays `countryIso`; the URL key stays `filterCountryIso`.
fn parse_index(query: &HashMap<String, String>) -> Result<ListState> {
    let mut values = query.clone();
    let iso = values.get(FILTER_PARAM).cloned().unwrap_or_default();
    values.insert(FILTER.into(), iso);
    parse(&values, &SPEC)
}
fn admin1_query(state: &ListState) -> HashMap<String, String> {
    let mut params = query(state, BTreeMap::new());
    let iso = params.remove(FILTER).unwrap_or_default();
    params.insert(FILTER_PARAM.into(), iso);
    params
}

#[derive(Template)]
#[template(path = "geonames_admin1.html")]
struct View<'a> {
    flash: FlashMap<'a>,
    header: Html,
    toolbar: Html,
    card: Html,
}

struct Shown {
    page: crate::Page<Admin1Index>,
    /// Only the countries that have divisions, as the filter offers them.
    filter_countries: Vec<Choice>,
    reader: Reader,
}

pub(super) struct Admin1 {
    pages: Pages,
    list: ListState,
    columns: ListColumns,
    flash: Flash,
    statuses: CommitStates,
    shown: Option<Shown>,
    update: Update,
}

impl Admin1 {
    pub fn new(pages: Pages, query: &HashMap<String, String>) -> Result<Self> {
        Ok(Self {
            pages,
            list: parse_index(query)?,
            columns: ListColumns::mount("admin1-table", builtins()).map_err(|_| Error::Internal)?,
            flash: Flash::default(),
            statuses: CommitStates::default(),
            shown: None,
            update: Update::new(update::Spec {
                dataset: Dataset::Admin1,
                noun: "First-level divisions",
                button_id: "admin1-update",
                event: "update-admin1",
                denied: DENIED,
            }),
        })
    }

    fn index_query(&self) -> crate::IndexQuery {
        crate::IndexQuery {
            country_iso: Some(self.list.filters[FILTER].clone()),
            ..index_query(&self.list)
        }
    }

    async fn load(&mut self, scope: &Scope) -> Result<()> {
        let reader = reader(&self.pages, scope).await?;
        let mut page = self.pages.geonames.page_admin1(&self.index_query()).await?;
        if self.list.clamp_to_last_page(page.total_pages, true) {
            page = self.pages.geonames.page_admin1(&self.index_query()).await?;
        }
        let mut filter_countries = vec![Choice::new("All Countries", "")];
        filter_countries.extend(
            self.pages
                .geonames
                .admin1_filter_countries()
                .await?
                .into_iter()
                .map(|country| {
                    Choice::new(
                        format!("{} ({})", country.country, country.iso),
                        country.iso,
                    )
                }),
        );
        self.columns
            .load(&page.entries, |admin1| admin1.id.to_string());
        self.shown = Some(Shown {
            page,
            filter_countries,
            reader,
        });
        Ok(())
    }

    async fn save_name(&mut self, scope: &Scope, id: Option<&str>, value: &str) -> Result<()> {
        let Some(id) = params::positive_id(id) else {
            self.flash.put(StatusKind::Error, NOT_SAVED);
            return Ok(());
        };
        let field = format!("admin1-{id}-name");
        match self
            .pages
            .geonames
            .update_admin1_name(scope, id, value)
            .await
        {
            Ok(updated) => {
                if let Some(shown) = &mut self.shown {
                    for entry in &mut shown.page.entries {
                        if entry.id == updated.id {
                            *entry = updated.clone();
                        }
                    }
                }
                self.statuses.put(field, CommitStatus::Saved);
                self.flash.put(
                    StatusKind::Success,
                    format!("First-level division {} updated.", updated.code),
                );
            }
            Err(NameWriteError::Access(Error::Unauthorized)) => return Err(Error::Unauthorized),
            Err(NameWriteError::Access(Error::Forbidden)) => {
                self.statuses.clear_saved();
                if let Some(shown) = &mut self.shown {
                    shown.reader.can_update = false;
                }
                self.flash.put(StatusKind::Error, DENIED);
            }
            Err(error @ (NameWriteError::Blank | NameWriteError::TooLong(_))) => {
                self.statuses.put(
                    field,
                    CommitStatus::refusal(
                        "first-level division name",
                        value,
                        &[&error.to_string()],
                    ),
                );
                self.flash.put(StatusKind::Error, NOT_SAVED);
            }
            Err(_) => {
                self.statuses.put(field, CommitStatus::failure());
                self.flash.put(StatusKind::Error, NOT_SAVED);
            }
        }
        Ok(())
    }

    pub async fn view(&mut self, scope: &Scope, requery: bool) -> Result<String> {
        if requery || self.shown.is_none() {
            self.load(scope).await?;
        }
        let shown = self.shown.as_ref().ok_or(Error::Internal)?;
        let can_update = shown.reader.can_update;
        let header = render(Header {
            title_actions: Html::EMPTY,
            title: Html::text("First-level divisions"),
            subtitle: "Edit the names of states and provinces.",
            actions: if can_update {
                self.update.button()?
            } else {
                Html::EMPTY
            },
        })?;
        let form = self.list.filters_form();
        let toolbar = render(FilterToolbar::new(
            "admin1-filters",
            vec![
                ToolbarControl::search(
                    "admin1-search",
                    "search",
                    "Search first-level divisions",
                    &form["search"],
                    "Search by name, code, or country...",
                ),
                ToolbarControl::select(
                    "admin1-country-filter",
                    FILTER,
                    "Country",
                    &form[FILTER],
                    shown.filter_countries.clone(),
                ),
            ],
        ))?;

        let mut rows = Vec::with_capacity(shown.page.entries.len());
        for admin1 in &shown.page.entries {
            let id = admin1.id;
            let mut row = FlexRow::new(id.to_string());
            row.dom_id = Some(format!("admin1-{id}"));
            let name = if can_update {
                let field_id = format!("admin1-{id}-name");
                let mut field =
                    InlineField::new(&field_id, "name", "first-level division name", &admin1.name)
                        .status(self.statuses.get(&field_id));
                field.id_value = id.to_string();
                field.save_event = "save-admin1-name".into();
                render(InlineEdit { field })?
            } else {
                Html::raw(format!(
                    "<span class=\"text-ink\">{}</span>",
                    Html::text(&admin1.name)
                ))
            };
            row = row
                .slot(
                    "country_name",
                    country_cell(&admin1.country_iso, admin1.country_name.as_deref()),
                )
                .slot(
                    "code",
                    Html::raw(format!(
                        "<span class=\"whitespace-nowrap font-mono text-ink\">{}</span>",
                        Html::text(&admin1.code)
                    )),
                )
                .slot("name", name)
                .slot("alt_name", muted(admin1.alt_name.as_deref(), false))
                .slot(
                    "updated_at",
                    date_cell(
                        &shown.reader.display,
                        &format!("admin1-{id}-updated"),
                        admin1.updated_at,
                    )?,
                );
            rows.push(row);
        }
        let mut table = FlexTable::new("admin1-table", self.columns.flex_columns(), rows)
            .sorted(self.list.sort_by.as_deref(), self.list.sort_dir);
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.framed = false;
        table.caption = "First-level divisions";
        if shown.page.entries.is_empty() {
            table.empty = Some(TableEmpty::Content(Html::text(
                "No first-level divisions found.",
            )));
        }
        let mut body = render(table)?;
        body.push(&render(Pagination::new(
            "admin1-pagination",
            PageCounts::new(
                self.list.page,
                self.list.page_size,
                shown.page.total_entries,
            ),
            &PAGE_SIZES,
        ))?);
        let card = render(Card {
            id: "admin1-card",
            inner_class: "p-0",
            body,
            ..Card::default()
        })?;
        View {
            flash: self.flash.map(),
            header,
            toolbar,
            card,
        }
        .render()
        .map_err(|_| Error::Internal)
    }
}

impl LiveState<Scope, Error> for Admin1 {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            // A finished Update has already changed the rows: this pass reads them.
            let settled = self.update.settle(&mut self.flash);
            let mut requery = true;
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) => self.list = parse_index(values)?,
                LiveInput::Event(event) => {
                    let get = |key: &str| event.get(key).and_then(Value::as_str);
                    match get("type") {
                        Some("filters") => apply_filters(
                            &mut self.list,
                            &params::form_values_checked(event, FILTERS_FORM, params::FIELD_LIMIT)
                                .map_err(bilibi_database::Error::refused)?,
                        ),
                        Some("sort") => {
                            if !self.list.next_sort(get("sort").unwrap_or_default()) {
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
                        Some("page") => self.list.put_page(get("page")),
                        Some("update-admin1") => {
                            self.update
                                .start(&self.pages, request.scope, &mut self.flash)
                                .await?;
                            requery = false;
                        }
                        Some("save-admin1-name") => {
                            let value = get("value").ok_or(Error::Invalid)?;
                            if get("field") != Some("name") || value.len() > 4096 {
                                return Err(Error::Invalid);
                            }
                            self.save_name(request.scope, get("id"), value).await?;
                            requery = false;
                        }
                        Some("clear-flash") => {
                            self.flash.dismiss(get("key"));
                            requery = false;
                        }
                        _ => return Err(Error::Invalid),
                    }
                }
            }
            let html = self.view(request.scope, requery || settled).await?;
            Ok(LiveBody {
                html,
                query: admin1_query(&self.list),
                ..LiveBody::default()
            })
        })
    }
    fn subscriptions(&mut self) -> Vec<Subscription> {
        vec![self.update.subscription()]
    }
}
