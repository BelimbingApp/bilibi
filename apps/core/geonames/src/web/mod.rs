//! The three index screens (Bilimbi `Core.Geonames.Web.{CountriesLive,
//! Admin1Live, PostcodesLive}`): bounded search, sort and page state kept in
//! the URL under Bilimbi's camelCase names (`CamelList`), built-in columns
//! through `ListColumns`/`FlexTable`, inline local-name corrections, the
//! operator postcode form, and the Countries and Admin1 Update (`update`).
//! `admin.geonames.list` is the route's gate; every write asks
//! `admin.geonames.update` again inside the API it calls, and a refusal is a
//! flash, never a page-clearing error.
mod admin1;
mod countries;
mod postcodes;
mod update;

use crate::{Geonames, ReferenceImport, UPDATE};
use askama::Template;
use bilibi_authz::effective_capabilities;
use bilibi_database::{Db, Error, Result};
use bilibi_datetime::{DisplayContext, DisplayPolicy, Format, Precision};
use bilibi_locale::Locales;
use bilibi_tenancy::Scope;
use bilibi_ui::{
    list_state::{InvalidPageSize, ParamNames},
    page::{LiveState, MountFuture, Page, PageBody, PageFuture, PageRequest},
    BuiltinColumn, Filter, FlashMap, Html, ListSpec, ListState, SortDir, StatusKind, NO_FLASH,
};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::SystemTime,
};

pub(crate) const PAGE_SIZES: [u64; 4] = crate::query::PAGE_SIZES;

#[derive(Clone)]
pub(crate) struct Pages {
    pub db: Db,
    pub geonames: Arc<Geonames>,
    pub importer: Arc<ReferenceImport>,
    pub display: Arc<DisplayPolicy>,
    pub locales: Arc<Locales>,
}

/// Bilimbi's `ListState.spec!` for these pages: `sortBy`, `sortDir` and
/// `perPage` in the URL, 25 rows by default, an invalid page size falling
/// back to the default. Every value is in the URL, as its `to_params/1` puts it.
pub(crate) fn spec(
    sortable: &[(&str, SortDir)],
    default_sort: &str,
    filters: &[(&str, Filter)],
) -> Arc<ListSpec> {
    ListSpec {
        sortable: sortable
            .iter()
            .map(|(key, dir)| ((*key).into(), *dir))
            .collect(),
        default_sort: Some(default_sort.into()),
        page_sizes: PAGE_SIZES.to_vec(),
        default_page_size: 25,
        page_size_param: "perPage".into(),
        invalid_page_size: InvalidPageSize::Default,
        filters: filters
            .iter()
            .map(|(key, filter)| ((*key).into(), filter.clone()))
            .collect(),
        param_names: ParamNames {
            search: "search".into(),
            page: "page".into(),
            sort_by: "sortBy".into(),
            sort_dir: "sortDir".into(),
        },
        ..ListSpec::default()
    }
    .shared()
    .expect("geonames list spec")
}

/// A forged page size snaps up to the next allowed size, which is what these
/// pages did before they used `ListState` (Bilimbi `CamelList`).
fn snap_page_size(values: &mut HashMap<String, String>) {
    if let Some(value) = values.get("perPage") {
        let wanted = bilibi_ui::params::positive_integer(Some(value)).unwrap_or(PAGE_SIZES[0]);
        let snapped = PAGE_SIZES
            .iter()
            .copied()
            .find(|size| *size >= wanted)
            .unwrap_or(PAGE_SIZES[PAGE_SIZES.len() - 1]);
        values.insert("perPage".into(), snapped.to_string());
    }
}
pub(crate) fn parse(query: &HashMap<String, String>, spec: &Arc<ListSpec>) -> Result<ListState> {
    let mut values = query.clone();
    snap_page_size(&mut values);
    ListState::parse(&values, spec.clone()).map_err(|_| Error::Internal)
}
pub(crate) fn apply_filters(state: &mut ListState, posted: &HashMap<String, String>) {
    let mut posted = posted.clone();
    snap_page_size(&mut posted);
    state.apply_filters(&posted);
}
/// The `IndexQuery` a list state asks the lookup API for.
pub(crate) fn index_query(state: &ListState) -> crate::IndexQuery {
    crate::IndexQuery {
        page: Some(state.page),
        page_size: Some(state.page_size),
        search: Some(state.search.clone()),
        sort_by: state.sort_by.clone(),
        sort_dir: state.sort_dir.map(|dir| dir.key().to_owned()),
        country_iso: None,
    }
}

pub(crate) fn render(fragment: impl Template) -> Result<Html> {
    Html::render(&fragment).map_err(|_| Error::Internal)
}
pub(crate) fn sortable(id: &str, label: &str, sort_id: &str) -> BuiltinColumn {
    let mut column = BuiltinColumn::new(id, label).sortable(id);
    column.sort_id = Some(sort_id.into());
    column
}
/// The flex table's operation values, as `ListColumns::handle` takes them.
pub(crate) fn grid_values(event: &Value) -> HashMap<String, String> {
    event
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_owned())))
        .collect()
}
/// Bilimbi `Geonames.Web.Components.format_integer/1`: thousands separated
/// by commas, no locale.
pub(crate) fn format_integer(value: i64) -> String {
    let digits = value.unsigned_abs().to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    if value < 0 {
        grouped.insert(0, '-');
    }
    grouped
}
pub(crate) fn muted(text: Option<&str>, tabular: bool) -> Html {
    Html::raw(format!(
        "<span class=\"whitespace-nowrap {}text-ink-muted\">{}</span>",
        if tabular { "tabular-nums " } else { "" },
        Html::text(text.unwrap_or("—"))
    ))
}
/// Bilimbi's country cell: the ISO code in mono beside the name, or the code
/// alone for a row whose country is not imported. The space between the two
/// spans is HEEx's own line break, which the browser renders; keep it, or
/// the column measures narrower than the source's.
pub(crate) fn country_cell(iso: &str, name: Option<&str>) -> Html {
    Html::raw(format!(
        "<div class=\"whitespace-nowrap text-ink-muted\"><span class=\"font-mono text-xs\">{}</span> <span class=\"ml-1\">{}</span></div>",
        Html::text(iso),
        Html::text(name.unwrap_or(iso))
    ))
}

/// Bilimbi `<.datetime format={:date}>`: both server-decidable texts travel
/// with the element so the shell's display-mode control swaps them.
#[derive(Template)]
#[template(
    source = r#"<span class="whitespace-nowrap text-xs tabular-nums text-ink-muted"><time id="{{ id }}" datetime="{{ iso }}" data-format="date" data-precision="minute" data-mode="{{ mode }}" data-text-company="{{ company }}" data-text-utc="{{ utc }}" data-follow-shell="true" data-hook="DateTime" class="tabular-nums">{{ text }}</time></span>"#,
    ext = "html"
)]
struct DateCell<'a> {
    id: &'a str,
    iso: String,
    text: String,
    company: String,
    utc: String,
    mode: &'static str,
}
pub(crate) fn date_cell(display: &DisplayContext, id: &str, at: SystemTime) -> Result<Html> {
    let instant = jiff::Timestamp::try_from(at).map_err(|_| Error::Internal)?;
    let rendered = display.render(instant, Format::Date, Precision::Minute);
    render(DateCell {
        id,
        iso: rendered.iso,
        text: rendered.text,
        company: rendered.company,
        utc: rendered.utc,
        mode: rendered.mode.as_str(),
    })
}

/// What every screen reads with its page: the reader's clock and whether the
/// update controls are drawn. The controls only show; each write asks again.
pub(crate) struct Reader {
    pub display: DisplayContext,
    pub can_update: bool,
}
pub(crate) async fn reader(pages: &Pages, scope: &Scope) -> Result<Reader> {
    let client = pages.db.get().await?;
    let locale = pages.locales.resolve(&**client, scope, None).await?.locale;
    let display = pages
        .display
        .display(&**client, scope, None, locale)
        .await?;
    let can_update = effective_capabilities(&**client, scope)
        .await?
        .contains(UPDATE);
    Ok(Reader {
        display,
        can_update,
    })
}

/// The page's one flash message, by kind (Bilimbi `put_flash`/`clear_flash`).
#[derive(Default)]
pub(crate) struct Flash(Option<(StatusKind, String)>);
impl Flash {
    pub fn put(&mut self, kind: StatusKind, message: impl Into<String>) {
        self.0 = Some((kind, message.into()));
    }
    pub fn clear(&mut self) {
        self.0 = None;
    }
    /// The browser dismissed the message of this kind.
    pub fn dismiss(&mut self, key: Option<&str>) {
        if self
            .0
            .as_ref()
            .is_some_and(|(kind, _)| Some(kind.name()) == key)
        {
            self.0 = None;
        }
    }
    pub fn map(&self) -> FlashMap<'_> {
        let mut flash = NO_FLASH;
        if let Some((kind, message)) = &self.0 {
            match kind {
                StatusKind::Success => flash.success = message,
                StatusKind::Error => flash.error = message,
                StatusKind::Warning => flash.warning = message,
                StatusKind::Info => flash.info = message,
            }
        }
        flash
    }
}

/// The URL these pages put back: the list state under its camelCase names,
/// with any extra keys a page adds (`filterCountryIso`, `summarySortBy`).
pub(crate) fn query(state: &ListState, extra: BTreeMap<String, String>) -> HashMap<String, String> {
    let mut params = state.to_params();
    params.extend(extra);
    params
}

impl Page<Scope, Error> for Pages {
    fn title(&self, operation: &str) -> Option<&'static str> {
        Some(match operation {
            "countries" => "Countries",
            "admin1" => "Admin1 Divisions",
            "postcodes" => "Geonames Postcodes",
            _ => return None,
        })
    }
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            let html = match request.operation {
                "countries" => {
                    countries::Countries::new(self.clone(), request.query)?
                        .view(scope, true)
                        .await?
                }
                "admin1" => {
                    admin1::Admin1::new(self.clone(), request.query)?
                        .view(scope, true)
                        .await?
                }
                "postcodes" => {
                    postcodes::Postcodes::new(self.clone(), request.query)?
                        .view(scope, true)
                        .await?
                }
                _ => return Err(Error::NotFound),
            };
            Ok(PageBody {
                html,
                live: true,
                title: None,
            })
        })
    }
    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            let state: Box<dyn LiveState<Scope, Error>> = match request.operation {
                "countries" => Box::new(countries::Countries::new(self.clone(), request.query)?),
                "admin1" => Box::new(admin1::Admin1::new(self.clone(), request.query)?),
                "postcodes" => Box::new(postcodes::Postcodes::new(self.clone(), request.query)?),
                _ => return Err(Error::NotFound),
            };
            Ok(Some(state))
        })
    }
}
