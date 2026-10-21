//! `/geonames/postcodes` (Bilimbi `PostcodesLive`; the Import and Update
//! are Belimbing `Geonames\Livewire\Postcodes\Index::{import, update}`,
//! absent from Bilimbi): the per-country totals, the searchable Postcodes
//! index with an inline place-name correction per row, the operator's
//! New/Edit Postcode dialog, and the GeoNames import. Import opens the
//! country picker (`geonames_postcode_picker.html`) and then runs the ticked
//! countries' postcodes through the shared background `Run`, one country at
//! a time, so the page says which one is running and shows the rows so far;
//! Update runs every imported country again. Corrections go through
//! `Geonames::{create_postcode, update_postcode}`, which keep the sidecar,
//! the compatible row and the audit trail together and refuse a stale
//! revision; the index row carries that revision in the fact's `data-id`
//! (`{id}|{revision}`, Bilimbi's `id_value`), so a redrawn row commits with
//! the current one.
use super::{
    update::{self, Outcome, Run, Started},
    *,
};
use crate::{
    DownloadStatus, FieldError, ImportError, PostcodeCountrySummary, PostcodeDraft, PostcodeIndex,
    PostcodeWriteError, Provenance, Target,
};
use bilibi_ui::{
    field_base_class,
    list_state::FILTERS_FORM,
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, Subscription},
    params, Alert, ButtonVariant, Card, Choice, ColumnType, ColumnsOutcome, CommitStates,
    CommitStatus, CreateButton, Field, FilterToolbar, FlexRow, FlexTable, Header, IconButton,
    IconContext, InlineEdit, InlineField, InputKind, ListColumns, Modal, ModalWidth, PageCounts,
    Pagination, RowMode, TableEmpty, ToolbarControl,
};
use std::{collections::BTreeSet, sync::LazyLock};

const DENIED: &str = "You do not have permission to update postcodes.";
const IMPORT_DENIED: &str = "You do not have permission to import postcodes.";
const NONE_SELECTED: &str = "Select at least one country to import.";
/// The picker's checkboxes post `country[MY]=true|false`; the search box
/// posts `country_search`.
const PICKER_FORM: &str = "country";
const PICKER_SEARCH: &str = "country_search";
/// A flash lists at most this many countries by name.
const NAMED_COUNTRIES: usize = 6;
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
// Bilimbi PostcodesLive's edit dialog and picker share their field labels.
bilibi_ui::record_fields! { mod fields {
    COUNTRY: bilibi_ui::RecordField::new("country_iso", "Country").slot("country_name"),
    POSTCODE: bilibi_ui::RecordField::new("postcode", "Postcode"),
    PLACE_NAME: bilibi_ui::RecordField::new("place_name", "Place Name"),
    ADMIN1: bilibi_ui::RecordField::new("admin1_division", "Admin1 Division"),
    LATITUDE: bilibi_ui::RecordField::new("latitude", "Latitude"),
    LONGITUDE: bilibi_ui::RecordField::new("longitude", "Longitude"),
    ACCURACY: bilibi_ui::RecordField::new("accuracy", "Accuracy"),
} }

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
    picker: Html,
    progress: Html,
    summary: Html,
    toolbar: Html,
    card: Html,
    modal: Html,
}
#[derive(Template)]
#[template(path = "geonames_postcode_picker.html")]
struct PickerView {
    close: Html,
    search: Field,
    entries: Vec<PickerEntry>,
}
/// One country in the picker: ticked through `field`, or marked as already
/// imported.
struct PickerEntry {
    iso: String,
    name: String,
    imported: bool,
    field: Field,
}
/// The open country picker: what the search box holds and which countries
/// are ticked, also the ones the search hides right now.
struct Picker {
    /// Every country as `(name, iso)`, by name.
    countries: Vec<(String, String)>,
    search: String,
    selected: BTreeSet<String>,
}
/// Which control started the run; it words the progress and the result.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RunKind {
    Import,
    Update,
}
impl RunKind {
    fn verb(self) -> &'static str {
        match self {
            Self::Import => "imported",
            Self::Update => "updated",
        }
    }
    fn gerund(self) -> &'static str {
        match self {
            Self::Import => "Importing",
            Self::Update => "Updating",
        }
    }
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
    picker: Option<Picker>,
    run: Run,
    run_kind: RunKind,
}

impl Postcodes {
    pub fn new(pages: Pages, query: &HashMap<String, String>) -> Result<Self> {
        Ok(Self {
            pages,
            list: parse(query, &SPEC)?,
            summary: parse_summary(query)?,
            columns: ListColumns::mount("postcodes-table", builtins())
                .map(|columns| columns.with_record_fields(fields::ALL.iter().copied()))
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
            picker: None,
            run: Run::new(),
            run_kind: RunKind::Import,
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

    fn imported_isos(&self) -> BTreeSet<String> {
        self.shown
            .iter()
            .flat_map(|shown| shown.summaries.iter())
            .map(|summary| summary.country_iso.clone())
            .collect()
    }

    /// "Malaysia (MY)" for a message, or the code alone for a country that
    /// is not imported.
    fn country_label(&self, iso: &str) -> String {
        self.shown
            .iter()
            .flat_map(|shown| shown.countries.iter())
            .find(|choice| choice.value == iso)
            .map(|choice| choice.label.clone())
            .unwrap_or_else(|| iso.to_owned())
    }

    /// Import pressed with the picker closed: open it (Belimbing
    /// `toggleCountryPicker`). The control only shows for an operator; this
    /// asks the flag the last load read, and the import itself asks the
    /// database again.
    async fn open_picker(&mut self) -> Result<()> {
        let can_update = self
            .shown
            .as_ref()
            .is_some_and(|shown| shown.reader.can_update);
        if !can_update {
            self.flash.put(StatusKind::Error, IMPORT_DENIED);
            return Ok(());
        }
        if self.picker.is_some() {
            return Ok(());
        }
        let countries = self
            .pages
            .geonames
            .list_countries()
            .await?
            .into_iter()
            .map(|country| (country.country, country.iso))
            .collect();
        self.flash.clear();
        self.picker = Some(Picker {
            countries,
            search: String::new(),
            selected: BTreeSet::new(),
        });
        Ok(())
    }

    /// The picker form changed: the search text, and every checkbox the
    /// search showed as `true`/`false`. A country the search hid posted
    /// nothing and keeps its tick.
    fn pick(&mut self, event: &Value) {
        let Some(picker) = &mut self.picker else {
            return;
        };
        if let Some(search) = event.get(PICKER_SEARCH).and_then(Value::as_str) {
            picker.search = search.chars().take(100).collect();
        }
        // Read every `country[XX]` key here: `params::form_values` keeps
        // 64 fields, and an unfiltered grid posts one per country.
        let keys = event.as_object().into_iter().flatten();
        for (key, value) in keys {
            let Some(iso) = key
                .strip_prefix(PICKER_FORM)
                .and_then(|key| key.strip_prefix('['))
                .and_then(|key| key.strip_suffix(']'))
                .and_then(crate::normalize_iso)
            else {
                continue;
            };
            if value.as_str() == Some("true") {
                picker.selected.insert(iso);
            } else {
                picker.selected.remove(&iso);
            }
        }
    }

    /// Import pressed with the picker open: run the ticked countries, in
    /// ISO order, and close the picker (Belimbing `import`). A country
    /// imported since the picker opened is skipped: Update owns it.
    async fn start_import(&mut self, scope: &Scope) -> Result<()> {
        let imported = self.imported_isos();
        let targets: Vec<Target> = self
            .picker
            .as_ref()
            .map(|picker| {
                picker
                    .selected
                    .iter()
                    .filter(|iso| !imported.contains(*iso))
                    .cloned()
                    .map(Target::Postcodes)
                    .collect()
            })
            .unwrap_or_default();
        if targets.is_empty() {
            self.flash.put(StatusKind::Error, NONE_SELECTED);
            return Ok(());
        }
        self.start(scope, RunKind::Import, targets, IMPORT_DENIED)
            .await
    }

    /// Update pressed: run every imported country again (Belimbing `update`).
    async fn start_update(&mut self, scope: &Scope) -> Result<()> {
        let targets: Vec<Target> = self
            .imported_isos()
            .into_iter()
            .map(Target::Postcodes)
            .collect();
        if targets.is_empty() {
            return Ok(());
        }
        self.start(scope, RunKind::Update, targets, DENIED).await
    }

    async fn start(
        &mut self,
        scope: &Scope,
        kind: RunKind,
        targets: Vec<Target>,
        denied: &'static str,
    ) -> Result<()> {
        match self.run.start(&self.pages, scope, targets).await? {
            Started::Ok => {
                self.run_kind = kind;
                self.picker = None;
                self.flash.clear();
            }
            Started::Busy => {}
            Started::Denied => {
                self.write_forbidden();
                self.flash.put(StatusKind::Error, denied);
            }
        }
        Ok(())
    }

    /// The finished run as one flash: what was imported, what was kept as it
    /// was because the download failed, and what was not imported and why.
    fn finish(&mut self, outcomes: Vec<Outcome>) {
        if outcomes
            .iter()
            .any(|outcome| matches!(outcome.result, Err(ImportError::Access(Error::Forbidden))))
        {
            self.write_forbidden();
            self.flash.put(StatusKind::Error, IMPORT_DENIED);
            return;
        }
        let verb = self.run_kind.verb();
        let total = outcomes.len();
        let mut done: Vec<String> = Vec::new();
        let mut rows = 0;
        let mut kept: Vec<String> = Vec::new();
        let mut cached: Vec<String> = Vec::new();
        let mut failed: Vec<String> = Vec::new();
        for outcome in &outcomes {
            let Target::Postcodes(iso) = &outcome.target else {
                continue;
            };
            let label = self.country_label(iso);
            match &outcome.result {
                Ok(report) => match &report.download {
                    DownloadStatus::Fallback(cause) if self.run_kind == RunKind::Update => kept
                        .push(format!(
                            "{label} ({}{}; kept the existing {} rows)",
                            update::fallback_cause(cause),
                            update::as_of(report.cached_at),
                            format_integer(report.imported as i64)
                        )),
                    DownloadStatus::Fallback(cause) => {
                        rows += report.imported;
                        done.push(format!(
                            "{label}: {} rows",
                            format_integer(report.imported as i64)
                        ));
                        cached.push(format!(
                            "{label} ({}{})",
                            update::fallback_cause(cause),
                            update::as_of(report.cached_at)
                        ));
                    }
                    _ => {
                        rows += report.imported;
                        done.push(format!(
                            "{label}: {} rows",
                            format_integer(report.imported as i64)
                        ));
                    }
                },
                Err(error) => {
                    failed.push(format!("{label}: {}", update::not_changed_advice(error)))
                }
            }
        }
        let mut sentences = Vec::new();
        match done.len() {
            0 => {}
            1 => sentences.push(format!("Postcodes {verb} for {}.", done[0])),
            n if n == total => sentences.push(format!(
                "Postcodes {verb} for {n} countries, {} rows in all: {}.",
                format_integer(rows as i64),
                named(&done)
            )),
            n => sentences.push(format!(
                "Postcodes {verb} for {n} of {total} countries, {} rows in all: {}.",
                format_integer(rows as i64),
                named(&done)
            )),
        }
        if !cached.is_empty() {
            sentences.push(format!(
                "Imported from the local GeoNames download, because the download failed: {}. Try Update later for current data.",
                named(&cached)
            ));
        }
        if !kept.is_empty() {
            sentences.push(format!(
                "Not {verb}, because the download failed: {}. Try Update again later.",
                named(&kept)
            ));
        }
        if !failed.is_empty() {
            sentences.push(format!("Not {verb}: {}", failed.join(" ")));
        }
        let kind = if done.len() == total && cached.is_empty() {
            StatusKind::Success
        } else if done.is_empty() && kept.is_empty() {
            StatusKind::Error
        } else {
            StatusKind::Warning
        };
        self.flash.put(kind, sentences.join(" "));
    }

    /// The Import and Update header controls (Belimbing's `actions` slot):
    /// Import opens the picker, or runs the ticked countries once it is open.
    fn import_buttons(&self) -> Result<Html> {
        let (event, label) = match &self.picker {
            Some(picker) if !picker.selected.is_empty() => (
                "import-postcodes",
                format!("Import ({})", picker.selected.len()),
            ),
            Some(_) => ("import-postcodes", "Import".to_owned()),
            None => ("open-country-picker", "Import".to_owned()),
        };
        let importing = self.run.busy() && self.run_kind == RunKind::Import;
        let mut html = Html::raw("<div class=\"flex flex-wrap items-center gap-2\">");
        html.push(&self.run.button(
            "postcodes-import",
            event,
            "hero-arrow-down-tray",
            ButtonVariant::Primary,
            &label,
            if importing { "Importing…" } else { &label },
        )?);
        if !self.imported_isos().is_empty() {
            let updating = self.run.busy() && self.run_kind == RunKind::Update;
            html.push(&self.run.button(
                "postcodes-update",
                "update-postcodes",
                "refresh",
                ButtonVariant::Secondary,
                "Update",
                if updating { "Updating…" } else { "Update" },
            )?);
        }
        Ok(html)
    }

    fn picker_view(&self) -> Result<Html> {
        let Some(picker) = &self.picker else {
            return Ok(Html::EMPTY);
        };
        let imported = self.imported_isos();
        let needle = picker.search.trim().to_lowercase();
        let entries = picker
            .countries
            .iter()
            .filter(|(name, iso)| {
                needle.is_empty()
                    || name.to_lowercase().contains(&needle)
                    || iso.to_lowercase().contains(&needle)
            })
            .map(|(name, iso)| PickerEntry {
                iso: iso.clone(),
                name: name.clone(),
                imported: imported.contains(iso),
                field: Field::named(format!("{PICKER_FORM}[{iso}]"))
                    .id(format!("pick-{iso}"))
                    .kind(InputKind::Checkbox)
                    .checked(picker.selected.contains(iso))
                    .label(name.clone())
                    .wrapper_class("min-w-0")
                    .label_class("min-w-0 gap-2"),
            })
            .collect();
        let mut search = Field::named(PICKER_SEARCH)
            .id("country-picker-search")
            .kind(InputKind::Search)
            .wrapper_class("mb-0")
            .value(picker.search.clone())
            .placeholder("Search countries...")
            .autocomplete("off")
            .attr("data-debounce", "300")
            .class(field_base_class(false, "pl-8 pr-3"));
        search.attrs.maxlength = Some("100".into());
        let body = render(PickerView {
            close: render(IconButton {
                context: IconContext::Inline,
                id: "country-picker-close",
                event: "close-country-picker",
                ..IconButton::new("close", "Close country picker")
            })?,
            search,
            entries,
        })?;
        render(Card {
            id: "postcodes-country-picker",
            role: "region",
            labelledby: "country-picker-title",
            body,
            ..Card::default()
        })
    }

    /// Where the run is, while it goes (Belimbing's "Importing..." notice).
    fn progress_view(&self) -> Result<Html> {
        let Some(progress) = self.run.progress() else {
            return Ok(Html::EMPTY);
        };
        let current = progress.current.as_ref().and_then(|target| match target {
            Target::Postcodes(iso) => Some(self.country_label(iso)),
            Target::Dataset(_) => None,
        });
        let gerund = self.run_kind.gerund();
        let text = match (progress.total, current) {
            (1, Some(country)) => format!("{gerund} postcodes for {country}."),
            (total, Some(country)) => format!(
                "{gerund} postcodes for {total} countries: {} done, now {country}.",
                progress.done
            ),
            (total, None) => format!(
                "{gerund} postcodes for {total} countries: {} done.",
                progress.done
            ),
        };
        render(Alert {
            kind: StatusKind::Info,
            id: "postcodes-import-progress",
            body: Html::raw(format!(
                "<p class=\"font-medium\">{}</p><p class=\"mt-0.5 text-xs opacity-75\">This may take several minutes. Keep this page open to see the result.</p>",
                Html::text(&text)
            )),
            ..Alert::default()
        })
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
            .label(fields::get("postcode").label)
            .required();
        postcode.attrs.maxlength = Some("20".into());
        let mut place_name = field("place_name")
            .id("postcode-place-name")
            .label(fields::PLACE_NAME.label)
            .required();
        place_name.attrs.maxlength = Some("180".into());
        let mut latitude = field("latitude")
            .id("postcode-latitude")
            .kind(InputKind::Number)
            .label(fields::LATITUDE.label);
        latitude.attrs.step = Some("0.0000001".into());
        let mut longitude = field("longitude")
            .id("postcode-longitude")
            .kind(InputKind::Number)
            .label(fields::LONGITUDE.label);
        longitude.attrs.step = Some("0.0000001".into());
        render(FormView {
            country: field("country_iso")
                .id("postcode-country")
                .kind(InputKind::Select)
                .label(fields::COUNTRY.label)
                .prompt("Select country...")
                .options(countries.to_vec())
                .required(),
            postcode,
            place_name,
            admin1: field("admin1_code")
                .id("postcode-admin1")
                .kind(InputKind::Select)
                .label(fields::ADMIN1.label)
                .prompt("None")
                .options(draft.admin1_options.clone()),
            latitude,
            longitude,
            accuracy: field("accuracy")
                .id("postcode-accuracy")
                .kind(InputKind::Select)
                .label(fields::ACCURACY.label)
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
                let mut actions = self.import_buttons()?;
                actions.push(&render(CreateButton {
                    noun: "Postcode",
                    id: "postcodes-new",
                    event: "new-postcode",
                    button_type: "button",
                    ..CreateButton::default()
                })?);
                actions.push(&Html::raw("</div>"));
                actions
            } else {
                Html::EMPTY
            },
            ..Header::default()
        })?;
        let picker = if can_update {
            self.picker_view()?
        } else {
            Html::EMPTY
        };
        let progress = self.progress_view()?;

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
                .slot("admin1_division", muted(postcode.admin1_name.as_deref().or(postcode.admin1_code.as_deref()), false))
                .slot("latitude", Html::text(&postcode.latitude.map(|value| value.to_string()).unwrap_or_else(|| "—".into())))
                .slot("longitude", Html::text(&postcode.longitude.map(|value| value.to_string()).unwrap_or_else(|| "—".into())))
                .slot("accuracy", Html::text(&postcode.accuracy.map(|value| value.to_string()).unwrap_or_else(|| "—".into())))
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
            picker,
            progress,
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
            // The background import wakes the page after each country: its
            // rows are there, so this pass reads them; the finished run
            // becomes the flash.
            let settled = self.run.settle();
            let changed = settled.new_outcomes > 0 || settled.finished.is_some();
            if let Some(outcomes) = settled.finished {
                self.finish(outcomes);
            }
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
                        Some("open-country-picker") => {
                            self.open_picker().await?;
                            requery = false;
                        }
                        Some("close-country-picker") => {
                            self.picker = None;
                            requery = false;
                        }
                        Some("pick-countries") => {
                            self.pick(event);
                            requery = false;
                        }
                        Some("import-postcodes") => {
                            self.start_import(request.scope).await?;
                            requery = false;
                        }
                        Some("update-postcodes") => {
                            self.start_update(request.scope).await?;
                            requery = false;
                        }
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
            let html = self.view(request.scope, requery || changed).await?;
            Ok(LiveBody {
                html,
                query: postcodes_query(&self.list, &self.summary),
                ..LiveBody::default()
            })
        })
    }
    fn subscriptions(&mut self) -> Vec<Subscription> {
        vec![self.run.subscription()]
    }
}

/// "A, B and C", or the first few "and 4 more".
fn named(items: &[String]) -> String {
    if items.len() > NAMED_COUNTRIES {
        format!(
            "{} and {} more",
            items[..NAMED_COUNTRIES].join(", "),
            items.len() - NAMED_COUNTRIES
        )
    } else {
        items.join(", ")
    }
}
