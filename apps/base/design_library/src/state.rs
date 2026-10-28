use bilibi_database::{Error, Result};
use bilibi_datetime::{filter, DisplayContext, Period, PeriodFilter};
use bilibi_ui::{
    flex_sample::FlexSample, BuiltinColumn, CommitStates, CommitStatus, ListColumns, ListSpec,
    ListState, LocationReport, SortDir, UploadSpec, Uploads,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, LazyLock},
};

static PATTERN_SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        page_size_param: "perPage".into(),
        ..ListSpec::default()
    }
    .shared()
    .expect("Design Library composite list spec")
});

static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        sortable: BTreeMap::from([
            ("name".into(), SortDir::Asc),
            ("code".into(), SortDir::Asc),
            ("status".into(), SortDir::Asc),
            ("updated_at".into(), SortDir::Desc),
        ]),
        default_sort: Some("name".into()),
        filters: [("status".into(), bilibi_ui::Filter::string(""))].into(),
        page_sizes: vec![25, 50, 100, 300],
        page_size_param: "perPage".into(),
        omit_blank: vec!["search".into()],
        ..ListSpec::default()
    }
    .shared()
    .expect("Design Library list spec")
});

pub struct State {
    pub area: String,
    pub csrf: String,
    pub sample: BTreeMap<String, Value>,
    pub list: ListState,
    pub pattern: ListState,
    pub sample_columns: ListColumns,
    pub pattern_columns: ListColumns,
    pub confirm_columns: ListColumns,
    pub flex: FlexSample,
    pub plain: FlexSample,
    pub display: DisplayContext,
    pub browser_zone: Option<String>,
    pub period: HashMap<String, String>,
    pub period_filters: Vec<PeriodFilter>,
    pub inline: BTreeMap<String, String>,
    pub statuses: CommitStates,
    pub version: u64,
    pub clicks: u64,
    pub modal: String,
    pub confirm_request: Option<String>,
    pub confirm_rows: Vec<&'static str>,
    pub outcome: String,
    pub outcome_kind: String,
    pub icon_search: String,
    pub icon_copy: Option<(String, bool)>,
    pub notice: String,
    pub redirect: Option<String>,
    pub submitted: bool,
    pub dismissed: Vec<String>,
    pub toolbar: HashMap<String, String>,
    /// The file input specimen's live uploads (Bilimbi `allow_upload`): up
    /// to two files of 256 KiB, any kind, so a larger file shows the refusal,
    /// and one resumable file as large as the installation allows.
    pub uploads: Uploads,
    /// The files the specimen's Keep event received, in this page's memory.
    pub kept: Vec<KeptFile>,
    /// The last report of the browser location specimen.
    pub location: Option<LocationReport>,
}
/// What the specimen records of a delivered file: enough to prove the
/// bytes arrived whole, never the bytes themselves.
pub struct KeptFile {
    pub name: String,
    pub content_type: String,
    pub size: usize,
    pub sha256: String,
}
/// The specimen's upload name and limit, shared by the state and the card.
const PERIOD_FILTERS: [&str; 2] = ["example-period-filter", "design-library-filter-period"];
pub const SAMPLE_UPLOAD: &str = "sample_file";
pub const SAMPLE_UPLOAD_BYTES: u64 = 256 * 1024;
/// The resumable specimen: one file, staged on Media's disk while it arrives.
pub const LARGE_UPLOAD: &str = "large_file";
impl State {
    pub fn new(area: &str, query: &HashMap<String, String>) -> Result<Self> {
        let sample =
            serde_json::from_str(include_str!("sample.json")).expect("copied specimen values");
        let list = ListState::parse(query, SPEC.clone()).map_err(|_| Error::Invalid)?;
        let clock = DisplayContext::anonymous().clock(None, jiff::Timestamp::now());
        let period = Period::parse(|key| query.get(key).cloned(), clock.today);
        let mut result = Self {
            area: area.into(),
            csrf: String::new(),
            sample,
            list,
            pattern: ListState::parse(&HashMap::new(), PATTERN_SPEC.clone())
                .map_err(|_| Error::Invalid)?,
            flex: FlexSample::default(),
            // Mirrors Bilimbi UI DesignLibraryLive; live lists use independent hosts.
            sample_columns: ListColumns::mount(
                "sample-table",
                vec![
                    BuiltinColumn::new("name", "Company").sortable("name"),
                    BuiltinColumn::new("code", "Code").sortable("code"),
                    BuiltinColumn::new("status", "Status").sortable("status"),
                    BuiltinColumn::new("updated_at", "Updated").sortable("updated_at"),
                ],
            )
            .expect("sample columns"),
            pattern_columns: ListColumns::mount(
                "design-library-pattern-table",
                vec![
                    BuiltinColumn::new("name", "Company"),
                    BuiltinColumn::new("code", "Code"),
                    BuiltinColumn::new("status", "Status"),
                    BuiltinColumn::new("updated_at", "Updated"),
                ],
            )
            .expect("pattern columns"),
            confirm_columns: ListColumns::mount(
                "design-library-confirm-rows",
                vec![BuiltinColumn::new("name", "Name")],
            )
            .expect("confirmation columns"),
            plain: FlexSample::default(),
            display: DisplayContext::anonymous(),
            browser_zone: None,
            period: HashMap::new(),
            period_filters: PERIOD_FILTERS
                .iter()
                .map(|id| PeriodFilter::new(*id, period, &clock))
                .collect(),
            inline: BTreeMap::new(),
            statuses: CommitStates::default(),
            version: 0,
            clicks: 0,
            modal: String::new(),
            confirm_request: None,
            confirm_rows: vec!["sole", "llc"],
            outcome: String::new(),
            outcome_kind: String::new(),
            icon_search: String::new(),
            icon_copy: None,
            notice: String::new(),
            redirect: None,
            submitted: query.get("specimen").is_some_and(|v| v == "requested"),
            dismissed: Vec::new(),
            toolbar: HashMap::new(),
            uploads: Uploads::new([
                UploadSpec::new(SAMPLE_UPLOAD, SAMPLE_UPLOAD_BYTES).max_entries(2),
                UploadSpec::resumable(LARGE_UPLOAD, bilibi_ui::upload::MAX_RESUMABLE_FILE_SIZE),
            ]),
            kept: Vec::new(),
            location: None,
        };
        result.params(query)?;
        Ok(result)
    }
    /// Takes one input's completed files and keeps their facts only.
    fn keep(&mut self, upload: &str) {
        let files = self.uploads.consume(upload);
        if files.is_empty() {
            self.notice = "Choose a file first.".into();
            return;
        }
        self.notice = format!(
            "{} file{} kept in this page's memory.",
            files.len(),
            if files.len() == 1 { "" } else { "s" }
        );
        for file in files {
            let digest = Sha256::digest(&file.bytes);
            self.kept.push(KeptFile {
                name: file.name,
                content_type: file.content_type,
                size: file.bytes.len(),
                sha256: digest.iter().map(|byte| format!("{byte:02x}")).collect(),
            });
        }
        self.kept.truncate(20);
    }
    pub fn params(&mut self, query: &HashMap<String, String>) -> Result<()> {
        self.list = ListState::parse(query, SPEC.clone()).map_err(|_| Error::Invalid)?;
        self.period = query
            .iter()
            .filter(|(k, _)| ["period", "from", "to"].contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        Ok(())
    }
    pub fn query(&self) -> HashMap<String, String> {
        let mut q = if self.area == "components" {
            self.list.to_params()
        } else {
            HashMap::new()
        };
        q.extend(self.period.clone());
        if self.submitted {
            q.insert("specimen".into(), "requested".into());
        }
        q
    }
    pub fn event(&mut self, event: &Value) -> Result<()> {
        let get = |key: &str| {
            event.get(key).and_then(|v| match v {
                Value::String(s) => Some(s.clone()),
                Value::Number(n) => Some(n.to_string()),
                _ => None,
            })
        };
        let ty = get("type").unwrap_or_default();
        match ty.as_str() {
            "sample-columns" => {
                self.sample_columns.handle(
                    &bilibi_ui::params::string_values_checked(
                        event,
                        bilibi_ui::params::FIELD_LIMIT,
                    )
                    .map_err(Error::refused)?,
                );
            }
            "pattern-columns" => {
                self.pattern_columns.handle(
                    &bilibi_ui::params::string_values_checked(
                        event,
                        bilibi_ui::params::FIELD_LIMIT,
                    )
                    .map_err(Error::refused)?,
                );
            }
            "confirm-columns" => {
                self.confirm_columns.handle(
                    &bilibi_ui::params::string_values_checked(
                        event,
                        bilibi_ui::params::FIELD_LIMIT,
                    )
                    .map_err(Error::refused)?,
                );
            }
            "sample-flex" => self.flex.apply(&get),
            "sample-flex-plain" => self.plain.apply(&get),
            // The source preview has no workspace: selection counts as a click.
            "test_click" | "toggle_split" | "close_tile" | "workspace:select" => {
                self.clicks += 1;
            }
            "open-modal" => {
                self.modal = if get("width").as_deref() == Some("wide") {
                    "wide"
                } else {
                    "narrow"
                }
                .into()
            }
            "close-modal" => self.modal.clear(),
            "confirm-request" => {
                self.confirm_request =
                    get("id").filter(|id| self.confirm_rows.contains(&id.as_str()));
            }
            "confirm-cancel" => self.confirm_request = None,
            "confirm-run" => {
                if let Some(id) = self.confirm_request.take() {
                    if id == "sole" {
                        self.confirm_rows.retain(|id| *id != "sole");
                        self.outcome = "Legal entity type deleted.".into();
                        self.outcome_kind = "success".into();
                    } else {
                        self.outcome="Limited Liability Company was not deleted: 3 companies still use it. Change those companies' legal entity type first.".into();
                        self.outcome_kind = "error".into();
                    }
                }
            }
            "confirm-reset" => {
                self.confirm_rows = vec!["sole", "llc"];
                self.outcome.clear();
                self.outcome_kind.clear();
                self.confirm_request = None;
            }
            "sample-sort" => {
                if let Some(key) = get("sort") {
                    self.list.next_sort(&key);
                }
            }
            "sample-search" => {
                let mut values = bilibi_ui::params::form_values_checked(
                    event,
                    "filters",
                    bilibi_ui::params::FIELD_LIMIT,
                )
                .map_err(Error::refused)?;
                if let Some(search) = get("sample_filters[search]") {
                    values.insert("search".into(), search);
                }
                self.list.apply_filters(&values);
            }
            "sample-clear-search" => self
                .list
                .apply_filters(&HashMap::from([("search".into(), String::new())])),
            "sample-page" => self.list.put_page(get("page").as_deref()),
            "sample-page-size" => self.list.apply_filters(
                &bilibi_ui::params::form_values_checked(
                    event,
                    "filters",
                    bilibi_ui::params::FIELD_LIMIT,
                )
                .map_err(Error::refused)?,
            ),
            "pattern-search" => self.pattern.apply_filters(
                &bilibi_ui::params::form_values_checked(
                    event,
                    "pattern",
                    bilibi_ui::params::FIELD_LIMIT,
                )
                .map_err(Error::refused)?,
            ),
            "pattern-page" => self.pattern.put_page(get("page").as_deref()),
            "pattern-page-size" => self.pattern.apply_filters(
                &bilibi_ui::params::form_values_checked(
                    event,
                    "filters",
                    bilibi_ui::params::FIELD_LIMIT,
                )
                .map_err(Error::refused)?,
            ),
            "sample_change" => {
                if let Some(object) = event.as_object() {
                    // Bound values only from the copied sample form; secrets never enter notices.
                    for (key, value) in object.iter().take(96) {
                        if let Some(key) = key
                            .strip_prefix("sample[")
                            .and_then(|k| k.strip_suffix("][]").or_else(|| k.strip_suffix(']')))
                        {
                            if self.sample.contains_key(key) && value.to_string().len() <= 4096 {
                                self.sample.insert(key.into(), value.clone());
                            }
                        }
                    }
                }
                if event.get("sample[checkbox_field]").is_none() {
                    self.sample
                        .insert("checkbox_field".into(), Value::String("false".into()));
                }
            }
            "sample_chip_remove" => {
                if let (Some(name), Some(value)) =
                    (get("name"), get("option").or_else(|| get("value")))
                {
                    let key = name
                        .strip_prefix("sample[")
                        .and_then(|k| k.strip_suffix("][]"))
                        .unwrap_or(&name);
                    if let Some(Value::Array(values)) = self.sample.get_mut(key) {
                        values.retain(|v| v.as_str() != Some(&value));
                    }
                }
            }
            "preview-inline-edit" | "preview-long-text" | "save_field" => {
                let id = get("field").or_else(|| get("id")).unwrap_or_default();
                if !crate::controls::inline_id(&id) {
                    return Ok(());
                }
                let value = get("value").unwrap_or_default();
                let version = get("version")
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(0);
                let status = if version != self.version {
                    CommitStatus::refusal(
                        "Value",
                        &value,
                        &["changed; reload the latest value before saving"],
                    )
                } else if value.len() > 2000 {
                    CommitStatus::refusal("Value", &value, &["is too long"])
                } else if value == "Pending" {
                    CommitStatus::Error("Pending is not allowed.".into())
                } else {
                    self.inline.insert(id.clone(), value);
                    self.version += 1;
                    self.notice = "Preview value updated.".into();
                    CommitStatus::Saved
                };
                self.statuses.put(id, status);
            }
            "preview-combobox-cancel" => self.notice = "Inline edit cancelled.".into(),
            "edit_field" | "cancel_edit_field" => {}
            "preview-company" => {
                if let Some(id) = get("id")
                    .and_then(|v| v.parse::<u64>().ok())
                    .filter(|id| (1..=120).contains(id))
                {
                    let row = Company::new(id);
                    self.notice =
                        format!("Example only: {} · {} · {}", row.name, row.code, row.status);
                }
            }
            "icon-filter" => {
                self.icon_search = get("filters[search]")
                    .or_else(|| get("icon_filters[search]"))
                    .unwrap_or_default()
                    .chars()
                    .take(200)
                    .collect();
            }
            "icon-clear-filter" => self.icon_search.clear(),
            "icon-copied" => {
                if let Some(name) =
                    get("text").filter(|name| crate::view::icon_names().any(|v| v == name))
                {
                    if let Some(copied) = event.get("copied").and_then(Value::as_bool) {
                        self.icon_copy = Some((name, copied));
                    }
                }
            }
            "browser_timezone" => {
                self.browser_zone = get("timezone").filter(|v| bilibi_datetime::valid_timezone(v));
            }
            "filter-toolbar-preview" | "filter-toolbar-search-preview" => {
                self.toolbar = bilibi_ui::params::form_values_checked(
                    event,
                    "filters",
                    bilibi_ui::params::FIELD_LIMIT,
                )
                .map_err(Error::refused)?
            }
            // Bilimbi's `consume_uploaded_entries` in the submit handler: the
            // completed files are taken once, here, and only their facts stay.
            "keep-files" => self.keep(SAMPLE_UPLOAD),
            "keep-large-file" => self.keep(LARGE_UPLOAD),
            "forget-files" => {
                self.kept.clear();
                self.uploads.clear(SAMPLE_UPLOAD);
                self.uploads.clear(LARGE_UPLOAD);
            }
            // The location action's one event: a position or an outcome the
            // card answers in words; a payload that is neither is invalid.
            "capture-location" => {
                self.location = Some(LocationReport::from_event(event).ok_or(Error::Invalid)?);
            }
            "clear-flash" => {
                self.submitted = false;
                self.notice.clear();
            }
            "dismiss-notice" => {
                if let Some(id) = get("id") {
                    self.dismissed.push(id);
                }
            }
            _ => {
                let clock = self
                    .display
                    .clock(self.browser_zone.as_deref(), jiff::Timestamp::now());
                let period = Period::parse(|key| self.period.get(key).cloned(), clock.today);
                let mut outcome = None;
                for filter in &mut self.period_filters {
                    filter.set(period, &clock);
                    outcome = filter
                        .event_checked(&ty, event, &clock)
                        .map_err(Error::refused)?;
                    if outcome.is_some() {
                        break;
                    }
                }
                if let Some(filter::Outcome::Applied(period)) = outcome {
                    self.period = period
                        .params()
                        .into_iter()
                        .map(|(k, v)| (k.into(), v))
                        .collect();
                    self.list.page = 1;
                }
            }
        }
        let count = companies(&self.list).len() as u64;
        self.list
            .clamp_to_last_page(count.div_ceil(self.list.page_size), true);
        let count = companies(&self.pattern).len() as u64;
        self.pattern
            .clamp_to_last_page(count.div_ceil(self.pattern.page_size), true);
        Ok(())
    }
}
#[derive(Clone)]
pub struct Company {
    pub id: u64,
    pub name: String,
    pub code: String,
    pub status: &'static str,
    pub updated: jiff::Timestamp,
}
impl Company {
    pub fn new(id: u64) -> Self {
        let (name, code, status, at) = match id {
            1 => (
                "Acme Holdings".into(),
                "acme".into(),
                "active",
                "2026-08-17T12:00:00Z",
            ),
            2 => (
                "Globex Corporation".into(),
                "globex".into(),
                "pending",
                "2026-08-16T15:30:00Z",
            ),
            3 => (
                "Initech LLC".into(),
                "initech".into(),
                "suspended",
                "2026-08-15T09:15:00Z",
            ),
            _ => (
                format!("Example Company {id}"),
                format!("example-{id}"),
                "active",
                "2026-08-14T12:00:00Z",
            ),
        };
        let mut updated = at.parse::<jiff::Timestamp>().expect("sample timestamp");
        if id >= 4 {
            updated = updated
                .checked_sub(std::time::Duration::from_secs((id - 4) * 86400))
                .expect("sample range");
        }
        Self {
            id,
            name,
            code,
            status,
            updated,
        }
    }
}
pub fn companies(list: &ListState) -> Vec<Company> {
    let search = list.search.trim().to_lowercase();
    let mut rows: Vec<_> = (1..=120)
        .map(Company::new)
        .filter(|row| {
            list.filters
                .get("status")
                .is_none_or(|status| status.is_empty() || row.status == status)
        })
        .filter(|row| {
            format!("{} {}", row.name, row.code)
                .to_lowercase()
                .contains(&search)
        })
        .collect();
    if list.sort_by.is_none() {
        return rows;
    }
    rows.sort_by(|a, b| {
        let order = match list.sort_by.as_deref() {
            Some("code") => a.code.cmp(&b.code),
            Some("status") => a.status.cmp(b.status),
            Some("updated_at") => a.updated.cmp(&b.updated),
            _ => a.name.cmp(&b.name),
        }
        .then(a.id.cmp(&b.id));
        if list.sort_dir == Some(SortDir::Desc) {
            order.reverse()
        } else {
            order
        }
    });
    rows
}
