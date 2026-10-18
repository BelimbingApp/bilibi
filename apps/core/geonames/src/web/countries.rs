//! `/geonames/countries` (Bilimbi `CountriesLive`): the searchable, sortable
//! Countries index with its built-in columns, an inline local-name
//! correction per row and the operator's Update, which refreshes the
//! countries dataset from GeoNames in the background and reports what it did.
use super::{update::Update, *};
use crate::{CountryIndex, Dataset, NameWriteError};
use bilibi_ui::{
    list_state::FILTERS_FORM,
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, Subscription},
    params, Card, ColumnsOutcome, CommitStates, CommitStatus, FilterToolbar, FlexRow, FlexTable,
    Header, IconButton, IconContext, InlineEdit, InlineField, ListColumns, PageCounts, Pagination,
    TableEmpty, ToolbarControl,
};
use std::sync::LazyLock;

const DENIED: &str = "You do not have permission to update countries.";
const NOT_SAVED: &str = "Failed to save country name.";

static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    spec(
        &[
            ("iso", SortDir::Asc),
            ("country", SortDir::Asc),
            ("capital", SortDir::Asc),
            ("phone", SortDir::Asc),
            ("currency_code", SortDir::Asc),
            ("population", SortDir::Desc),
            ("updated_at", SortDir::Desc),
        ],
        "country",
        &[],
    )
});
fn builtins() -> Vec<BuiltinColumn> {
    vec![
        sortable("iso", "ISO", "countries-sort-iso"),
        sortable("country", "Country", "countries-sort-country"),
        sortable("capital", "Capital", "countries-sort-capital"),
        sortable("phone", "Phone", "countries-sort-phone"),
        sortable("currency_code", "Currency", "countries-sort-currency"),
        sortable("population", "Population", "countries-sort-population").right(),
        sortable("updated_at", "Updated", "countries-sort-updated"),
    ]
}

#[derive(Template)]
#[template(path = "geonames_countries.html")]
struct View<'a> {
    flash: FlashMap<'a>,
    header: Html,
    toolbar: Html,
    card: Html,
}

struct Shown {
    page: crate::Page<CountryIndex>,
    reader: Reader,
}

pub(super) struct Countries {
    pages: Pages,
    list: ListState,
    columns: ListColumns,
    flash: Flash,
    statuses: CommitStates,
    shown: Option<Shown>,
    update: Update,
}

impl Countries {
    pub fn new(pages: Pages, query: &HashMap<String, String>) -> Result<Self> {
        Ok(Self {
            pages,
            list: parse(query, &SPEC)?,
            columns: ListColumns::mount("countries-table", builtins())
                .map_err(|_| Error::Internal)?,
            flash: Flash::default(),
            statuses: CommitStates::default(),
            shown: None,
            update: Update::new(update::Spec {
                dataset: Dataset::Countries,
                noun: "Countries",
                button_id: "countries-update",
                event: "update-countries",
                denied: DENIED,
            }),
        })
    }

    async fn load(&mut self, scope: &Scope) -> Result<()> {
        let reader = reader(&self.pages, scope).await?;
        let mut page = self
            .pages
            .geonames
            .page_countries(&index_query(&self.list))
            .await?;
        if self.list.clamp_to_last_page(page.total_pages, true) {
            page = self
                .pages
                .geonames
                .page_countries(&index_query(&self.list))
                .await?;
        }
        // The rows changed: ListColumns forgets an unfinished add box.
        self.columns
            .load(&page.entries, |country| country.id.to_string());
        self.shown = Some(Shown { page, reader });
        Ok(())
    }

    async fn save_name(&mut self, scope: &Scope, id: Option<&str>, value: &str) -> Result<()> {
        let Some(id) = params::positive_id(id) else {
            self.flash.put(StatusKind::Error, NOT_SAVED);
            return Ok(());
        };
        let field = format!("country-{id}-name");
        match self
            .pages
            .geonames
            .update_country_name(scope, id, value)
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
                    format!("Country {} name updated.", updated.iso),
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
                    CommitStatus::refusal("Country name", value, &[&error.to_string()]),
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

        let actions = if can_update {
            self.update.button()?
        } else {
            Html::EMPTY
        };
        let header = render(Header {
            title: Html::text("Countries"),
            title_actions: render(IconButton {
                context: IconContext::Inline,
                id: "countries-pin",
                nav_pin: "nav-admin-geonames-country",
                ..IconButton::new("bilimbi-pin", "Pin Countries to sidebar")
            })?,
            actions,
            ..Header::default()
        })?;
        let form = self.list.filters_form();
        let toolbar = render(FilterToolbar::new(
            "countries-filters",
            vec![ToolbarControl::search(
                "countries-search",
                "search",
                "Search countries",
                &form["search"],
                "Search by country name or ISO code...",
            )],
        ))?;

        let mut rows = Vec::with_capacity(shown.page.entries.len());
        for country in &shown.page.entries {
            let id = country.id;
            let mut row = FlexRow::new(id.to_string());
            row.dom_id = Some(format!("country-{id}"));
            let name = if can_update {
                let field_id = format!("country-{id}-name");
                let mut field =
                    InlineField::new(&field_id, "country", "Country name", &country.country)
                        .status(self.statuses.get(&field_id));
                field.id_value = id.to_string();
                field.save_event = "save-country-name".into();
                render(InlineEdit { field })?
            } else {
                Html::raw(format!(
                    "<span class=\"font-medium text-ink\">{}</span>",
                    Html::text(&country.country)
                ))
            };
            row = row
                .slot(
                    "iso",
                    Html::raw(format!(
                        "<span class=\"whitespace-nowrap font-medium tabular-nums text-ink\">{}</span>",
                        Html::text(&country.iso)
                    )),
                )
                .slot("country", name)
                .slot("capital", muted(country.capital.as_deref(), true))
                .slot("phone", muted(country.phone.as_deref(), true))
                .slot("currency_code", muted(country.currency_code.as_deref(), false))
                .slot(
                    "population",
                    muted(Some(&format_integer(country.population)), true),
                )
                .slot(
                    "updated_at",
                    date_cell(
                        &shown.reader.display,
                        &format!("country-{id}-updated"),
                        country.updated_at,
                    )?,
                );
            rows.push(row);
        }
        let mut table = FlexTable::new("countries-table", self.columns.flex_columns(), rows)
            .sorted(self.list.sort_by.as_deref(), self.list.sort_dir);
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.framed = false;
        table.caption = "Countries";
        if shown.page.entries.is_empty() {
            table.empty = Some(TableEmpty::Content(Html::text("No countries found.")));
        }
        let mut body = render(table)?;
        body.push(&render(Pagination::new(
            "countries-pagination",
            PageCounts::new(
                self.list.page,
                self.list.page_size,
                shown.page.total_entries,
            ),
            &PAGE_SIZES,
        ))?);
        let card = render(Card {
            id: "countries-card",
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

impl LiveState<Scope, Error> for Countries {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            let mut requery = self.update.settle(&mut self.flash);
            match request.input {
                LiveInput::Refresh => requery = true,
                LiveInput::Params(values) => {
                    self.list = parse(values, &SPEC)?;
                    requery = true;
                }
                LiveInput::Event(event) => {
                    let get = |key: &str| event.get(key).and_then(Value::as_str);
                    match get("type") {
                        Some("filters") => {
                            apply_filters(
                                &mut self.list,
                                &params::form_values(event, FILTERS_FORM),
                            );
                            requery = true;
                        }
                        Some("sort") => {
                            if !self.list.next_sort(get("sort").unwrap_or_default()) {
                                return Err(Error::Invalid);
                            }
                            requery = true;
                        }
                        Some("grid") => match self.columns.handle(&grid_values(event)) {
                            ColumnsOutcome::Sort(key) => {
                                if !self.list.next_sort(&key) {
                                    return Err(Error::Invalid);
                                }
                                requery = true;
                            }
                            ColumnsOutcome::Update | ColumnsOutcome::Noop => {}
                        },
                        Some("page") => {
                            self.list.put_page(get("page"));
                            requery = true;
                        }
                        Some("update-countries") => {
                            self.update
                                .start(&self.pages, request.scope, &mut self.flash)
                                .await?
                        }
                        Some("save-country-name") => {
                            let value = get("value").ok_or(Error::Invalid)?;
                            if get("field") != Some("country") || value.len() > 4096 {
                                return Err(Error::Invalid);
                            }
                            self.save_name(request.scope, get("id"), value).await?;
                        }
                        Some("clear-flash") => self.flash.dismiss(get("key")),
                        _ => return Err(Error::Invalid),
                    }
                }
            }
            let html = self.view(request.scope, requery).await?;
            Ok(LiveBody {
                html,
                query: query(&self.list, BTreeMap::new()),
                ..LiveBody::default()
            })
        })
    }
    fn subscriptions(&mut self) -> Vec<Subscription> {
        vec![self.update.subscription()]
    }
}
